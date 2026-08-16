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

    #[test]
    fn transfer_rejects_a_mutable_or_holder_local_graph() {
        let mut src = Heap::new(1 << 20);
        let mut dst = Heap::new(1 << 20);
        let mutable = src.alloc(Object::List { items: vec![] });
        assert_eq!(
            transfer(&mut src, &mut dst, &[], Value::Obj(mutable), &limits()),
            Err(FaultCode::UnsendableValue)
        );
        let handle = src.alloc(Object::NativeVm { vm: 3 });
        assert_eq!(
            transfer(&mut src, &mut dst, &[], Value::Obj(handle), &limits()),
            Err(FaultCode::UnsendableValue)
        );
        assert_eq!(dst.live_count(), 0);
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
        assert_eq!(
            transfer(&mut src, &mut dst, &[], Value::Obj(root), &limits()),
            Err(FaultCode::UnsendableValue)
        );
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

    #[test]
    fn snapshot_traversal_orders_the_reachable_graph() {
        let mut heap = Heap::new(1 << 20);
        let root = frozen_ring(&mut heap, 1, 2);
        let order = snapshot_ordinals(&mut heap, &[root], &limits()).expect("the walk finishes");
        assert_eq!(order.len(), 2);
        assert_eq!(order[0], root);
    }
}
