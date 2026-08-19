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
mod shared;

use lm_value::ObjRef;
#[cfg(test)]
use lm_value::Value;
pub use shape::{
    dump_shapes, BoundaryPolicy, MapIndex, Object, ShapeDesc, MIN_OBJECT_COST, SHAPES,
};
pub use shared::{
    process_lookup_hash, NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText,
};
use std::cell::Cell;
use std::rc::Rc;

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;

/// One object header.
#[derive(Debug, Clone, Copy)]
struct Header {
    frozen: bool,
    /// The logical byte cost currently charged for the object.
    bytes: usize,
    /// The shared immutable allocation charged through this object.
    shared: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct SharedCharge {
    capacity: usize,
    references: usize,
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

    /// Release storage that no longer matches the heap table.
    pub fn trim(&mut self, slots: usize) {
        self.seen.truncate(slots);
        self.ordinal.truncate(slots);
        self.seen.shrink_to(slots);
        self.ordinal.shrink_to(slots);
        self.order.clear();
        self.order.shrink_to(slots);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HeapBudgetState {
    bytes: usize,
    objects: usize,
}

/// One shared heap ledger for all machines in a world.
#[derive(Debug, Clone)]
pub struct HeapBudget {
    state: Rc<Cell<HeapBudgetState>>,
    max_bytes: usize,
    max_objects: usize,
}

impl HeapBudget {
    /// Create one ledger with byte and object limits.
    pub fn new(max_bytes: usize, max_objects: usize) -> HeapBudget {
        HeapBudget {
            state: Rc::new(Cell::new(HeapBudgetState::default())),
            max_bytes,
            max_objects,
        }
    }

    /// The logical bytes charged to this ledger.
    pub fn used_bytes(&self) -> usize {
        self.state.get().bytes
    }

    /// The live objects charged to this ledger.
    pub fn live_objects(&self) -> usize {
        self.state.get().objects
    }

    fn would_exceed(&self, bytes: usize, objects: usize) -> bool {
        let state = self.state.get();
        state
            .bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.max_bytes)
            || state
                .objects
                .checked_add(objects)
                .is_none_or(|total| total > self.max_objects)
    }

    /// Charge one allocation to the shared ledger.
    ///
    /// The caller tests `Heap::would_exceed`, `would_exceed_growth`, or
    /// `would_exceed_batch` first, and each of those reads this ledger.
    /// The rule is a debug assertion, because a release build runs this
    /// call once for each allocated object, and a second test of the
    /// same rule costs the allocation path a `Cell` read and two
    /// checked additions.
    ///
    /// A caller that skips its test over-charges the ledger instead of
    /// raising a fault. The debug build and the test suite catch that
    /// mistake.
    fn charge(&self, bytes: usize, objects: usize) {
        debug_assert!(
            !self.would_exceed(bytes, objects),
            "a checked heap charge fits the world budget"
        );
        let state = self.state.get();
        self.state.set(HeapBudgetState {
            bytes: state.bytes + bytes,
            objects: state.objects + objects,
        });
    }

    fn release(&self, bytes: usize, objects: usize) {
        let state = self.state.get();
        debug_assert!(state.bytes >= bytes);
        debug_assert!(state.objects >= objects);
        self.state.set(HeapBudgetState {
            bytes: state.bytes.saturating_sub(bytes),
            objects: state.objects.saturating_sub(objects),
        });
    }
}

/// A terminal heap compaction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactError {
    /// A required host allocation failed.
    Allocation,
    /// A live value names no live object.
    InvalidReference,
}

/// The VM heap.
pub struct Heap {
    pages: Vec<Vec<Entry>>,
    /// The latest generation of every slot ever allocated.
    generations: Vec<u32>,
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
    /// The aggregate ledger of the owning world.
    budget: Option<HeapBudget>,
    /// Shared immutable allocations referenced by this heap.
    shared_allocations: std::collections::HashMap<usize, SharedCharge>,
}

impl Heap {
    pub fn new(cap_bytes: usize) -> Heap {
        Heap::with_optional_budget(cap_bytes, None)
    }

    /// Create a heap that charges one proc-tree ledger.
    pub fn with_budget(cap_bytes: usize, budget: HeapBudget) -> Heap {
        Heap::with_optional_budget(cap_bytes, Some(budget))
    }

    fn with_optional_budget(cap_bytes: usize, budget: Option<HeapBudget>) -> Heap {
        Heap {
            pages: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            live: 0,
            used_bytes: 0,
            cap_bytes,
            collections: 0,
            host_roots: Vec::new(),
            scratch: GraphScratch::default(),
            digests: std::collections::HashMap::new(),
            budget,
            shared_allocations: std::collections::HashMap::new(),
        }
    }

    /// Attach one shared ledger to an existing local heap.
    ///
    /// This operation charges all live storage once. It changes
    /// nothing when the aggregate ledger cannot hold that storage.
    pub fn attach_budget(&mut self, budget: HeapBudget) -> bool {
        if self.budget.is_some() {
            return true;
        }
        if budget.would_exceed(self.used_bytes, self.live) {
            return false;
        }
        budget.charge(self.used_bytes, self.live);
        self.budget = Some(budget);
        true
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
        self.would_exceed_batch(cost, 1)
    }

    /// Get the incremental cost of one object allocation.
    pub fn allocation_cost(&self, object: &Object) -> usize {
        object.heap_base_cost()
            + object
                .shared_allocation()
                .filter(|(key, _)| !self.shared_allocations.contains_key(key))
                .map(|(_, capacity)| capacity)
                .unwrap_or(0)
    }

    fn add_shared(&mut self, shared: Option<(usize, usize)>) -> usize {
        let Some((key, capacity)) = shared else {
            return 0;
        };
        match self.shared_allocations.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                debug_assert_eq!(entry.get().capacity, capacity);
                entry.get_mut().references += 1;
                0
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(SharedCharge {
                    capacity,
                    references: 1,
                });
                capacity
            }
        }
    }

    fn remove_shared(&mut self, key: Option<usize>) -> usize {
        let Some(key) = key else {
            return 0;
        };
        let Some(charge) = self.shared_allocations.get_mut(&key) else {
            debug_assert!(false, "a shared allocation has a heap charge");
            return 0;
        };
        charge.references -= 1;
        if charge.references != 0 {
            return 0;
        }
        self.shared_allocations
            .remove(&key)
            .map(|charge| charge.capacity)
            .unwrap_or(0)
    }

    /// True when object growth would exceed a byte limit.
    pub fn would_exceed_growth(&self, cost: usize) -> bool {
        self.used_bytes
            .checked_add(cost)
            .is_none_or(|total| total > self.cap_bytes)
            || self
                .budget
                .as_ref()
                .is_some_and(|budget| budget.would_exceed(cost, 0))
    }

    /// True when one batch would exceed a heap limit.
    pub fn would_exceed_batch(&self, bytes: usize, objects: usize) -> bool {
        self.used_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.cap_bytes)
            || self.live.checked_add(objects).is_none()
            || self
                .budget
                .as_ref()
                .is_some_and(|budget| budget.would_exceed(bytes, objects))
    }

    fn entry(&self, slot: u32) -> &Entry {
        &self.pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS]
    }

    fn entry_mut(&mut self, slot: u32) -> &mut Entry {
        &mut self.pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS]
    }

    /// Read two distinct entries through one mutable heap borrow.
    fn two_entries_mut(&mut self, a: u32, b: u32) -> Option<(&mut Entry, &mut Entry)> {
        if a == b {
            return None;
        }
        let (a_page, a_slot) = (a as usize / PAGE_SLOTS, a as usize % PAGE_SLOTS);
        let (b_page, b_slot) = (b as usize / PAGE_SLOTS, b as usize % PAGE_SLOTS);
        if a_page == b_page {
            let page = self.pages.get_mut(a_page)?;
            if a_slot < b_slot {
                let (left, right) = page.split_at_mut(b_slot);
                return Some((left.get_mut(a_slot)?, right.first_mut()?));
            }
            let (left, right) = page.split_at_mut(a_slot);
            return Some((right.first_mut()?, left.get_mut(b_slot)?));
        }
        if a_page < b_page {
            let (left, right) = self.pages.split_at_mut(b_page);
            return Some((
                left.get_mut(a_page)?.get_mut(a_slot)?,
                right.first_mut()?.get_mut(b_slot)?,
            ));
        }
        let (left, right) = self.pages.split_at_mut(a_page);
        Some((
            right.first_mut()?.get_mut(a_slot)?,
            left.get_mut(b_page)?.get_mut(b_slot)?,
        ))
    }

    /// Append one string object to one distinct string builder.
    /// The caller reserves growth and recharges the builder.
    pub fn append_string(&mut self, builder: ObjRef, source: ObjRef) -> bool {
        let Some((builder_entry, source_entry)) = self.two_entries_mut(builder.slot, source.slot)
        else {
            return false;
        };
        if builder_entry.generation != builder.generation
            || source_entry.generation != source.generation
        {
            return false;
        }
        match (&mut builder_entry.live, &source_entry.live) {
            (Some((_, Object::StrBuilder(builder))), Some((_, Object::Str(source))))
            | (Some((_, Object::StrBuilder(builder))), Some((_, Object::Substring(source)))) => {
                builder.append(source)
            }
            _ => false,
        }
    }

    /// Allocate one object. The caller must check the cap first with
    /// `would_exceed` and run a collection when needed.
    pub fn alloc(&mut self, object: Object) -> ObjRef {
        let base = object.heap_base_cost();
        let shared = object.shared_allocation();
        let shared_key = shared.map(|(key, _)| key);
        let shared_cost = self.add_shared(shared);
        let cost = base + shared_cost;
        let header = Header {
            frozen: object.shape().born_frozen,
            bytes: base,
            shared: shared_key,
        };
        self.used_bytes += cost;
        self.live += 1;
        if let Some(budget) = &self.budget {
            budget.charge(cost, 1);
        }
        if let Some(slot) = self.free.pop() {
            let entry = self.entry_mut(slot);
            debug_assert!(entry.live.is_none());
            let generation = entry.generation;
            entry.live = Some((header, object));
            return ObjRef { slot, generation };
        }

        let need_page = self
            .pages
            .last()
            .map(|page| page.len() == PAGE_SLOTS)
            .unwrap_or(true);
        if need_page {
            self.pages.push(Vec::with_capacity(PAGE_SLOTS));
        }
        let page_idx = self.pages.len() - 1;
        let page = &mut self.pages[page_idx];
        let slot = page_idx * PAGE_SLOTS + page.len();
        debug_assert!(slot <= self.generations.len());
        let generation = if slot == self.generations.len() {
            self.generations.push(0);
            0
        } else {
            self.generations[slot]
        };
        page.push(Entry {
            generation,
            live: Some((header, object)),
        });
        ObjRef {
            slot: slot as u32,
            generation,
        }
    }

    /// Allocate one object with a fallible table reservation.
    pub fn try_alloc(&mut self, object: Object) -> Result<ObjRef, Object> {
        if self.would_exceed(self.allocation_cost(&object)) {
            return Err(object);
        }
        if self.free.is_empty() {
            let need_page = self
                .pages
                .last()
                .map(|page| page.len() == PAGE_SLOTS)
                .unwrap_or(true);
            if need_page {
                if self.pages.try_reserve(1).is_err() || self.generations.try_reserve(1).is_err() {
                    return Err(object);
                }
                let mut page = Vec::new();
                if page.try_reserve(1).is_err() {
                    return Err(object);
                }
                self.pages.push(page);
            } else if self
                .pages
                .last_mut()
                .expect("the last page exists")
                .try_reserve(1)
                .is_err()
                || self.generations.try_reserve(1).is_err()
            {
                return Err(object);
            }
        }
        Ok(self.alloc(object))
    }

    /// Read an object. Return `None` for a stale or dead reference.
    pub fn try_get(&self, r: ObjRef) -> Option<&Object> {
        let entry = self
            .pages
            .get(r.slot as usize / PAGE_SLOTS)?
            .get(r.slot as usize % PAGE_SLOTS)?;
        if entry.generation != r.generation {
            return None;
        }
        entry.live.as_ref().map(|(_, obj)| obj)
    }

    /// Read an object. The reference must be live and current.
    pub fn get(&self, r: ObjRef) -> &Object {
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry
            .live
            .as_ref()
            .map(|(_, object)| object)
            .expect("object reference is live")
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
        let (old_cost, old_shared, new_cost, new_shared) = {
            let entry = self.entry(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live.as_ref().expect("live object");
            (
                header.bytes,
                header.shared,
                object.heap_base_cost(),
                object.shared_allocation(),
            )
        };
        let new_shared_key = new_shared.map(|(key, _)| key);
        let (released, added) = if old_shared == new_shared_key {
            (0, 0)
        } else {
            (self.remove_shared(old_shared), self.add_shared(new_shared))
        };
        let entry = self.entry_mut(r.slot);
        let (header, _) = entry.live.as_mut().expect("live object");
        header.bytes = new_cost;
        header.shared = new_shared_key;
        self.used_bytes = self.used_bytes - old_cost - released + new_cost + added;
        if let Some(budget) = &self.budget {
            let old_total = old_cost + released;
            let new_total = new_cost + added;
            if new_total >= old_total {
                budget.charge(new_total - old_total, 0);
            } else {
                budget.release(old_total - new_total, 0);
            }
        }
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
        self.generations[slot as usize] = entry.generation;
        let shared = self.remove_shared(header.shared);
        let released = header.bytes + shared;
        self.used_bytes -= released;
        self.live -= 1;
        if let Some(budget) = &self.budget {
            budget.release(released, 1);
        }
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
        self.collections = self.collections.saturating_add(1);
        let mut freed_bytes = 0usize;
        let mut freed = 0usize;
        // Most heaps hold no digest at all. The lookup per freed slot
        // costs a hash, so the empty case skips it.
        let has_digests = !self.digests.is_empty();
        let shared_allocations = &mut self.shared_allocations;
        for (page_idx, page) in self.pages.iter_mut().enumerate() {
            for (idx, entry) in page.iter_mut().enumerate() {
                let slot = (page_idx * PAGE_SLOTS + idx) as u32;
                if entry.live.is_none() || keep(slot) {
                    continue;
                }
                let (header, _) = entry.live.take().expect("live object");
                freed_bytes += header.bytes;
                if let Some(key) = header.shared {
                    let charge = shared_allocations
                        .get_mut(&key)
                        .expect("a shared allocation has a heap charge");
                    charge.references -= 1;
                    if charge.references == 0 {
                        freed_bytes += shared_allocations
                            .remove(&key)
                            .expect("the shared allocation exists")
                            .capacity;
                    }
                }
                freed += 1;
                entry.generation = entry.generation.wrapping_add(1);
                self.generations[slot as usize] = entry.generation;
                self.free.push(slot);
                if has_digests {
                    self.digests.remove(&slot);
                }
            }
        }
        self.used_bytes -= freed_bytes;
        self.live -= freed;
        if let Some(budget) = &self.budget {
            budget.release(freed_bytes, freed);
        }
    }

    /// Release trailing empty pages and their work tables.
    pub fn trim_free_pages(&mut self) {
        while self
            .pages
            .last()
            .is_some_and(|page| page.iter().all(|entry| entry.live.is_none()))
        {
            self.pages.pop();
        }
        let slots = self.slot_count();
        self.free.retain(|slot| (*slot as usize) < slots);
        self.free.shrink_to_fit();
        self.scratch.trim(slots);
    }

    /// Rebuild all live objects into a dense table.
    ///
    /// The caller must collect unreachable objects first. `roots`
    /// contains each live reference stored outside the heap.
    /// Compaction also remaps registered host roots.
    pub fn compact_live(&mut self, roots: &mut [ObjRef]) -> Result<(), CompactError> {
        let mut order = Vec::new();
        order
            .try_reserve_exact(self.live)
            .map_err(|_| CompactError::Allocation)?;
        self.for_each_live(|reference, frozen, _| order.push((reference, frozen)));

        let mut mapping = std::collections::HashMap::new();
        mapping
            .try_reserve(order.len())
            .map_err(|_| CompactError::Allocation)?;
        for (slot, (reference, _)) in order.iter().enumerate() {
            let slot = u32::try_from(slot).map_err(|_| CompactError::Allocation)?;
            let generation = self
                .generations
                .get(slot as usize)
                .copied()
                .map(|held| held.wrapping_add(1))
                .unwrap_or(0);
            mapping.insert(*reference, ObjRef { slot, generation });
        }

        let map_roots = |source: &[ObjRef]| -> Result<Vec<ObjRef>, CompactError> {
            let mut mapped = Vec::new();
            mapped
                .try_reserve_exact(source.len())
                .map_err(|_| CompactError::Allocation)?;
            for reference in source {
                mapped.push(
                    mapping
                        .get(reference)
                        .copied()
                        .ok_or(CompactError::InvalidReference)?,
                );
            }
            Ok(mapped)
        };
        let mapped_roots = map_roots(roots)?;
        let mapped_host_roots = map_roots(&self.host_roots)?;

        let mut compacted = Heap::new(self.cap_bytes);
        for (reference, frozen) in &order {
            let mut missing = false;
            let object = self
                .get(*reference)
                .try_clone_remapped(|child| match mapping.get(&child).copied() {
                    Some(mapped) => mapped,
                    None => {
                        missing = true;
                        child
                    }
                })
                .map_err(|_| CompactError::Allocation)?;
            if missing {
                return Err(CompactError::InvalidReference);
            }
            let expected = mapping[reference];
            let mapped = compacted
                .try_alloc(object)
                .map_err(|_| CompactError::Allocation)?;
            if mapped.slot != expected.slot {
                return Err(CompactError::InvalidReference);
            }
            let entry = compacted.entry_mut(mapped.slot);
            entry.generation = expected.generation;
            compacted.generations[mapped.slot as usize] = expected.generation;
            if *frozen {
                compacted.set_frozen(expected);
            }
        }
        if compacted.live != self.live || compacted.used_bytes != self.used_bytes {
            return Err(CompactError::InvalidReference);
        }

        self.pages = std::mem::take(&mut compacted.pages);
        self.generations = std::mem::take(&mut compacted.generations);
        self.free = std::mem::take(&mut compacted.free);
        self.host_roots = mapped_host_roots;
        self.scratch = std::mem::take(&mut compacted.scratch);
        self.digests = std::mem::take(&mut compacted.digests);
        self.shared_allocations = std::mem::take(&mut compacted.shared_allocations);
        roots.copy_from_slice(&mapped_roots);
        Ok(())
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

impl Drop for Heap {
    fn drop(&mut self) {
        if let Some(budget) = &self.budget {
            budget.release(self.used_bytes, self.live);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_obj(text: &str) -> Object {
        Object::Str(text.into())
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
    fn immutable_text_clones_share_storage() {
        let source = SharedText::from("shared");
        let object = Object::Str(source.clone());
        let copied = object
            .try_clone_remapped(|reference| reference)
            .expect("the clone succeeds");
        let shell = object.shell().expect("String is sendable");
        for clone in [copied, shell] {
            let Object::Str(text) = clone else {
                panic!("the clone keeps its String shape");
            };
            assert!(source.shares_storage(&text));
        }
    }

    #[test]
    fn shared_text_and_bytes_charge_one_backing_allocation() {
        let mut heap = Heap::new(1 << 20);
        let text = SharedText::from("aé猫z");
        let view = text.scalar_slice(1, 2).expect("the scalar range is valid");
        let bytes = text.bytes();
        let string_base = Object::Str(text.clone()).heap_base_cost();
        let view_base = Object::Substring(view.clone()).heap_base_cost();
        let bytes_base = Object::Bytes(bytes.clone()).heap_base_cost();
        let capacity = text.retained_capacity();
        let pending_view = Object::Substring(view.clone());

        let string = heap.alloc(Object::Str(text));
        assert_eq!(heap.used_bytes(), string_base + capacity);
        assert_eq!(heap.allocation_cost(&pending_view), view_base);
        let substring = heap.alloc(Object::Substring(view));
        assert_eq!(heap.used_bytes(), string_base + view_base + capacity);
        let binary = heap.alloc(Object::Bytes(bytes));
        assert_eq!(
            heap.used_bytes(),
            string_base + view_base + bytes_base + capacity
        );

        heap.free(string);
        assert_eq!(heap.used_bytes(), view_base + bytes_base + capacity);
        heap.free(substring);
        assert_eq!(heap.used_bytes(), bytes_base + capacity);
        heap.free(binary);
        assert_eq!(heap.used_bytes(), 0);
        assert_eq!(heap.allocation_cost(&pending_view), view_base + capacity);
    }

    #[test]
    fn a_builder_appends_a_string_without_changing_the_source() {
        let mut heap = Heap::new(1 << 20);
        let builder = heap.alloc(Object::StrBuilder(NativeStringBuilder::from_string(
            "a".to_string(),
        )));
        let source = heap.alloc(str_obj("bc"));

        assert!(heap.append_string(builder, source));
        heap.recharge(builder);
        assert_eq!(
            heap.get(builder),
            &Object::StrBuilder(NativeStringBuilder::from_string("abc".to_string()))
        );
        assert_eq!(heap.get(source), &str_obj("bc"));
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

    #[test]
    fn two_heaps_charge_one_shared_budget() {
        let cost = str_obj("one").cost();
        let budget = HeapBudget::new(cost, 1);
        let mut first = Heap::with_budget(1 << 20, budget.clone());
        let second = Heap::with_budget(1 << 20, budget.clone());
        assert!(!first.would_exceed(cost));
        let value = first.alloc(str_obj("one"));
        assert_eq!(budget.used_bytes(), cost);
        assert!(second.would_exceed(cost));
        first.free(value);
        assert_eq!(budget.used_bytes(), 0);
        assert!(!second.would_exceed(cost));
    }

    #[test]
    fn trimmed_slots_keep_their_generation() {
        let mut heap = Heap::new(1 << 20);
        let stale = heap.alloc(str_obj("old"));
        heap.sweep(|_| false);
        heap.trim_free_pages();
        assert_eq!(heap.slot_count(), 0);
        assert_eq!(heap.try_get(stale), None);
        let fresh = heap.alloc(str_obj("new"));
        assert_eq!(fresh.slot, stale.slot);
        assert_ne!(fresh.generation, stale.generation);
        assert_eq!(heap.try_get(stale), None);
    }

    #[test]
    fn terminal_compaction_removes_dead_slot_storage() {
        let budget = HeapBudget::new(1 << 20, 2048);
        let mut heap = Heap::with_budget(1 << 20, budget.clone());
        for _ in 0..1500 {
            heap.alloc(str_obj("dead"));
        }
        let mut root = heap.alloc(str_obj("live"));
        heap.sweep(|slot| slot == root.slot);
        assert_eq!(heap.live_count(), 1);
        assert!(heap.slot_count() > 1);
        let bytes = heap.used_bytes();
        heap.compact_live(std::slice::from_mut(&mut root))
            .expect("the live object compacts");
        assert_eq!(root.slot, 0);
        assert_eq!(heap.slot_count(), 1);
        assert_eq!(heap.used_bytes(), bytes);
        assert_eq!(budget.used_bytes(), bytes);
        assert_eq!(heap.get(root), &str_obj("live"));
    }

    #[test]
    fn terminal_compaction_does_not_revive_a_stale_reference() {
        let mut heap = Heap::new(1 << 20);
        let stale = heap.alloc(str_obj("old"));
        heap.free(stale);
        let mut root = heap.alloc(str_obj("live"));
        heap.compact_live(std::slice::from_mut(&mut root))
            .expect("the live object compacts");
        assert_eq!(heap.try_get(stale), None);
        assert_ne!(root.generation, stale.generation);
        assert_eq!(heap.get(root), &str_obj("live"));
    }
}
