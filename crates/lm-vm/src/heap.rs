//! The per-VM heap: object table, allocation pages, and a
//! stop-the-VM mark/sweep collector.
//!
//! Objects live in a slot table that grows in fixed-size pages, so an
//! entry never moves. A reference is a `(slot, generation)` pair. The
//! sweep step raises the generation of a freed slot, so a stale
//! reference to a collected slot is detected.
//!
//! The tracer is iterative. One `trace_children` walker defines
//! reachability for both the mark phase and `freeze`.

use lm_value::{ObjRef, Value};

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;
/// Logical byte cost of one object header.
const HEADER_COST: usize = 32;
/// Logical byte cost of one stored value.
const VALUE_COST: usize = 16;
/// Logical byte cost of one map entry (key and value).
const ENTRY_COST: usize = 2 * VALUE_COST;

/// The payload of one heap object.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// Immutable UTF-8 text. Born frozen.
    Str(String),
    /// A class instance. Fields follow the class layout. A field holds
    /// `Value::Uninit` before its first assignment.
    Instance { class: u32, fields: Vec<Value> },
    /// A growable list.
    List { items: Vec<Value> },
    /// A map with entries in insertion order.
    Map { entries: Vec<(Value, Value)> },
    /// A closure: code index plus captured values. Born frozen.
    Closure { func: u32, captures: Vec<Value> },
    /// A string builder.
    StrBuilder(String),
    /// A byte buffer.
    ByteBuf(Vec<u8>),
}

/// A native shape descriptor. The tracer and the printer read the
/// layout of each object kind through its shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeDesc {
    /// The stable display name of the shape.
    pub name: &'static str,
    /// True when the payload can hold object references.
    pub has_refs: bool,
    /// True when the object is frozen at birth.
    pub born_frozen: bool,
}

const SHAPE_STR: ShapeDesc = ShapeDesc {
    name: "String",
    has_refs: false,
    born_frozen: true,
};
const SHAPE_INSTANCE: ShapeDesc = ShapeDesc {
    name: "Instance",
    has_refs: true,
    born_frozen: false,
};
const SHAPE_LIST: ShapeDesc = ShapeDesc {
    name: "List",
    has_refs: true,
    born_frozen: false,
};
const SHAPE_MAP: ShapeDesc = ShapeDesc {
    name: "Map",
    has_refs: true,
    born_frozen: false,
};
const SHAPE_CLOSURE: ShapeDesc = ShapeDesc {
    name: "Closure",
    has_refs: true,
    born_frozen: true,
};
const SHAPE_SB: ShapeDesc = ShapeDesc {
    name: "StringBuilder",
    has_refs: false,
    born_frozen: false,
};
const SHAPE_BB: ShapeDesc = ShapeDesc {
    name: "ByteBuffer",
    has_refs: false,
    born_frozen: false,
};

impl Object {
    /// The shape descriptor for this object.
    pub fn shape(&self) -> &'static ShapeDesc {
        match self {
            Object::Str(_) => &SHAPE_STR,
            Object::Instance { .. } => &SHAPE_INSTANCE,
            Object::List { .. } => &SHAPE_LIST,
            Object::Map { .. } => &SHAPE_MAP,
            Object::Closure { .. } => &SHAPE_CLOSURE,
            Object::StrBuilder(_) => &SHAPE_SB,
            Object::ByteBuf(_) => &SHAPE_BB,
        }
    }

    /// The logical byte cost charged against the heap cap.
    pub fn cost(&self) -> usize {
        HEADER_COST
            + match self {
                Object::Str(s) => s.len(),
                Object::Instance { fields, .. } => fields.len() * VALUE_COST,
                Object::List { items } => items.len() * VALUE_COST,
                Object::Map { entries } => entries.len() * ENTRY_COST,
                Object::Closure { captures, .. } => captures.len() * VALUE_COST,
                Object::StrBuilder(s) => s.len(),
                Object::ByteBuf(b) => b.len(),
            }
    }

    /// Push every object reference inside this object onto `work`.
    /// This is the one shape walker for tracing and freezing.
    pub fn trace_children(&self, work: &mut Vec<ObjRef>) {
        let mut visit = |v: &Value| {
            if let Value::Obj(r) = v {
                work.push(*r);
            }
        };
        match self {
            Object::Str(_) | Object::StrBuilder(_) | Object::ByteBuf(_) => {}
            Object::Instance { fields, .. } => fields.iter().for_each(&mut visit),
            Object::List { items } => items.iter().for_each(&mut visit),
            Object::Map { entries } => {
                for (k, v) in entries {
                    visit(k);
                    visit(v);
                }
            }
            Object::Closure { captures, .. } => captures.iter().for_each(&mut visit),
        }
    }
}

/// One object header.
#[derive(Debug, Clone, Copy)]
struct Header {
    frozen: bool,
    marked: bool,
    /// The logical byte cost currently charged for the object.
    bytes: usize,
}

/// One object-table entry.
struct Entry {
    generation: u32,
    live: Option<(Header, Object)>,
}

/// Statistics of one heap, for `lm inspect --live`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    pub live: usize,
    pub slots: usize,
    pub pages: usize,
    pub free: usize,
    pub used_bytes: usize,
    pub cap_bytes: usize,
    pub collections: u64,
}

/// The VM heap.
pub struct Heap {
    pages: Vec<Vec<Entry>>,
    free: Vec<u32>,
    live: usize,
    used_bytes: usize,
    cap_bytes: usize,
    collections: u64,
    /// Host-registered roots. Push and pop in LIFO order.
    host_roots: Vec<ObjRef>,
}

impl Heap {
    pub fn new(cap_bytes: usize) -> Heap {
        Heap {
            pages: Vec::new(),
            free: Vec::new(),
            live: 0,
            used_bytes: 0,
            cap_bytes,
            collections: 0,
            host_roots: Vec::new(),
        }
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live: self.live,
            slots: self.pages.iter().map(Vec::len).sum(),
            pages: self.pages.len(),
            free: self.free.len(),
            used_bytes: self.used_bytes,
            cap_bytes: self.cap_bytes,
            collections: self.collections,
        }
    }

    pub fn live_count(&self) -> usize {
        self.live
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    pub fn collections(&self) -> u64 {
        self.collections
    }

    /// True when charging `cost` more bytes would exceed the cap.
    pub fn would_exceed(&self, cost: usize) -> bool {
        self.used_bytes + cost > self.cap_bytes
    }

    fn entry(&self, slot: u32) -> &Entry {
        &self.pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS]
    }

    fn entry_mut(&mut self, slot: u32) -> &mut Entry {
        &mut self.pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS]
    }

    /// Allocate one object. The caller must check the cap first with
    /// `would_exceed` and run a collection when needed.
    pub fn alloc(&mut self, object: Object) -> ObjRef {
        let cost = object.cost();
        let header = Header {
            frozen: object.shape().born_frozen,
            marked: false,
            bytes: cost,
        };
        self.used_bytes += cost;
        self.live += 1;
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                let need_page = self
                    .pages
                    .last()
                    .map(|p| p.len() == PAGE_SLOTS)
                    .unwrap_or(true);
                if need_page {
                    self.pages.push(Vec::with_capacity(PAGE_SLOTS));
                }
                let page_idx = self.pages.len() - 1;
                let page = &mut self.pages[page_idx];
                page.push(Entry {
                    generation: 0,
                    live: None,
                });
                (page_idx * PAGE_SLOTS + page.len() - 1) as u32
            }
        };
        let entry = self.entry_mut(slot);
        debug_assert!(entry.live.is_none());
        let generation = entry.generation;
        entry.live = Some((header, object));
        ObjRef { slot, generation }
    }

    /// Read an object. Return `None` for a stale or dead reference.
    pub fn try_get(&self, r: ObjRef) -> Option<&Object> {
        let entry = self.entry(r.slot);
        if entry.generation != r.generation {
            return None;
        }
        entry.live.as_ref().map(|(_, obj)| obj)
    }

    /// Read an object. The reference must be live and current.
    pub fn get(&self, r: ObjRef) -> &Object {
        self.try_get(r)
            .expect("object reference is live and generation-current")
    }

    /// Write access to an object. The caller must check the frozen bit
    /// first and must recompute the charged bytes after growth with
    /// `recharge`.
    pub fn get_mut(&mut self, r: ObjRef) -> &mut Object {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry
            .live
            .as_mut()
            .map(|(_, obj)| obj)
            .expect("object reference is live")
    }

    /// True when the object carries the frozen bit.
    pub fn is_frozen(&self, r: ObjRef) -> bool {
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.live.as_ref().map(|(h, _)| h.frozen).unwrap_or(false)
    }

    /// Update the charged byte count of one object after a mutation.
    pub fn recharge(&mut self, r: ObjRef) {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        let (header, object) = entry.live.as_mut().expect("live object");
        let new_cost = object.cost();
        let old_cost = header.bytes;
        header.bytes = new_cost;
        self.used_bytes = self.used_bytes - old_cost + new_cost;
    }

    /// Register a host root. Pop it later in LIFO order.
    pub fn push_host_root(&mut self, r: ObjRef) {
        self.host_roots.push(r);
    }

    /// Remove the most recent host root. The check is active in all
    /// build profiles: a mis-nested pop must fail loudly, because a
    /// silent wrong pop would unroot a live object.
    pub fn pop_host_root(&mut self, r: ObjRef) {
        let top = self.host_roots.pop();
        assert_eq!(top, Some(r), "host roots pop in LIFO order");
    }

    /// Deeply freeze the graph under `root`. The walk is iterative and
    /// preserves cycles and sharing. Freezing is monotonic: an already
    /// frozen object is complete, so the walk does not enter it again.
    pub fn freeze(&mut self, root: ObjRef) {
        let mut work = vec![root];
        while let Some(r) = work.pop() {
            let entry = self.entry_mut(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live.as_mut().expect("live object");
            if header.frozen {
                continue;
            }
            header.frozen = true;
            object.trace_children(&mut work);
        }
    }

    /// Collect garbage. `roots` holds every reachable entry point
    /// outside the heap; host roots are added inside. Marking uses an
    /// iterative worklist. Sweeping raises dead slot generations.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = ObjRef>) {
        self.collections += 1;
        // Mark.
        let mut work: Vec<ObjRef> = roots.into_iter().collect();
        work.extend(self.host_roots.iter().copied());
        while let Some(r) = work.pop() {
            let entry = self.entry_mut(r.slot);
            assert_eq!(entry.generation, r.generation, "stale root reference");
            let (header, _) = entry.live.as_mut().expect("root object is live");
            if header.marked {
                continue;
            }
            header.marked = true;
            let (_, object) = self.entry(r.slot).live.as_ref().expect("live object");
            object.trace_children(&mut work);
        }
        // Sweep.
        for (page_idx, page) in self.pages.iter_mut().enumerate() {
            for (idx, entry) in page.iter_mut().enumerate() {
                match &mut entry.live {
                    Some((header, _)) if header.marked => {
                        header.marked = false;
                    }
                    Some((header, _)) => {
                        self.used_bytes -= header.bytes;
                        self.live -= 1;
                        entry.live = None;
                        entry.generation = entry.generation.wrapping_add(1);
                        self.free.push((page_idx * PAGE_SLOTS + idx) as u32);
                    }
                    None => {}
                }
            }
        }
    }

    /// Visit every live object in slot order.
    pub fn for_each_live(&self, mut f: impl FnMut(ObjRef, bool, &Object)) {
        for (page_idx, page) in self.pages.iter().enumerate() {
            for (idx, entry) in page.iter().enumerate() {
                if let Some((header, object)) = &entry.live {
                    let r = ObjRef {
                        slot: (page_idx * PAGE_SLOTS + idx) as u32,
                        generation: entry.generation,
                    };
                    f(r, header.frozen, object);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_obj(text: &str) -> Object {
        Object::Str(text.to_string())
    }

    #[test]
    fn round_trips_objects() {
        let mut heap = Heap::new(1 << 20);
        let a = heap.alloc(str_obj("hello"));
        let b = heap.alloc(Object::List {
            items: vec![Value::Int(1)],
        });
        assert_eq!(heap.get(a), &str_obj("hello"));
        assert_eq!(
            heap.get(b),
            &Object::List {
                items: vec![Value::Int(1)]
            }
        );
        assert_eq!(heap.live_count(), 2);
    }

    #[test]
    fn strings_are_born_frozen_and_lists_are_not() {
        let mut heap = Heap::new(1 << 20);
        let s = heap.alloc(str_obj("x"));
        let l = heap.alloc(Object::List { items: vec![] });
        assert!(heap.is_frozen(s));
        assert!(!heap.is_frozen(l));
    }

    #[test]
    fn collect_reclaims_unreachable_objects() {
        let mut heap = Heap::new(1 << 20);
        let keep = heap.alloc(str_obj("keep"));
        let _drop1 = heap.alloc(str_obj("drop1"));
        let _drop2 = heap.alloc(str_obj("drop2"));
        assert_eq!(heap.live_count(), 3);
        heap.collect([keep]);
        assert_eq!(heap.live_count(), 1);
        assert_eq!(heap.get(keep), &str_obj("keep"));
    }

    #[test]
    fn collect_detects_stale_references() {
        let mut heap = Heap::new(1 << 20);
        let stale = heap.alloc(str_obj("gone"));
        heap.collect([]);
        assert_eq!(heap.try_get(stale), None);
        // The slot is reused with a new generation.
        let fresh = heap.alloc(str_obj("new"));
        assert_eq!(fresh.slot, stale.slot);
        assert_ne!(fresh.generation, stale.generation);
        assert_eq!(heap.try_get(stale), None);
        assert_eq!(heap.get(fresh), &str_obj("new"));
    }

    #[test]
    fn collect_keeps_cyclic_reachable_graphs_and_reclaims_cyclic_garbage() {
        let mut heap = Heap::new(1 << 20);
        // A two-node cycle rooted from outside.
        let a = heap.alloc(Object::List { items: vec![] });
        let b = heap.alloc(Object::List {
            items: vec![Value::Obj(a)],
        });
        if let Object::List { items } = heap.get_mut(a) {
            items.push(Value::Obj(b));
        }
        heap.recharge(a);
        // An unreachable two-node cycle.
        let c = heap.alloc(Object::List { items: vec![] });
        let d = heap.alloc(Object::List {
            items: vec![Value::Obj(c)],
        });
        if let Object::List { items } = heap.get_mut(c) {
            items.push(Value::Obj(d));
        }
        heap.recharge(c);
        assert_eq!(heap.live_count(), 4);
        heap.collect([a]);
        assert_eq!(heap.live_count(), 2);
        assert!(heap.try_get(a).is_some());
        assert!(heap.try_get(b).is_some());
        assert!(heap.try_get(c).is_none());
        assert!(heap.try_get(d).is_none());
    }

    #[test]
    fn collect_traces_deep_chains_iteratively() {
        // A 100,000-deep chain on a small Rust stack proves the tracer
        // and the freezer never recurse.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut heap = Heap::new(64 << 20);
                let mut head = heap.alloc(Object::List { items: vec![] });
                for _ in 0..100_000 {
                    head = heap.alloc(Object::List {
                        items: vec![Value::Obj(head)],
                    });
                }
                let baseline = heap.live_count();
                heap.collect([head]);
                assert_eq!(heap.live_count(), baseline);
                heap.freeze(head);
                assert!(heap.is_frozen(head));
                heap.collect([]);
                assert_eq!(heap.live_count(), 0);
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }

    #[test]
    fn host_roots_keep_objects_alive() {
        let mut heap = Heap::new(1 << 20);
        let rooted = heap.alloc(str_obj("rooted"));
        heap.push_host_root(rooted);
        heap.collect([]);
        assert_eq!(heap.get(rooted), &str_obj("rooted"));
        heap.pop_host_root(rooted);
        heap.collect([]);
        assert_eq!(heap.live_count(), 0);
    }

    #[test]
    fn freeze_preserves_sharing_and_cycles() {
        let mut heap = Heap::new(1 << 20);
        let shared = heap.alloc(Object::List { items: vec![] });
        let root = heap.alloc(Object::List {
            items: vec![Value::Obj(shared), Value::Obj(shared)],
        });
        if let Object::List { items } = heap.get_mut(shared) {
            items.push(Value::Obj(root));
        }
        heap.recharge(shared);
        heap.freeze(root);
        assert!(heap.is_frozen(root));
        assert!(heap.is_frozen(shared));
    }

    #[test]
    fn used_bytes_track_growth_and_collection() {
        let mut heap = Heap::new(1 << 20);
        let l = heap.alloc(Object::List { items: vec![] });
        let before = heap.used_bytes();
        if let Object::List { items } = heap.get_mut(l) {
            items.push(Value::Int(1));
        }
        heap.recharge(l);
        assert_eq!(heap.used_bytes(), before + 16);
        heap.collect([]);
        assert_eq!(heap.used_bytes(), 0);
    }

    #[test]
    fn slots_grow_in_pages() {
        let mut heap = Heap::new(64 << 20);
        for _ in 0..(PAGE_SLOTS + 1) {
            heap.alloc(Object::List { items: vec![] });
        }
        assert_eq!(heap.stats().pages, 2);
    }
}
