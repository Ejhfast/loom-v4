//! The graph modes.
//!
//! Each mode is a visitor plus its own result state over the one
//! traversal engine in `engine`. Mark, deep freeze, frozen
//! verification, boundary transfer, structural copy, canonical
//! digest, detached inspection, and snapshot traversal therefore
//! share exactly one definition of reachability and child order.

use crate::digest::{self, CodeIdentity};
use crate::engine::{walk, GraphCost, GraphLimits, Visitor};
use lm_abi::{FaultCode, SnapshotClass};
use lm_heap::{BoundaryPolicy, Heap, Object};
use lm_value::{ObjRef, Value};

// ---------------------------------------------------------------
// Mark and sweep.
// ---------------------------------------------------------------

/// Collect garbage. `roots` holds every entry point outside the heap;
/// the host roots join them here.
///
/// The walk marks the reachable set in the graph work table, and the
/// heap frees every live slot the table rejects.
pub fn collect(heap: &mut Heap, roots: impl IntoIterator<Item = ObjRef>) {
    let mut all: Vec<ObjRef> = roots.into_iter().collect();
    all.extend_from_slice(heap.host_roots());
    let mut scratch = heap.take_scratch();
    walk(heap, &mut scratch, &all, &GraphLimits::UNBOUNDED, &mut ())
        .expect("the mark mode has no limit and no rejecting visitor");
    heap.sweep(|slot| scratch.seen(slot));
    heap.put_scratch(scratch);
}

// ---------------------------------------------------------------
// Deep freeze and frozen verification.
// ---------------------------------------------------------------

/// Deeply freeze the graph under `root`, preserving cycles and
/// sharing.
///
/// The walk validates the whole reachable graph first, and the bits
/// go on afterwards. A rejected walk therefore leaves every frozen
/// bit as it was.
pub fn freeze(heap: &mut Heap, root: ObjRef, limits: &GraphLimits) -> Result<(), FaultCode> {
    let mut scratch = heap.take_scratch();
    let walked = walk(heap, &mut scratch, &[root], limits, &mut ());
    if walked.is_ok() {
        for r in scratch.order() {
            heap.set_frozen(*r);
        }
    }
    heap.put_scratch(scratch);
    walked.map(|_| ())
}

/// A visitor that rejects the first object without the frozen bit.
struct FrozenCheck<'h> {
    heap: &'h Heap,
}

impl Visitor for FrozenCheck<'_> {
    fn enter(&mut self, r: ObjRef, _: u32, _: &Object) -> Result<(), FaultCode> {
        if self.heap.is_frozen(r) {
            Ok(())
        } else {
            Err(FaultCode::UnsendableValue)
        }
    }
}

/// Check that every object reachable from `root` carries the frozen
/// bit.
pub fn verify_frozen(
    heap: &mut Heap,
    root: ObjRef,
    limits: &GraphLimits,
) -> Result<GraphCost, FaultCode> {
    let mut scratch = heap.take_scratch();
    let out = {
        let view: &Heap = heap;
        walk(
            view,
            &mut scratch,
            &[root],
            limits,
            &mut FrozenCheck { heap: view },
        )
    };
    heap.put_scratch(scratch);
    out
}

// ---------------------------------------------------------------
// Boundary transfer, structural copy, and detached inspection.
// ---------------------------------------------------------------

/// What a copy demands of the source graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    /// A boundary transfer. Every reachable object must be frozen.
    Transfer,
    /// A detached inspection copy. A mutable source object is
    /// accepted, and the copy is frozen (specification 16.5).
    Detach,
}

/// A visitor that admits only the shapes a copy can carry.
struct CopyCheck<'h> {
    heap: &'h Heap,
    mode: CopyMode,
}

impl Visitor for CopyCheck<'_> {
    fn enter(&mut self, r: ObjRef, _: u32, object: &Object) -> Result<(), FaultCode> {
        if self.mode == CopyMode::Transfer && !self.heap.is_frozen(r) {
            return Err(FaultCode::UnsendableValue);
        }
        if object.shape().boundary == BoundaryPolicy::HolderLocal {
            return Err(FaultCode::UnsendableValue);
        }
        Ok(())
    }
}

/// Transfer one value from `src` into `dst`.
///
/// The copy preserves cycles and sharing, and it is failure-atomic
/// for the destination heap: a rejected copy frees every shell it
/// allocated, so the destination keeps its earlier live count and
/// byte count.
///
/// `dst_roots` holds the destination entry points outside its heap.
/// A destination collection during the copy reads them.
pub fn transfer(
    src: &mut Heap,
    dst: &mut Heap,
    dst_roots: &[ObjRef],
    value: Value,
    limits: &GraphLimits,
) -> Result<Value, FaultCode> {
    copy_value(src, dst, dst_roots, value, limits, CopyMode::Transfer)
}

/// Copy one value out of `src` as a detached frozen graph.
///
/// Inspection never returns a writable guest reference, so the copy
/// is frozen whatever the source was.
pub fn detach(
    src: &mut Heap,
    dst: &mut Heap,
    dst_roots: &[ObjRef],
    value: Value,
    limits: &GraphLimits,
) -> Result<Value, FaultCode> {
    copy_value(src, dst, dst_roots, value, limits, CopyMode::Detach)
}

fn copy_value(
    src: &mut Heap,
    dst: &mut Heap,
    dst_roots: &[ObjRef],
    value: Value,
    limits: &GraphLimits,
    mode: CopyMode,
) -> Result<Value, FaultCode> {
    let root = match value {
        Value::Unit | Value::Bool(_) | Value::Int(_) | Value::Op(_) => return Ok(value),
        Value::Uninit => unreachable!("no verified value is uninitialized"),
        Value::Obj(r) => r,
    };
    let mut scratch = src.take_scratch();
    // Pass 1: reach the graph in canonical order and check every
    // shape. Nothing in the destination changes yet.
    let discovered = {
        let view: &Heap = src;
        walk(
            view,
            &mut scratch,
            &[root],
            limits,
            &mut CopyCheck { heap: view, mode },
        )
    };
    let result = discovered.and_then(|_| copy_passes(src, dst, dst_roots, &scratch, root));
    src.put_scratch(scratch);
    result
}

/// Passes 2 and 3: allocate the destination shells, then patch every
/// child reference through the identity table.
fn copy_passes(
    src: &Heap,
    dst: &mut Heap,
    dst_roots: &[ObjRef],
    scratch: &lm_heap::GraphScratch,
    root: ObjRef,
) -> Result<Value, FaultCode> {
    let order = scratch.order();
    let mut new_refs: Vec<ObjRef> = Vec::with_capacity(order.len());
    let mut result = Ok(());
    for r in order {
        let shell = src
            .get(*r)
            .shell()
            .expect("pass 1 admitted sendable shapes only");
        let cost = shell.cost();
        if dst.would_exceed(cost) {
            // The shells are host-rooted already, so a collection
            // here keeps the partial copy. A shell holds unit
            // placeholders, so it roots nothing else.
            collect(dst, dst_roots.iter().copied());
            if dst.would_exceed(cost) {
                result = Err(FaultCode::HeapLimit);
                break;
            }
        }
        let new_ref = dst.alloc(shell);
        dst.push_host_root(new_ref);
        new_refs.push(new_ref);
    }
    if result.is_ok() {
        // Pass 3: patch children through the identity table.
        for (old, new) in order.iter().zip(new_refs.iter()) {
            if let Some(patched) = src
                .get(*old)
                .remap(|child| new_refs[scratch.ordinal(child.slot) as usize])
            {
                *dst.get_mut(*new) = patched;
                dst.recharge(*new);
            }
        }
    }
    // Unroot in LIFO order.
    for r in new_refs.iter().rev() {
        dst.pop_host_root(*r);
    }
    if result.is_err() {
        // Failure atomicity: free every shell this call allocated.
        for r in new_refs.iter().rev() {
            dst.free(*r);
        }
        result?;
    }
    // Every copied object is frozen: a transfer required a frozen
    // source, and a detached copy is frozen by rule.
    for r in &new_refs {
        dst.set_frozen(*r);
    }
    Ok(Value::Obj(new_refs[scratch.ordinal(root.slot) as usize]))
}

// ---------------------------------------------------------------
// Canonical digest.
// ---------------------------------------------------------------

/// The canonical digest of one value (specification 10.3).
///
/// The encoding walks the graph in canonical order, assigns an
/// ordinal at the first encounter of each object, and writes a
/// back-reference for every later encounter. It names code and
/// classes by verified semantic hash, so a digest never depends on a
/// numeric slot of one linked program.
///
/// The graph must be frozen. A live holder-local value raises
/// `BoundaryViolation`.
pub fn digest_value(
    heap: &mut Heap,
    value: Value,
    codes: &dyn CodeIdentity,
    limits: &GraphLimits,
) -> Result<[u8; 32], FaultCode> {
    if let Value::Obj(root) = value {
        if let Some(cached) = heap.cached_digest(root) {
            return Ok(cached);
        }
    }
    let mut scratch = heap.take_scratch();
    let out = {
        let view: &Heap = heap;
        digest::compute(view, &mut scratch, value, codes, limits)
    };
    heap.put_scratch(scratch);
    let out = out?;
    if let Value::Obj(root) = value {
        // Only a frozen object caches: the walk proved it frozen.
        heap.cache_digest(root, out);
    }
    Ok(out)
}

// ---------------------------------------------------------------
// Snapshot traversal.
// ---------------------------------------------------------------

/// A visitor that rejects a reachable live host attachment.
struct SnapshotCheck;

impl Visitor for SnapshotCheck {
    fn enter(&mut self, _: ObjRef, _: u32, object: &Object) -> Result<(), FaultCode> {
        match object.shape().snapshot {
            SnapshotClass::MachineState => Ok(()),
            SnapshotClass::HostAttachment => Err(FaultCode::BoundaryViolation),
        }
    }
}

/// Assign canonical snapshot ordinals to the graph under `roots`.
///
/// This is the traversal half of a snapshot only. Week 7 defines no
/// snapshot byte format, so the mode produces the ordinal order and
/// the rejection rule and nothing else.
pub fn snapshot_ordinals(
    heap: &mut Heap,
    roots: &[ObjRef],
    limits: &GraphLimits,
) -> Result<Vec<ObjRef>, FaultCode> {
    let mut scratch = heap.take_scratch();
    let walked = walk(heap, &mut scratch, roots, limits, &mut SnapshotCheck);
    let out = walked.map(|_| scratch.order().to_vec());
    heap.put_scratch(scratch);
    out
}
