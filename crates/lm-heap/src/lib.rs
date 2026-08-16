//! The per-VM heap: object table, allocation pages, native shapes,
//! and the reusable graph work tables.
//!
//! Objects live in a slot table that grows in fixed-size pages, so an
//! entry never moves. A reference is a `(slot, generation)` pair. A
//! freed slot raises its generation, so a stale reference to a
//! collected slot is detected.
//!
//! This crate holds storage and shape declarations only. Reachability
//! belongs to `lm-graph`: mark, freeze, transfer, copy, digest,
//! verification, inspection, and snapshot traversal all run there over
//! one traversal engine.

pub mod shape;

use lm_value::ObjRef;
#[cfg(test)]
use lm_value::Value;
pub use shape::{dump_shapes, BoundaryPolicy, MapIndex, Object, ShapeDesc, SHAPES};

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;

/// One object header.
#[derive(Debug, Clone, Copy)]
struct Header {
    frozen: bool,
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

/// The reusable graph work tables of one heap.
///
/// `seen` holds the walk epoch that last reached each slot, so a new
/// walk needs no clearing pass. `ordinal` holds the canonical
/// traversal ordinal of each reached slot. Both tables are one entry
/// per object-table slot, so the work table is bounded by the heap
/// itself and never by the graph shape.
#[derive(Debug, Default)]
pub struct GraphScratch {
    epoch: u32,
    seen: Vec<u32>,
    ordinal: Vec<u32>,
    /// Reached objects in canonical first-encounter order. The index
    /// is the traversal ordinal.
    order: Vec<ObjRef>,
}

impl GraphScratch {
    /// Start one walk over a table of `slots` slots. The epoch step
    /// invalidates the previous walk without touching the tables.
    pub fn begin(&mut self, slots: usize) {
        if self.seen.len() < slots {
            self.seen.resize(slots, 0);
            self.ordinal.resize(slots, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            // The epoch wrapped. One clearing pass restarts the
            // sequence; it happens once every four billion walks.
            self.seen.iter_mut().for_each(|slot| *slot = 0);
            self.epoch = 1;
        }
        self.order.clear();
    }

    /// True when the current walk already reached this slot.
    #[inline]
    pub fn seen(&self, slot: u32) -> bool {
        self.seen[slot as usize] == self.epoch
    }

    /// Record the first encounter of one slot and return its
    /// canonical traversal ordinal.
    #[inline]
    pub fn record(&mut self, r: ObjRef) -> u32 {
        let ordinal = self.order.len() as u32;
        self.seen[r.slot as usize] = self.epoch;
        self.ordinal[r.slot as usize] = ordinal;
        self.order.push(r);
        ordinal
    }

    /// The canonical traversal ordinal of one reached slot.
    #[inline]
    pub fn ordinal(&self, slot: u32) -> u32 {
        debug_assert!(self.seen(slot), "the walk reached this slot");
        self.ordinal[slot as usize]
    }

    /// The reached objects in canonical first-encounter order.
    pub fn order(&self) -> &[ObjRef] {
        &self.order
    }
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
    /// The reusable graph work tables. `lm-graph` borrows them for
    /// the length of one walk and returns them afterwards.
    scratch: GraphScratch,
    /// The canonical digest of frozen objects, keyed by slot. A
    /// frozen object never changes, so an entry stays valid until the
    /// slot is freed.
    digests: std::collections::HashMap<u32, ([u8; 32], u32)>,
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
            scratch: GraphScratch::default(),
            digests: std::collections::HashMap::new(),
        }
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live: self.live,
            slots: self.slot_count(),
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

    /// The number of object-table slots, live and free.
    pub fn slot_count(&self) -> usize {
        self.pages.iter().map(Vec::len).sum()
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

    /// Set the frozen bit of one object. Freezing is monotonic, and
    /// only `lm-graph` calls this after a whole graph validates.
    pub fn set_frozen(&mut self, r: ObjRef) {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        let (header, _) = entry.live.as_mut().expect("live object");
        header.frozen = true;
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

    /// Free one live object now.
    ///
    /// A failed transfer calls this to roll back the shells it
    /// allocated, so the destination heap keeps its earlier live
    /// count and byte count.
    pub fn free(&mut self, r: ObjRef) {
        let slot = r.slot;
        let entry = self.entry_mut(slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        let (header, _) = entry.live.take().expect("live object");
        entry.generation = entry.generation.wrapping_add(1);
        self.used_bytes -= header.bytes;
        self.live -= 1;
        self.free.push(slot);
        self.digests.remove(&slot);
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

    /// The registered host roots.
    pub fn host_roots(&self) -> &[ObjRef] {
        &self.host_roots
    }

    /// Borrow the graph work tables for the length of one walk.
    ///
    /// The heap keeps an empty table meanwhile, so a walk started
    /// inside another walk allocates its own table instead of sharing
    /// one.
    pub fn take_scratch(&mut self) -> GraphScratch {
        std::mem::take(&mut self.scratch)
    }

    /// Return the graph work tables after one walk.
    pub fn put_scratch(&mut self, scratch: GraphScratch) {
        self.scratch = scratch;
    }

    /// The cached canonical digest of one frozen object.
    pub fn cached_digest(&self, r: ObjRef) -> Option<[u8; 32]> {
        match self.digests.get(&r.slot) {
            Some((digest, generation)) if *generation == r.generation => Some(*digest),
            _ => None,
        }
    }

    /// Cache the canonical digest of one frozen object. A frozen
    /// object never changes, so the entry stays valid until the slot
    /// is freed.
    pub fn cache_digest(&mut self, r: ObjRef, digest: [u8; 32]) {
        debug_assert!(self.is_frozen(r), "only a frozen object caches a digest");
        self.digests.insert(r.slot, (digest, r.generation));
    }

    /// The number of cached digests, for the cache tests.
    pub fn digest_cache_len(&self) -> usize {
        self.digests.len()
    }

    /// Free every live slot that `keep` rejects, and raise the
    /// generation of each freed slot.
    ///
    /// `lm-graph` marks the reachable set first and passes the test
    /// here. The heap never decides reachability.
    pub fn sweep(&mut self, keep: impl Fn(u32) -> bool) {
        self.collections += 1;
        let mut freed_bytes = 0usize;
        let mut freed = 0usize;
        // Most heaps hold no digest at all. The lookup per freed slot
        // costs a hash, so the empty case skips it.
        let has_digests = !self.digests.is_empty();
        for (page_idx, page) in self.pages.iter_mut().enumerate() {
            for (idx, entry) in page.iter_mut().enumerate() {
                let slot = (page_idx * PAGE_SLOTS + idx) as u32;
                if entry.live.is_none() || keep(slot) {
                    continue;
                }
                let (header, _) = entry.live.take().expect("live object");
                freed_bytes += header.bytes;
                freed += 1;
                entry.generation = entry.generation.wrapping_add(1);
                self.free.push(slot);
                if has_digests {
                    self.digests.remove(&slot);
                }
            }
        }
        self.used_bytes -= freed_bytes;
        self.live -= freed;
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
    fn freeing_one_object_restores_the_earlier_counts() {
        let mut heap = Heap::new(1 << 20);
        let before_live = heap.live_count();
        let before_bytes = heap.used_bytes();
        let r = heap.alloc(str_obj("gone"));
        assert_ne!(heap.used_bytes(), before_bytes);
        heap.free(r);
        assert_eq!(heap.live_count(), before_live);
        assert_eq!(heap.used_bytes(), before_bytes);
        assert_eq!(heap.try_get(r), None);
    }

    #[test]
    fn used_bytes_track_growth() {
        let mut heap = Heap::new(1 << 20);
        let l = heap.alloc(Object::List { items: vec![] });
        let before = heap.used_bytes();
        if let Object::List { items } = heap.get_mut(l) {
            items.push(Value::Int(1));
        }
        heap.recharge(l);
        assert_eq!(heap.used_bytes(), before + 16);
    }

    #[test]
    fn slots_grow_in_pages() {
        let mut heap = Heap::new(64 << 20);
        for _ in 0..(PAGE_SLOTS + 1) {
            heap.alloc(Object::List { items: vec![] });
        }
        assert_eq!(heap.stats().pages, 2);
    }

    #[test]
    fn the_scratch_epoch_separates_two_walks() {
        let mut scratch = GraphScratch::default();
        scratch.begin(4);
        let a = ObjRef {
            slot: 1,
            generation: 0,
        };
        assert!(!scratch.seen(a.slot));
        assert_eq!(scratch.record(a), 0);
        assert!(scratch.seen(a.slot));
        scratch.begin(4);
        assert!(!scratch.seen(a.slot));
        assert!(scratch.order().is_empty());
    }

    #[test]
    fn the_digest_cache_follows_the_slot_generation() {
        let mut heap = Heap::new(1 << 20);
        let r = heap.alloc(str_obj("cached"));
        assert_eq!(heap.cached_digest(r), None);
        heap.cache_digest(r, [7; 32]);
        assert_eq!(heap.cached_digest(r), Some([7; 32]));
        heap.free(r);
        let fresh = heap.alloc(str_obj("fresh"));
        assert_eq!(fresh.slot, r.slot);
        assert_eq!(heap.cached_digest(fresh), None);
        assert_eq!(heap.digest_cache_len(), 0);
    }
}
