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

/// What a copy does with the frozen bit of the source graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyMode {
    /// A boundary transfer. The copy preserves the frozen bit of
    /// every source object, so a mutable graph copies as a mutable
    /// graph (specification 16.1).
    Transfer,
    /// A detached inspection copy. The copy is frozen whatever the
    /// source was (specification 16.5).
    Detach,
}

/// A visitor that admits only the shapes a copy can carry.
///
/// The frozen bit is not part of this rule. A boundary crossing
/// copies the graph, and the copy keeps the frozen bit each object
/// carried. Only a holder-local shape refuses to cross.
struct CopyCheck;

impl Visitor for CopyCheck {
    fn enter(&mut self, _: ObjRef, _: u32, object: &Object) -> Result<(), FaultCode> {
        if object.shape().boundary == BoundaryPolicy::HolderLocal {
            return Err(FaultCode::UnsendableValue);
        }
        Ok(())
    }
}

/// Check that every object reachable from `root` satisfies the
/// boundary rules of a transfer.
///
/// A transfer demands one thing of the source graph: a sendable shape
/// on every object. A caller that has no copy to run still owes that
/// rule, so it runs this walk instead of the copy.
///
/// The walk carries the same `CopyCheck` visitor the copy runs, so
/// the standalone answer and the copy answer cannot drift.
pub fn verify_sendable(
    heap: &mut Heap,
    root: ObjRef,
    limits: &GraphLimits,
) -> Result<GraphCost, FaultCode> {
    let mut scratch = heap.take_scratch();
    let out = walk(heap, &mut scratch, &[root], limits, &mut CopyCheck);
    heap.put_scratch(scratch);
    out
}

/// Transfer one value from `src` into `dst`.
///
/// The copy preserves cycles, sharing, and the frozen bit of every
/// source object. Nothing is shared across the boundary, and the
/// identity of the source graph does not cross.
///
/// The copy is failure-atomic
/// for the destination heap: a rejected copy frees every shell it
/// allocated, so the destination keeps its earlier live count and
/// byte count.
///
/// `dst_roots` holds the destination entry points outside its heap.
/// A destination collection during the copy reads them.
///
/// The result is not rooted. A caller that copies several values into
/// one destination must root each result before it copies the next
/// one, because a collection during a later copy frees every
/// destination object the roots do not reach.
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
        // A verified read never produces the marker, and a local slot
        // that holds it is unreadable. A caller that still hands one
        // over takes a local fault instead of a host panic.
        Value::Uninit => return Err(FaultCode::BoundaryViolation),
        Value::Obj(r) => r,
    };
    let mut scratch = src.take_scratch();
    // Pass 1: reach the graph in canonical order and check every
    // shape. Nothing in the destination changes yet.
    let discovered = walk(src, &mut scratch, &[root], limits, &mut CopyCheck);
    let result = discovered.and_then(|_| copy_passes(src, dst, dst_roots, &scratch, root, mode));
    src.put_scratch(scratch);
    result
}

/// Copy one value inside one heap.
///
/// A proc may hold its own handle, so a message can name the sending
/// machine. That send crosses a machine boundary with no second heap
/// behind it. The boundary rule is the same rule, so the message
/// copies here as well: the sender and the mailbox never share one
/// mutable graph.
///
/// `roots` holds the entry points of the heap outside itself. The
/// source graph roots itself for the length of the call, so a
/// collection during the copy keeps it.
pub fn copy_within(
    heap: &mut Heap,
    roots: &[ObjRef],
    value: Value,
    limits: &GraphLimits,
) -> Result<Value, FaultCode> {
    let root = match value {
        Value::Unit | Value::Bool(_) | Value::Int(_) | Value::Op(_) => return Ok(value),
        // A verified read never produces the marker, and a local slot
        // that holds it is unreadable. A caller that still hands one
        // over takes a local fault instead of a host panic.
        Value::Uninit => return Err(FaultCode::BoundaryViolation),
        Value::Obj(r) => r,
    };
    let mut scratch = heap.take_scratch();
    // Pass 1 is the shared visitor, so the one-heap answer and the
    // two-heap answer cannot drift.
    let discovered = walk(heap, &mut scratch, &[root], limits, &mut CopyCheck);
    let result = discovered.and_then(|_| {
        heap.push_host_root(root);
        let out = copy_passes_within(heap, roots, &scratch, root);
        heap.pop_host_root(root);
        out
    });
    heap.put_scratch(scratch);
    result
}

/// Passes 2 and 3 inside one heap.
///
/// The shells allocate first and root themselves, then every child
/// reference patches through the identity table. A collection between
/// two allocations therefore keeps the source graph and the partial
/// copy.
fn copy_passes_within(
    heap: &mut Heap,
    roots: &[ObjRef],
    scratch: &lm_heap::GraphScratch,
    root: ObjRef,
) -> Result<Value, FaultCode> {
    let order = scratch.order();
    let mut new_refs: Vec<ObjRef> = Vec::with_capacity(order.len());
    let mut result = Ok(());
    for r in order {
        let shell = heap
            .get(*r)
            .shell()
            .expect("pass 1 admitted sendable shapes only");
        let cost = shell.cost();
        if heap.would_exceed(cost) {
            collect(heap, roots.iter().copied());
            if heap.would_exceed(cost) {
                result = Err(FaultCode::HeapLimit);
                break;
            }
        }
        let new_ref = heap.alloc(shell);
        heap.push_host_root(new_ref);
        new_refs.push(new_ref);
    }
    if result.is_ok() {
        // Pass 3: patch children through the identity table.
        for (old, new) in order.iter().zip(new_refs.iter()) {
            let patched = heap
                .get(*old)
                .remap(|child| new_refs[scratch.ordinal(child.slot) as usize]);
            if let Some(patched) = patched {
                *heap.get_mut(*new) = patched;
                heap.recharge(*new);
            }
        }
    }
    // Unroot in LIFO order.
    for r in new_refs.iter().rev() {
        heap.pop_host_root(*r);
    }
    if result.is_err() {
        // Failure atomicity: free every shell this call allocated.
        for r in new_refs.iter().rev() {
            heap.free(*r);
        }
        result?;
    }
    // The copy carries the frozen bit of its source object.
    for (old, new) in order.iter().zip(new_refs.iter()) {
        if heap.is_frozen(*old) {
            heap.set_frozen(*new);
        }
    }
    Ok(Value::Obj(new_refs[scratch.ordinal(root.slot) as usize]))
}

/// Passes 2 and 3: allocate the destination shells, then patch every
/// child reference through the identity table.
fn copy_passes(
    src: &Heap,
    dst: &mut Heap,
    dst_roots: &[ObjRef],
    scratch: &lm_heap::GraphScratch,
    root: ObjRef,
    mode: CopyMode,
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
    // A transfer copy carries the frozen bit of its source object. A
    // detached copy is frozen by rule (specification 16.5).
    for (old, new) in order.iter().zip(new_refs.iter()) {
        if mode == CopyMode::Detach || src.is_frozen(*old) {
            dst.set_frozen(*new);
        }
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
        // A cache hit costs one lookup, which is under every published
        // limit, so the walk and its limits do not run again. The
        // frozen bit is monotonic, so a cached answer stays true.
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

#[cfg(test)]
mod tests {
    use super::*;
    use lm_heap::MapIndex;

    /// A code-identity stub: the hash of one slot is that slot.
    struct Slots;

    impl CodeIdentity for Slots {
        fn func_hash(&self, func: u32) -> Result<[u8; 32], FaultCode> {
            let mut out = [0u8; 32];
            out[0] = func as u8;
            Ok(out)
        }

        fn class_hash(&self, class: u32) -> Result<[u8; 32], FaultCode> {
            let mut out = [1u8; 32];
            out[0] = class as u8;
            Ok(out)
        }
    }

    fn limits() -> GraphLimits {
        GraphLimits::default()
    }

    /// Build a frozen two-node cycle and return its root.
    fn frozen_ring(heap: &mut Heap, first: i64, second: i64) -> ObjRef {
        let a = heap.alloc(Object::List {
            items: vec![Value::Int(first)],
        });
        let b = heap.alloc(Object::List {
            items: vec![Value::Int(second), Value::Obj(a)],
        });
        if let Object::List { items } = heap.get_mut(a) {
            items.push(Value::Obj(b));
        }
        heap.recharge(a);
        freeze(heap, a, &limits()).expect("the freeze finishes");
        a
    }

    // -----------------------------------------------------------
    // Mark and freeze.
    // -----------------------------------------------------------

    #[test]
    fn collect_keeps_reachable_cycles_and_frees_unreachable_ones() {
        let mut heap = Heap::new(1 << 20);
        let keep = frozen_ring(&mut heap, 1, 2);
        let drop = frozen_ring(&mut heap, 3, 4);
        assert_eq!(heap.live_count(), 4);
        collect(&mut heap, [keep]);
        assert_eq!(heap.live_count(), 2);
        assert!(heap.try_get(keep).is_some());
        assert!(heap.try_get(drop).is_none());
    }

    #[test]
    fn freeze_preserves_sharing_and_cycles() {
        let mut heap = Heap::new(1 << 20);
        let root = frozen_ring(&mut heap, 1, 2);
        assert!(heap.is_frozen(root));
        assert_eq!(
            verify_frozen(&mut heap, root, &limits())
                .expect("the graph is frozen")
                .objects,
            2
        );
    }

    /// A rejected freeze sets no bit. The whole graph validates first.
    #[test]
    fn a_rejected_freeze_leaves_every_bit_alone() {
        let mut heap = Heap::new(1 << 20);
        let leaf = heap.alloc(Object::List { items: vec![] });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(leaf)],
        });
        let tight = GraphLimits {
            max_objects: 1,
            ..limits()
        };
        assert_eq!(
            freeze(&mut heap, root, &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert!(!heap.is_frozen(root));
        assert!(!heap.is_frozen(leaf));
    }

    #[test]
    fn verification_rejects_a_mutable_child() {
        let mut heap = Heap::new(1 << 20);
        let leaf = heap.alloc(Object::List { items: vec![] });
        let root = heap.alloc(Object::Tuple {
            items: vec![Value::Obj(leaf)],
        });
        // A tuple is born frozen, but its child is not.
        assert!(heap.is_frozen(root));
        assert_eq!(
            verify_frozen(&mut heap, root, &limits()),
            Err(FaultCode::UnsendableValue)
        );
    }

    /// The two standalone checks answer different questions, and a
    /// caller that owes the boundary rule must run the sendable one.
    ///
    /// A machine handle is born frozen and holder local. The frozen
    /// check therefore accepts it, and only the sendable check
    /// rejects it, exactly as a copy would.
    #[test]
    fn the_sendable_check_rejects_what_the_frozen_check_admits() {
        let mut heap = Heap::new(1 << 20);
        let handle = heap.alloc(Object::NativeVm { vm: 1 });
        assert!(heap.is_frozen(handle));
        assert!(verify_frozen(&mut heap, handle, &limits()).is_ok());
        assert_eq!(
            verify_sendable(&mut heap, handle, &limits()),
            Err(FaultCode::UnsendableValue)
        );
        // A holder-local object inside a frozen container hides from
        // the frozen check as well.
        let wrapper = heap.alloc(Object::Tuple {
            items: vec![Value::Obj(handle)],
        });
        assert!(verify_frozen(&mut heap, wrapper, &limits()).is_ok());
        assert_eq!(
            verify_sendable(&mut heap, wrapper, &limits()),
            Err(FaultCode::UnsendableValue)
        );
    }

    /// The sendable check asks about shapes alone. A mutable graph
    /// crosses a boundary as a copy, so it passes the check.
    #[test]
    fn the_sendable_check_admits_a_mutable_graph() {
        let mut heap = Heap::new(1 << 20);
        let leaf = heap.alloc(Object::List { items: vec![] });
        let root = heap.alloc(Object::Tuple {
            items: vec![Value::Obj(leaf)],
        });
        assert_eq!(
            verify_sendable(&mut heap, root, &limits())
                .expect("a mutable graph is sendable")
                .objects,
            2
        );
        let ring = frozen_ring(&mut heap, 1, 2);
        assert_eq!(
            verify_sendable(&mut heap, ring, &limits())
                .expect("the graph is sendable")
                .objects,
            2
        );
    }

    // -----------------------------------------------------------
    // Transfer, copy, and inspection.
    // -----------------------------------------------------------

    #[test]
    fn transfer_preserves_a_cycle_and_its_sharing() {
        let mut src = Heap::new(1 << 20);
        let mut dst = Heap::new(1 << 20);
        let root = frozen_ring(&mut src, 1, 2);
        let moved = transfer(&mut src, &mut dst, &[], Value::Obj(root), &limits())
            .expect("the frozen ring crosses");
        let new_root = moved.as_obj().expect("a copied graph is an object");
        assert_eq!(dst.live_count(), 2);
        assert!(dst.is_frozen(new_root));
        // The ring closes back on the copied root, not on the source.
        let next = match dst.get(new_root) {
            Object::List { items } => items[1].as_obj().expect("the next node"),
            other => panic!("expected a list, got {other:?}"),
        };
        let back = match dst.get(next) {
            Object::List { items } => items[1].as_obj().expect("the back edge"),
            other => panic!("expected a list, got {other:?}"),
        };
        assert_eq!(back, new_root);
    }

    /// A mutable graph crosses as a mutable copy. A holder-local
    /// shape still refuses to cross.
    #[test]
    fn transfer_copies_a_mutable_graph_and_rejects_a_holder_local_one() {
        let mut src = Heap::new(1 << 20);
        let mut dst = Heap::new(1 << 20);
        let mutable = src.alloc(Object::List {
            items: vec![Value::Int(1)],
        });
        let moved = transfer(&mut src, &mut dst, &[], Value::Obj(mutable), &limits())
            .expect("a mutable graph crosses");
        let new_root = moved.as_obj().expect("the copy is an object");
        assert!(!dst.is_frozen(new_root), "the copy stays mutable");
        assert_eq!(dst.live_count(), 1);
        // The copy is a second object: a later source write is not
        // visible through it.
        if let Object::List { items } = src.get_mut(mutable) {
            items.push(Value::Int(2));
        }
        src.recharge(mutable);
        match dst.get(new_root) {
            Object::List { items } => assert_eq!(items, &vec![Value::Int(1)]),
            other => panic!("expected a list, got {other:?}"),
        }
        let handle = src.alloc(Object::NativeVm { vm: 3 });
        assert_eq!(
            transfer(&mut src, &mut dst, &[], Value::Obj(handle), &limits()),
            Err(FaultCode::UnsendableValue)
        );
        assert_eq!(dst.live_count(), 1);
    }

    /// The copy carries the frozen bit of each source object, so a
    /// frozen subgraph inside a mutable wrapper stays frozen and the
    /// wrapper stays mutable.
    #[test]
    fn transfer_preserves_every_frozen_bit() {
        let mut src = Heap::new(1 << 20);
        let mut dst = Heap::new(1 << 20);
        let inner = src.alloc(Object::List {
            items: vec![Value::Int(7)],
        });
        freeze(&mut src, inner, &limits()).expect("the inner list freezes");
        let outer = src.alloc(Object::List {
            items: vec![Value::Obj(inner)],
        });
        assert!(!src.is_frozen(outer));
        let moved = transfer(&mut src, &mut dst, &[], Value::Obj(outer), &limits())
            .expect("the graph crosses");
        let new_outer = moved.as_obj().expect("the copy is an object");
        assert!(!dst.is_frozen(new_outer), "the wrapper stays mutable");
        let new_inner = match dst.get(new_outer) {
            Object::List { items } => items[0].as_obj().expect("the inner list"),
            other => panic!("expected a list, got {other:?}"),
        };
        assert!(dst.is_frozen(new_inner), "the subgraph stays frozen");
    }

    /// A one-heap copy applies the same rule and makes a second
    /// graph, so the source and the copy never share one object.
    #[test]
    fn a_copy_inside_one_heap_makes_a_second_graph() {
        let mut heap = Heap::new(1 << 20);
        let leaf = heap.alloc(Object::List {
            items: vec![Value::Int(1)],
        });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(leaf), Value::Obj(leaf)],
        });
        let copy = copy_within(&mut heap, &[root], Value::Obj(root), &limits())
            .expect("a mutable graph copies");
        let new_root = copy.as_obj().expect("the copy is an object");
        assert_ne!(new_root, root);
        assert_eq!(heap.live_count(), 4);
        assert!(!heap.is_frozen(new_root), "the copy stays mutable");
        // The shared leaf stays one object inside the copy.
        let (first, second) = match heap.get(new_root) {
            Object::List { items } => (
                items[0].as_obj().expect("the first child"),
                items[1].as_obj().expect("the second child"),
            ),
            other => panic!("expected a list, got {other:?}"),
        };
        assert_eq!(first, second);
        assert_ne!(first, leaf);
        // A write through the source is not visible in the copy.
        if let Object::List { items } = heap.get_mut(leaf) {
            items.push(Value::Int(2));
        }
        heap.recharge(leaf);
        match heap.get(first) {
            Object::List { items } => assert_eq!(items, &vec![Value::Int(1)]),
            other => panic!("expected a list, got {other:?}"),
        }
        // The one-heap path keeps the shape rule of the copy.
        let handle = heap.alloc(Object::NativeVm { vm: 1 });
        assert_eq!(
            copy_within(&mut heap, &[], Value::Obj(handle), &limits()),
            Err(FaultCode::UnsendableValue)
        );
        // A scalar carries no reference and copies as itself.
        assert_eq!(
            copy_within(&mut heap, &[], Value::Int(9), &limits()),
            Ok(Value::Int(9))
        );
    }

    /// A transfer that runs out of destination bytes leaves the
    /// destination exactly as it was.
    #[test]
    fn a_failed_transfer_leaves_the_destination_unchanged() {
        let mut src = Heap::new(1 << 20);
        // A small destination: the first shells fit, the last does not.
        let mut dst = Heap::new(200);
        let anchor = dst.alloc(Object::Str("anchor".to_string()));
        let before_live = dst.live_count();
        let before_bytes = dst.used_bytes();
        let mut chain = src.alloc(Object::Str("tail".to_string()));
        for _ in 0..8 {
            chain = src.alloc(Object::Tuple {
                items: vec![Value::Obj(chain)],
            });
        }
        freeze(&mut src, chain, &limits()).expect("the chain freezes");
        assert_eq!(
            transfer(&mut src, &mut dst, &[anchor], Value::Obj(chain), &limits()),
            Err(FaultCode::HeapLimit)
        );
        assert_eq!(dst.live_count(), before_live);
        assert_eq!(dst.used_bytes(), before_bytes);
        assert_eq!(dst.get(anchor), &Object::Str("anchor".to_string()));
    }

    /// A destination collection during one copy frees an earlier
    /// result that no root reaches. The caller must root each result
    /// before it copies the next value.
    #[test]
    fn a_later_transfer_needs_the_earlier_result_rooted() {
        let payload = |heap: &mut Heap, text: &str| {
            let leaf = heap.alloc(Object::Str(text.to_string()));
            heap.alloc(Object::Tuple {
                items: vec![Value::Obj(leaf)],
            })
        };
        let build = || {
            let mut src = Heap::new(1 << 20);
            let first = payload(&mut src, "first payload");
            let second = payload(&mut src, "second payload");
            freeze(&mut src, first, &limits()).expect("the first graph freezes");
            freeze(&mut src, second, &limits()).expect("the second graph freezes");
            (src, first, second)
        };
        // A cap that holds one payload but not two.
        let (mut src, first, second) = build();
        let one = src.get(first).cost() + src.get(second).cost();
        let mut dst = Heap::new(one + 40);
        let moved = transfer(&mut src, &mut dst, &[], Value::Obj(first), &limits())
            .expect("the first graph crosses");
        let moved_ref = moved.as_obj().expect("the copy is an object");
        // Without a root, the second copy collects the first away.
        transfer(&mut src, &mut dst, &[], Value::Obj(second), &limits())
            .expect("the second graph crosses");
        assert!(
            dst.try_get(moved_ref).is_none(),
            "an unrooted earlier result must not survive"
        );

        // With a root, the first result survives every later copy.
        let (mut src, first, second) = build();
        let mut dst = Heap::new(one + 40);
        let moved = transfer(&mut src, &mut dst, &[], Value::Obj(first), &limits())
            .expect("the first graph crosses");
        let moved_ref = moved.as_obj().expect("the copy is an object");
        dst.push_host_root(moved_ref);
        let out = transfer(&mut src, &mut dst, &[], Value::Obj(second), &limits());
        assert!(dst.try_get(moved_ref).is_some(), "a rooted result survives");
        // The destination cannot hold both, so the second copy stops
        // at the heap limit and leaves the first one whole.
        assert_eq!(out, Err(FaultCode::HeapLimit));
        dst.pop_host_root(moved_ref);
    }

    /// A one-heap copy that runs out of bytes leaves the heap with
    /// its earlier live count and byte count.
    #[test]
    fn a_failed_copy_inside_one_heap_frees_every_shell() {
        // A cap that holds the source but not a second copy of it.
        let mut heap = Heap::new(300);
        let mut chain = heap.alloc(Object::Str("tail".to_string()));
        for _ in 0..4 {
            chain = heap.alloc(Object::Tuple {
                items: vec![Value::Obj(chain)],
            });
        }
        let before_live = heap.live_count();
        let before_bytes = heap.used_bytes();
        assert_eq!(
            copy_within(&mut heap, &[chain], Value::Obj(chain), &limits()),
            Err(FaultCode::HeapLimit)
        );
        assert_eq!(heap.live_count(), before_live);
        assert_eq!(heap.used_bytes(), before_bytes);
        // The source graph survived the collection the copy ran.
        assert!(heap.try_get(chain).is_some());
    }

    /// Detached inspection copies a mutable graph and freezes the
    /// copy, so inspection never returns a writable reference.
    #[test]
    fn detach_copies_a_mutable_graph_and_freezes_it() {
        let mut src = Heap::new(1 << 20);
        let mut dst = Heap::new(1 << 20);
        let leaf = src.alloc(Object::Str("leaf".to_string()));
        let root = src.alloc(Object::List {
            items: vec![Value::Obj(leaf), Value::Obj(leaf)],
        });
        assert!(!src.is_frozen(root));
        let copy = detach(&mut src, &mut dst, &[], Value::Obj(root), &limits())
            .expect("inspection copies a mutable graph");
        let new_root = copy.as_obj().expect("the copy is an object");
        assert!(dst.is_frozen(new_root));
        // The shared leaf stays one object.
        assert_eq!(dst.live_count(), 2);
        // The source keeps its mutable bit.
        assert!(!src.is_frozen(root));
    }

    // -----------------------------------------------------------
    // Canonical digest.
    // -----------------------------------------------------------

    #[test]
    fn the_digest_ignores_allocation_order_and_heap_identity() {
        let mut first = Heap::new(1 << 20);
        let a = frozen_ring(&mut first, 1, 2);
        let mut second = Heap::new(1 << 20);
        // Allocate spare objects, so the equal ring uses other slots.
        for _ in 0..5 {
            second.alloc(Object::Str("spare".to_string()));
        }
        let b = frozen_ring(&mut second, 1, 2);
        assert_ne!(a.slot, b.slot);
        let da = digest_value(&mut first, Value::Obj(a), &Slots, &limits()).expect("a digests");
        let db = digest_value(&mut second, Value::Obj(b), &Slots, &limits()).expect("b digests");
        assert_eq!(da, db);
        // A different ring digests differently.
        let c = frozen_ring(&mut second, 1, 3);
        let dc = digest_value(&mut second, Value::Obj(c), &Slots, &limits()).expect("c digests");
        assert_ne!(da, dc);
    }

    /// Sharing and cycles are part of the encoding: a shared child and
    /// two equal children are different graphs.
    #[test]
    fn the_digest_separates_sharing_from_equal_copies() {
        let mut heap = Heap::new(1 << 20);
        let shared = heap.alloc(Object::Str("x".to_string()));
        let one = heap.alloc(Object::Tuple {
            items: vec![Value::Obj(shared), Value::Obj(shared)],
        });
        let left = heap.alloc(Object::Str("x".to_string()));
        let right = heap.alloc(Object::Str("x".to_string()));
        let two = heap.alloc(Object::Tuple {
            items: vec![Value::Obj(left), Value::Obj(right)],
        });
        let d1 = digest_value(&mut heap, Value::Obj(one), &Slots, &limits()).expect("one digests");
        let d2 = digest_value(&mut heap, Value::Obj(two), &Slots, &limits()).expect("two digests");
        assert_ne!(d1, d2);
    }

    #[test]
    fn the_digest_rejects_mutable_and_nondigestible_graphs() {
        let mut heap = Heap::new(1 << 20);
        let mutable = heap.alloc(Object::List { items: vec![] });
        assert_eq!(
            digest_value(&mut heap, Value::Obj(mutable), &Slots, &limits()),
            Err(FaultCode::UnsendableValue)
        );
        let handle = heap.alloc(Object::NativeVm { vm: 1 });
        assert_eq!(
            digest_value(&mut heap, Value::Obj(handle), &Slots, &limits()),
            Err(FaultCode::BoundaryViolation)
        );
    }

    #[test]
    fn a_frozen_object_caches_its_digest() {
        let mut heap = Heap::new(1 << 20);
        let root = frozen_ring(&mut heap, 1, 2);
        assert_eq!(heap.digest_cache_len(), 0);
        let first = digest_value(&mut heap, Value::Obj(root), &Slots, &limits()).expect("digests");
        assert_eq!(heap.digest_cache_len(), 1);
        let second = digest_value(&mut heap, Value::Obj(root), &Slots, &limits()).expect("digests");
        assert_eq!(first, second);
    }

    /// A map digests in insertion order, key before value, and the
    /// derived lookup index never enters the encoding.
    #[test]
    fn a_map_digests_in_insertion_order() {
        let mut heap = Heap::new(1 << 20);
        let build = |heap: &mut Heap, first: i64, second: i64| {
            let map = heap.alloc(Object::Map {
                entries: vec![
                    (Value::Int(first), Value::Int(10)),
                    (Value::Int(second), Value::Int(20)),
                ],
                index: MapIndex::default(),
            });
            freeze(heap, map, &GraphLimits::default()).expect("the map freezes");
            map
        };
        let forward = build(&mut heap, 1, 2);
        let backward = build(&mut heap, 2, 1);
        let d1 = digest_value(&mut heap, Value::Obj(forward), &Slots, &limits()).expect("digests");
        let d2 = digest_value(&mut heap, Value::Obj(backward), &Slots, &limits()).expect("digests");
        assert_ne!(d1, d2);
    }

    // -----------------------------------------------------------
    // Limits and snapshot traversal.
    // -----------------------------------------------------------

    /// Every mode rejects a graph past the published limits.
    #[test]
    fn every_mode_rejects_work_past_its_limits() {
        let mut heap = Heap::new(1 << 20);
        let root = frozen_ring(&mut heap, 1, 2);
        let mut dst = Heap::new(1 << 20);
        let tight = GraphLimits {
            max_objects: 1,
            ..limits()
        };
        assert_eq!(
            freeze(&mut heap, root, &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            verify_frozen(&mut heap, root, &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            transfer(&mut heap, &mut dst, &[], Value::Obj(root), &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            detach(&mut heap, &mut dst, &[], Value::Obj(root), &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            copy_within(&mut heap, &[root], Value::Obj(root), &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            digest_value(&mut heap, Value::Obj(root), &Slots, &tight),
            Err(FaultCode::BoundaryLimit)
        );
        assert_eq!(
            snapshot_ordinals(&mut heap, &[root], &tight),
            Err(FaultCode::BoundaryLimit)
        );
        // The destination never received a shell.
        assert_eq!(dst.live_count(), 0);
    }

    /// Every mode runs a 50,000-deep chain on a small Rust stack.
    #[test]
    fn every_mode_runs_a_deep_chain_off_the_rust_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut heap = Heap::new(64 << 20);
                let mut head = heap.alloc(Object::List { items: vec![] });
                for _ in 0..50_000 {
                    head = heap.alloc(Object::List {
                        items: vec![Value::Obj(head)],
                    });
                }
                freeze(&mut heap, head, &limits()).expect("the freeze finishes");
                verify_frozen(&mut heap, head, &limits()).expect("the graph is frozen");
                digest_value(&mut heap, Value::Obj(head), &Slots, &limits())
                    .expect("the digest finishes");
                snapshot_ordinals(&mut heap, &[head], &limits()).expect("the walk finishes");
                let mut dst = Heap::new(64 << 20);
                transfer(&mut heap, &mut dst, &[], Value::Obj(head), &limits())
                    .expect("the transfer finishes");
                assert_eq!(dst.live_count(), 50_001);
                collect(&mut heap, []);
                assert_eq!(heap.live_count(), 0);
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }

    #[test]
    fn snapshot_traversal_orders_the_reachable_graph() {
        let mut heap = Heap::new(1 << 20);
        let root = frozen_ring(&mut heap, 1, 2);
        let order = snapshot_ordinals(&mut heap, &[root], &limits()).expect("the walk finishes");
        assert_eq!(order.len(), 2);
        assert_eq!(order[0], root);
    }
}
