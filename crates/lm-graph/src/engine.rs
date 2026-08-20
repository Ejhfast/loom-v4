//! The one non-recursive traversal engine.
//!
//! Every graph mode runs this walk. The walk owns reachability,
//! child order, the identity table, the canonical traversal ordinals,
//! and the four work limits. A mode adds only its own result state
//! through a `Visitor`, so no mode carries a second shape table and no
//! mode recurses on the Rust stack.

use lm_abi::FaultCode;
use lm_heap::{GraphScratch, Heap, Object};
use lm_value::ObjRef;

/// The work limits of one graph mode.
///
/// Every mode publishes limits. A walk that passes any limit stops
/// with `BoundaryLimit` and leaves the heap unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLimits {
    /// The largest number of distinct objects one walk reaches.
    pub max_objects: u64,
    /// The largest number of child references one walk reads.
    pub max_edges: u64,
    /// The largest total logical byte cost of the reached objects.
    pub max_bytes: u64,
    /// The largest number of work-list steps one walk retires.
    pub max_work: u64,
}

impl GraphLimits {
    /// The limits of the mark mode.
    ///
    /// Marking has no separate budget: the heap cap already bounds the
    /// object table, and a mark that refused to finish would turn a
    /// full heap into a crash instead of a collection.
    pub const UNBOUNDED: GraphLimits = GraphLimits {
        max_objects: u64::MAX,
        max_edges: u64::MAX,
        max_bytes: u64::MAX,
        max_work: u64::MAX,
    };
}

impl Default for GraphLimits {
    /// The published default for freeze, transfer, copy, digest,
    /// verification, inspection, and snapshot traversal.
    fn default() -> GraphLimits {
        GraphLimits {
            max_objects: 1 << 24,
            max_edges: 1 << 26,
            max_bytes: 1 << 30,
            max_work: 1 << 26,
        }
    }
}

/// The work one walk charged against its limits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GraphCost {
    pub objects: u64,
    pub edges: u64,
    pub bytes: u64,
    pub work: u64,
}

/// The per-mode result state of one walk.
///
/// The engine calls `enter` once per object, at its first encounter,
/// in canonical traversal order. A mode rejects the graph by
/// returning a fault; the engine stops the walk at once.
pub trait Visitor {
    fn enter(&mut self, r: ObjRef, ordinal: u32, object: &Object) -> Result<(), FaultCode>;
}

/// The visitor of a mode that only needs the reachable set and the
/// ordinals, such as mark and freeze.
impl Visitor for () {
    fn enter(&mut self, _: ObjRef, _: u32, _: &Object) -> Result<(), FaultCode> {
        Ok(())
    }
}

/// Walk the graph under `roots` in canonical order.
///
/// The order is a preorder over the child order each shape declares:
/// the engine assigns the ordinal of an object at its first
/// encounter, and it visits the children of an object before the
/// later siblings of that object. The order therefore depends on the
/// graph alone, never on allocation order or slot numbers.
///
/// The work list is explicit, so graph depth never reaches the Rust
/// stack. On success `scratch` holds the reached objects in ordinal
/// order and the identity table that maps a slot to its ordinal.
pub fn walk(
    heap: &Heap,
    scratch: &mut GraphScratch,
    roots: &[ObjRef],
    limits: &GraphLimits,
    visitor: &mut impl Visitor,
) -> Result<GraphCost, FaultCode> {
    scratch.begin(heap.slot_count());
    let mut stack: Vec<ObjRef> = Vec::with_capacity(roots.len());
    let mut children: Vec<ObjRef> = Vec::new();
    let mut cost = GraphCost::default();
    // The first root pops first, so the ordinals follow root order.
    stack.extend(roots.iter().rev().copied());
    while let Some(r) = stack.pop() {
        cost.work += 1;
        if cost.work > limits.max_work {
            return Err(FaultCode::BoundaryLimit);
        }
        if scratch.seen(r.slot) {
            continue;
        }
        let object = heap.get(r);
        cost.objects += 1;
        cost.bytes += object.cost() as u64;
        if cost.objects > limits.max_objects || cost.bytes > limits.max_bytes {
            return Err(FaultCode::BoundaryLimit);
        }
        let ordinal = scratch.record(r);
        visitor.enter(r, ordinal, object)?;
        children.clear();
        object.children(&mut children);
        cost.edges += children.len() as u64;
        if cost.edges > limits.max_edges {
            return Err(FaultCode::BoundaryLimit);
        }
        stack.extend(children.iter().rev().copied());
    }
    Ok(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_heap::Object;
    use lm_value::Value;

    /// A visitor that records the order it saw.
    struct Recorder(Vec<u32>);

    impl Visitor for Recorder {
        fn enter(&mut self, r: ObjRef, ordinal: u32, _: &Object) -> Result<(), FaultCode> {
            assert_eq!(ordinal as usize, self.0.len());
            self.0.push(r.slot);
            Ok(())
        }
    }

    /// A three-level tree: the ordinals follow the declared child
    /// order, not the allocation order.
    #[test]
    fn ordinals_follow_the_declared_child_order() {
        let mut heap = Heap::new(1 << 20);
        // Allocate the right branch first, so allocation order and
        // child order disagree.
        let right = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        let left = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(left), Value::Obj(right)],
            epoch: Default::default(),
        });
        let mut scratch = heap.take_scratch();
        let mut seen = Recorder(Vec::new());
        walk(
            &heap,
            &mut scratch,
            &[root],
            &GraphLimits::default(),
            &mut seen,
        )
        .expect("the walk finishes");
        assert_eq!(seen.0, vec![root.slot, left.slot, right.slot]);
        assert_eq!(scratch.ordinal(right.slot), 2);
    }

    /// A cycle terminates, and a shared node takes one ordinal.
    #[test]
    fn a_cycle_terminates_and_sharing_takes_one_ordinal() {
        let mut heap = Heap::new(1 << 20);
        let shared = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(shared), Value::Obj(shared)],
            epoch: Default::default(),
        });
        if let Object::List { items, .. } = heap.get_mut(shared) {
            items.push(Value::Obj(root));
        }
        heap.recharge(shared);
        let mut scratch = heap.take_scratch();
        let cost = walk(
            &heap,
            &mut scratch,
            &[root],
            &GraphLimits::default(),
            &mut (),
        )
        .expect("the walk finishes");
        assert_eq!(cost.objects, 2);
        assert_eq!(scratch.order().len(), 2);
    }

    #[test]
    fn each_limit_rejects_on_its_own() {
        let mut heap = Heap::new(1 << 20);
        let leaf = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(leaf)],
            epoch: Default::default(),
        });
        let base = GraphLimits::default();
        for limits in [
            GraphLimits {
                max_objects: 1,
                ..base
            },
            GraphLimits {
                max_edges: 0,
                ..base
            },
            GraphLimits {
                max_bytes: 8,
                ..base
            },
            GraphLimits {
                max_work: 1,
                ..base
            },
        ] {
            let mut scratch = heap.take_scratch();
            let out = walk(&heap, &mut scratch, &[root], &limits, &mut ());
            heap.put_scratch(scratch);
            assert_eq!(out, Err(FaultCode::BoundaryLimit), "{limits:?}");
        }
        // The same graph passes under the published default.
        let mut scratch = heap.take_scratch();
        assert!(walk(&heap, &mut scratch, &[root], &base, &mut ()).is_ok());
    }

    /// A one-hundred-thousand-deep chain never reaches the Rust
    /// stack, on a small stack.
    #[test]
    fn deep_chains_never_recurse() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut heap = Heap::new(64 << 20);
                let mut head = heap.alloc(Object::List {
                    items: vec![],
                    epoch: Default::default(),
                });
                for _ in 0..100_000 {
                    head = heap.alloc(Object::List {
                        items: vec![Value::Obj(head)],
                        epoch: Default::default(),
                    });
                }
                let mut scratch = heap.take_scratch();
                let cost = walk(
                    &heap,
                    &mut scratch,
                    &[head],
                    &GraphLimits::default(),
                    &mut (),
                )
                .expect("the walk finishes");
                assert_eq!(cost.objects, 100_001);
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }
}
