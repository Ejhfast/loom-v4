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
    dump_shapes, BoundaryPolicy, CodeHandleKind, FaultSite, MapEntry, MapIndex, Object,
    PortableCode, PortableCodeKind, ShapeDesc, SlotChangeKind, StructuralEpoch, MIN_OBJECT_COST,
    SHAPES,
};
pub use shared::{
    process_lookup_hash, NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText,
};
use std::hash::{BuildHasherDefault, Hasher};

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;
/// The first collection point for a heap with a larger hard limit.
const INITIAL_COLLECTION_BYTES: usize = 4 << 20;

/// One dead or unused JIT object slot.
pub const JIT_OBJECT_DEAD: u16 = 0;
/// One ordinary class instance.
pub const JIT_OBJECT_INSTANCE: u16 = 1;
/// One growable list.
pub const JIT_OBJECT_LIST: u16 = 2;
/// One immutable tuple.
pub const JIT_OBJECT_TUPLE: u16 = 3;
/// One object that requires a typed runtime slow path.
pub const JIT_OBJECT_OPAQUE: u16 = 4;
/// The object cannot accept a native store.
pub const JIT_OBJECT_FROZEN: u16 = 1;
/// One object kind without a runtime class.
pub const JIT_NO_CLASS: u32 = u32::MAX;
/// Shift from an object slot to its JIT page index.
pub const JIT_PAGE_SHIFT: u32 = 10;
/// Mask from an object slot to its JIT page offset.
pub const JIT_PAGE_MASK: u32 = (PAGE_SLOTS as u32) - 1;

/// Fixed native view of one object-table slot.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JitObjectEntry {
    pub generation: u32,
    pub kind: u16,
    pub flags: u16,
    pub class: u32,
    pub len: usize,
    pub data: usize,
}

impl JitObjectEntry {
    fn dead(generation: u32) -> JitObjectEntry {
        JitObjectEntry {
            generation,
            kind: JIT_OBJECT_DEAD,
            flags: 0,
            class: JIT_NO_CLASS,
            len: 0,
            data: 0,
        }
    }

    fn live(generation: u32, header: Header, object: &Object) -> JitObjectEntry {
        let flags = if header.frozen { JIT_OBJECT_FROZEN } else { 0 };
        match object {
            Object::Instance { class, fields, .. } => JitObjectEntry {
                generation,
                kind: JIT_OBJECT_INSTANCE,
                flags,
                class: *class,
                len: fields.len(),
                data: slice_address(fields),
            },
            Object::List { items, .. } => JitObjectEntry {
                generation,
                kind: JIT_OBJECT_LIST,
                flags,
                class: JIT_NO_CLASS,
                len: items.len(),
                data: slice_address(items),
            },
            Object::Tuple { items } => JitObjectEntry {
                generation,
                kind: JIT_OBJECT_TUPLE,
                flags,
                class: JIT_NO_CLASS,
                len: items.len(),
                data: slice_address(items),
            },
            _ => JitObjectEntry {
                generation,
                kind: JIT_OBJECT_OPAQUE,
                flags,
                class: JIT_NO_CLASS,
                len: 0,
                data: 0,
            },
        }
    }
}

fn slice_address(values: &[lm_value::Value]) -> usize {
    if values.is_empty() {
        0
    } else {
        values.as_ptr() as usize
    }
}

/// Borrowed native view of one heap revision.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JitHeapView {
    pub pages: *const usize,
    pub page_count: usize,
    pub slot_count: usize,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<JitObjectEntry>() == 32);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::align_of::<JitObjectEntry>() == 8);
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, generation) == 0);
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, kind) == 4);
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, flags) == 6);
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, class) == 8);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, len) == 16);
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::offset_of!(JitObjectEntry, data) == 24);

fn next_collection_threshold(used_bytes: usize, cap_bytes: usize) -> usize {
    let live_target = used_bytes.saturating_mul(2);
    cap_bytes.min(INITIAL_COLLECTION_BYTES.max(live_target))
}

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

/// Hash trusted allocation addresses for the shared-storage ledger.
#[derive(Default)]
struct AllocationHasher(u64);

impl Hasher for AllocationHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }

    fn write_usize(&mut self, value: usize) {
        let mut hash = value as u64;
        hash ^= hash >> 30;
        hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 27;
        hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
        self.0 = hash ^ (hash >> 31);
    }
}

type SharedAllocations =
    std::collections::HashMap<usize, SharedCharge, BuildHasherDefault<AllocationHasher>>;

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

/// A terminal heap compaction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactError {
    /// A required host allocation failed.
    Allocation,
    /// A live value names no live object.
    InvalidReference,
}

struct DigestCacheEntry {
    generation: u32,
    values: Vec<(Option<u32>, [u8; 32])>,
}

/// The VM heap.
pub struct Heap {
    pages: Vec<Vec<Entry>>,
    /// Fixed-layout native side records for each object page.
    jit_pages: Vec<Vec<JitObjectEntry>>,
    /// Stable addresses of the fixed-layout side-record pages.
    jit_page_addresses: Vec<usize>,
    /// Slots that need a side-record refresh before native entry.
    jit_dirty: Vec<u32>,
    /// One bit for each slot already present in `jit_dirty`.
    jit_dirty_bits: Vec<u64>,
    /// The latest generation of every slot ever allocated.
    generations: Vec<u32>,
    free: Vec<u32>,
    live: usize,
    used_bytes: usize,
    cap_bytes: usize,
    /// The next local byte count that requests a collection.
    collection_threshold: usize,
    collections: u64,
    /// Host-registered roots. Push and pop in LIFO order.
    host_roots: Vec<ObjRef>,
    /// The reusable graph work tables. `lm-graph` borrows them for
    /// the length of one walk and returns them afterwards.
    scratch: GraphScratch,
    /// Canonical digests of frozen objects, keyed by slot and type.
    digests: std::collections::HashMap<u32, DigestCacheEntry>,
    /// Shared immutable allocations referenced by this heap.
    shared_allocations: SharedAllocations,
}

impl Heap {
    pub fn new(cap_bytes: usize) -> Heap {
        Heap {
            pages: Vec::new(),
            jit_pages: Vec::new(),
            jit_page_addresses: Vec::new(),
            jit_dirty: Vec::new(),
            jit_dirty_bits: Vec::new(),
            generations: Vec::new(),
            free: Vec::new(),
            live: 0,
            used_bytes: 0,
            cap_bytes,
            collection_threshold: cap_bytes.min(INITIAL_COLLECTION_BYTES),
            collections: 0,
            host_roots: Vec::new(),
            scratch: GraphScratch::default(),
            digests: std::collections::HashMap::new(),
            shared_allocations: SharedAllocations::default(),
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

    /// Return a synchronized native view of the object table.
    pub fn jit_view(&mut self) -> JitHeapView {
        self.sync_jit_entries();
        JitHeapView {
            pages: self.jit_page_addresses.as_ptr(),
            page_count: self.jit_page_addresses.len(),
            slot_count: self.slot_count(),
        }
    }

    /// Synchronize side records after Rust object mutations.
    pub fn sync_jit_entries(&mut self) {
        while let Some(slot) = self.jit_dirty.pop() {
            let word = slot as usize / 64;
            let bit = slot as usize % 64;
            self.jit_dirty_bits[word] &= !(1u64 << bit);
            self.refresh_jit_entry(slot);
        }
    }

    /// Return one synchronized side record.
    pub fn jit_entry(&mut self, slot: u32) -> Option<JitObjectEntry> {
        if slot as usize >= self.slot_count() {
            return None;
        }
        self.sync_jit_entries();
        self.jit_pages
            .get(slot as usize / PAGE_SLOTS)
            .and_then(|page| page.get(slot as usize % PAGE_SLOTS))
            .copied()
    }

    fn add_jit_page(&mut self) {
        self.jit_dirty.reserve(PAGE_SLOTS);
        self.jit_dirty_bits
            .resize(self.jit_pages.len() * 16 + 16, 0);
        let page = vec![JitObjectEntry::dead(0); PAGE_SLOTS];
        let address = page.as_ptr() as usize;
        self.jit_pages.push(page);
        self.jit_page_addresses.push(address);
    }

    fn try_add_page(&mut self) -> bool {
        if self.pages.try_reserve(1).is_err()
            || self.jit_pages.try_reserve(1).is_err()
            || self.jit_page_addresses.try_reserve(1).is_err()
            || self.jit_dirty.try_reserve(PAGE_SLOTS).is_err()
            || self.jit_dirty_bits.try_reserve(16).is_err()
        {
            return false;
        }
        let mut page = Vec::new();
        if page.try_reserve(1).is_err() {
            return false;
        }
        let mut jit_page = Vec::new();
        if jit_page.try_reserve_exact(PAGE_SLOTS).is_err() {
            return false;
        }
        jit_page.resize(PAGE_SLOTS, JitObjectEntry::dead(0));
        let address = jit_page.as_ptr() as usize;
        self.pages.push(page);
        self.jit_pages.push(jit_page);
        self.jit_page_addresses.push(address);
        self.jit_dirty_bits.resize(self.jit_pages.len() * 16, 0);
        true
    }

    fn mark_jit_dirty(&mut self, slot: u32) {
        let word = slot as usize / 64;
        let bit = slot as usize % 64;
        debug_assert!(word < self.jit_dirty_bits.len());
        let mask = 1u64 << bit;
        if self.jit_dirty_bits[word] & mask == 0 {
            self.jit_dirty_bits[word] |= mask;
            self.jit_dirty.push(slot);
        }
    }

    fn refresh_jit_entry(&mut self, slot: u32) {
        let record = {
            let entry = self.entry(slot);
            match entry.live.as_ref() {
                Some((header, object)) => JitObjectEntry::live(entry.generation, *header, object),
                None => JitObjectEntry::dead(entry.generation),
            }
        };
        self.jit_pages[slot as usize / PAGE_SLOTS][slot as usize % PAGE_SLOTS] = record;
    }

    /// True when charging `cost` more bytes would exceed the cap.
    pub fn would_exceed(&self, cost: usize) -> bool {
        self.would_exceed_batch(cost, 1)
    }

    /// Test whether local growth reached the next collection point.
    pub fn collection_due(&self, growth: usize) -> bool {
        self.used_bytes
            .checked_add(growth)
            .is_none_or(|total| total > self.collection_threshold)
    }

    fn set_next_collection_threshold(&mut self) {
        self.collection_threshold = next_collection_threshold(self.used_bytes, self.cap_bytes);
    }

    /// Get the incremental cost of one object allocation.
    pub fn allocation_cost(&self, object: &Object) -> usize {
        object.heap_base_cost()
            + object
                .shared_allocation()
                .filter(|(key, _)| {
                    object.shared_allocation_is_unique()
                        || !self.shared_allocations.contains_key(key)
                })
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
    }

    /// True when one batch would exceed a heap limit.
    pub fn would_exceed_batch(&self, bytes: usize, objects: usize) -> bool {
        self.used_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > self.cap_bytes)
            || self.live.checked_add(objects).is_none()
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
        if let Some(slot) = self.free.pop() {
            let entry = self.entry_mut(slot);
            debug_assert!(entry.live.is_none());
            let generation = entry.generation;
            entry.live = Some((header, object));
            self.refresh_jit_entry(slot);
            return ObjRef { slot, generation };
        }

        let need_page = self
            .pages
            .last()
            .map(|page| page.len() == PAGE_SLOTS)
            .unwrap_or(true);
        if need_page {
            self.pages.push(Vec::with_capacity(PAGE_SLOTS));
            self.add_jit_page();
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
        self.refresh_jit_entry(slot as u32);
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
                if self.generations.try_reserve(1).is_err() || !self.try_add_page() {
                    return Err(object);
                }
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
        self.mark_jit_dirty(r.slot);
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
        self.refresh_jit_entry(r.slot);
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
        self.refresh_jit_entry(r.slot);
    }

    /// Update one object that has no shared allocation.
    pub fn recharge_local(&mut self, r: ObjRef) {
        let (old_cost, new_cost) = {
            let entry = self.entry_mut(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live.as_mut().expect("live object");
            debug_assert!(header.shared.is_none());
            debug_assert!(object.shared_allocation().is_none());
            let old_cost = header.bytes;
            let new_cost = object.heap_base_cost();
            header.bytes = new_cost;
            (old_cost, new_cost)
        };
        self.used_bytes = self.used_bytes - old_cost + new_cost;
        self.refresh_jit_entry(r.slot);
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
        self.free.push(slot);
        self.digests.remove(&slot);
        self.refresh_jit_entry(slot);
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
        self.cached_digest_for(r, None)
    }

    /// The cached digest of one frozen object under one static type.
    pub fn cached_typed_digest(&self, r: ObjRef, ty: u32) -> Option<[u8; 32]> {
        self.cached_digest_for(r, Some(ty))
    }

    fn cached_digest_for(&self, r: ObjRef, ty: Option<u32>) -> Option<[u8; 32]> {
        match self.digests.get(&r.slot) {
            Some(entry) if entry.generation == r.generation => entry
                .values
                .iter()
                .find_map(|(held, digest)| (*held == ty).then_some(*digest)),
            _ => None,
        }
    }

    /// Cache the canonical digest of one frozen object. A frozen
    /// object never changes, so the entry stays valid until the slot
    /// is freed.
    pub fn cache_digest(&mut self, r: ObjRef, digest: [u8; 32]) {
        self.cache_digest_for(r, None, digest);
    }

    /// Cache one canonical digest under its static root type.
    pub fn cache_typed_digest(&mut self, r: ObjRef, ty: u32, digest: [u8; 32]) {
        self.cache_digest_for(r, Some(ty), digest);
    }

    fn cache_digest_for(&mut self, r: ObjRef, ty: Option<u32>, digest: [u8; 32]) {
        debug_assert!(self.is_frozen(r), "only a frozen object caches a digest");
        let entry = self
            .digests
            .entry(r.slot)
            .or_insert_with(|| DigestCacheEntry {
                generation: r.generation,
                values: Vec::new(),
            });
        if entry.generation != r.generation {
            entry.generation = r.generation;
            entry.values.clear();
        }
        if let Some((_, held)) = entry.values.iter_mut().find(|(held, _)| *held == ty) {
            *held = digest;
        } else {
            entry.values.push((ty, digest));
        }
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
        let jit_pages = &mut self.jit_pages;
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
                jit_pages[page_idx][idx] = JitObjectEntry::dead(entry.generation);
                self.free.push(slot);
                if has_digests {
                    self.digests.remove(&slot);
                }
            }
        }
        self.used_bytes -= freed_bytes;
        self.live -= freed;
        self.set_next_collection_threshold();
    }

    /// Release trailing empty pages and their work tables.
    pub fn trim_free_pages(&mut self) {
        while self
            .pages
            .last()
            .is_some_and(|page| page.iter().all(|entry| entry.live.is_none()))
        {
            self.pages.pop();
            self.jit_pages.pop();
            self.jit_page_addresses.pop();
        }
        let slots = self.slot_count();
        self.free.retain(|slot| (*slot as usize) < slots);
        self.free.shrink_to_fit();
        self.jit_dirty.retain(|slot| (*slot as usize) < slots);
        self.jit_dirty.shrink_to(slots);
        self.jit_dirty_bits.truncate(self.jit_pages.len() * 16);
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
            } else {
                compacted.refresh_jit_entry(expected.slot);
            }
        }
        if compacted.live != self.live || compacted.used_bytes != self.used_bytes {
            return Err(CompactError::InvalidReference);
        }

        self.pages = std::mem::take(&mut compacted.pages);
        self.jit_pages = std::mem::take(&mut compacted.jit_pages);
        self.jit_page_addresses = std::mem::take(&mut compacted.jit_page_addresses);
        self.jit_dirty = std::mem::take(&mut compacted.jit_dirty);
        self.jit_dirty_bits = std::mem::take(&mut compacted.jit_dirty_bits);
        self.generations = std::mem::take(&mut compacted.generations);
        self.free = std::mem::take(&mut compacted.free);
        self.host_roots = mapped_host_roots;
        self.scratch = std::mem::take(&mut compacted.scratch);
        self.digests = std::mem::take(&mut compacted.digests);
        self.shared_allocations = std::mem::take(&mut compacted.shared_allocations);
        self.set_next_collection_threshold();
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
            epoch: Default::default(),
        });
        assert_eq!(heap.get(a), &str_obj("hello"));
        assert_eq!(
            heap.get(b),
            &Object::List {
                items: vec![Value::Int(1)],
                epoch: Default::default(),
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
    fn text_views_from_bytes_charge_one_backing_allocation() {
        let mut heap = Heap::new(1 << 20);
        let bytes = SharedBytes::from("aé猫z".as_bytes());
        let text = bytes.utf8_view().expect("the bytes contain UTF-8");
        let view = text.scalar_slice(1, 2).expect("the scalar range is valid");
        let text_base = Object::Str(text.clone()).heap_base_cost();
        let view_base = Object::Substring(view.clone()).heap_base_cost();
        let capacity = text.retained_capacity();

        let string = heap.alloc(Object::Str(text));
        assert_eq!(heap.used_bytes(), text_base + capacity);
        assert_eq!(
            heap.allocation_cost(&Object::Substring(view.clone())),
            view_base
        );
        let substring = heap.alloc(Object::Substring(view));
        assert_eq!(heap.used_bytes(), text_base + view_base + capacity);

        heap.free(string);
        assert_eq!(heap.used_bytes(), view_base + capacity);
        heap.free(substring);
        assert_eq!(heap.used_bytes(), 0);
    }

    #[test]
    fn a_builder_appends_a_string_without_changing_the_source() {
        let mut heap = Heap::new(1 << 20);
        let builder = heap.alloc(Object::StrBuilder(NativeStringBuilder::from_string(
            "a".to_string(),
        )));
        let source = heap.alloc(str_obj("bc"));

        assert!(heap.append_string(builder, source));
        heap.recharge_local(builder);
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
        let l = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        assert!(heap.is_frozen(s));
        assert!(!heap.is_frozen(l));
    }

    #[test]
    fn jit_instance_records_expose_the_canonical_field_array() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Instance {
            class: 17,
            fields: vec![Value::Int(4), Value::Bool(true)],
            env: lm_value::Witness::EMPTY,
        });
        let record = heap.jit_entry(reference.slot).expect("the slot exists");
        assert_eq!(record.generation, reference.generation);
        assert_eq!(record.kind, JIT_OBJECT_INSTANCE);
        assert_eq!(record.flags, 0);
        assert_eq!(record.class, 17);
        assert_eq!(record.len, 2);

        // SAFETY: The live side record names two canonical field values.
        let fields = unsafe { std::slice::from_raw_parts(record.data as *const Value, record.len) };
        assert_eq!(fields, [Value::Int(4), Value::Bool(true)]);
    }

    #[test]
    fn jit_list_records_refresh_after_unaccounted_growth() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::List {
            items: vec![Value::Int(1)],
            epoch: Default::default(),
        });
        if let Object::List { items, .. } = heap.get_mut(reference) {
            items.reserve(1_024);
            items.push(Value::Int(2));
        }
        let record = heap.jit_entry(reference.slot).expect("the slot exists");
        let Object::List { items, .. } = heap.get(reference) else {
            panic!("the object remains a list");
        };
        assert_eq!(record.kind, JIT_OBJECT_LIST);
        assert_eq!(record.len, items.len());
        assert_eq!(record.data, items.as_ptr() as usize);
    }

    #[test]
    fn jit_tuple_records_are_frozen() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Tuple {
            items: vec![Value::Unit],
        });
        let record = heap.jit_entry(reference.slot).expect("the slot exists");
        assert_eq!(record.kind, JIT_OBJECT_TUPLE);
        assert_ne!(record.flags & JIT_OBJECT_FROZEN, 0);
        assert_eq!(record.len, 1);
    }

    #[test]
    fn jit_pages_match_slot_addressing() {
        let mut heap = Heap::new(64 << 20);
        let mut last = None;
        for _ in 0..=PAGE_SLOTS {
            last = Some(heap.alloc(Object::Tuple { items: vec![] }));
        }
        let reference = last.expect("one object exists");
        let expected = heap.jit_entry(reference.slot).expect("the slot exists");
        let view = heap.jit_view();
        assert_eq!(view.page_count, 2);
        assert_eq!(view.slot_count, PAGE_SLOTS + 1);

        // SAFETY: The view names two complete fixed side-record pages.
        let pages = unsafe { std::slice::from_raw_parts(view.pages, view.page_count) };
        let page = pages[reference.slot as usize >> JIT_PAGE_SHIFT] as *const JitObjectEntry;
        // SAFETY: The masked slot stays inside one complete side-record page.
        let actual = unsafe {
            page.add(reference.slot as usize & JIT_PAGE_MASK as usize)
                .read()
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn freeing_a_slot_updates_its_jit_generation() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Tuple { items: vec![] });
        heap.free(reference);
        let record = heap.jit_entry(reference.slot).expect("the slot remains");
        assert_eq!(record.kind, JIT_OBJECT_DEAD);
        assert_eq!(record.generation, reference.generation.wrapping_add(1));
        assert_eq!(record.data, 0);
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
        let l = heap.alloc(Object::List {
            items: vec![],
            epoch: Default::default(),
        });
        let before = heap.used_bytes();
        if let Object::List { items, .. } = heap.get_mut(l) {
            items.push(Value::Int(1));
        }
        heap.recharge(l);
        assert_eq!(heap.used_bytes(), before + 16);
    }

    #[test]
    fn slots_grow_in_pages() {
        let mut heap = Heap::new(64 << 20);
        for _ in 0..(PAGE_SLOTS + 1) {
            heap.alloc(Object::List {
                items: vec![],
                epoch: Default::default(),
            });
        }
        assert_eq!(heap.stats().pages, 2);
    }

    #[test]
    fn collection_threshold_tracks_the_live_heap() {
        assert_eq!(next_collection_threshold(0, 64 << 20), 4 << 20);
        assert_eq!(next_collection_threshold(3 << 20, 64 << 20), 6 << 20);
        assert_eq!(next_collection_threshold(40 << 20, 64 << 20), 64 << 20);
        assert_eq!(next_collection_threshold(0, 1024), 1024);
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
    fn trimmed_slots_keep_their_generation() {
        let mut heap = Heap::new(1 << 20);
        let stale = heap.alloc(str_obj("old"));
        heap.sweep(|_| false);
        let swept = heap.jit_entry(stale.slot).expect("the slot remains");
        assert_eq!(swept.kind, JIT_OBJECT_DEAD);
        assert_ne!(swept.generation, stale.generation);
        heap.trim_free_pages();
        assert_eq!(heap.slot_count(), 0);
        assert_eq!(heap.jit_view().page_count, 0);
        assert_eq!(heap.try_get(stale), None);
        let fresh = heap.alloc(str_obj("new"));
        assert_eq!(fresh.slot, stale.slot);
        assert_ne!(fresh.generation, stale.generation);
        assert_eq!(heap.try_get(stale), None);
    }

    #[test]
    fn terminal_compaction_removes_dead_slot_storage() {
        let mut heap = Heap::new(1 << 20);
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
        assert_eq!(heap.get(root), &str_obj("live"));
        let record = heap.jit_entry(root.slot).expect("the slot exists");
        assert_eq!(record.generation, root.generation);
        assert_eq!(record.kind, JIT_OBJECT_OPAQUE);
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
