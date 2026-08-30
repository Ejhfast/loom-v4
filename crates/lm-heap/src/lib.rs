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
mod value_array;

#[cfg(test)]
use lm_value::Value;
use lm_value::{ObjRef, Witness};
pub use shape::{
    dump_shapes, BoundaryPolicy, CodeHandleKind, FaultSite, MapEntry, MapIndex, Object,
    PortableCode, PortableCodeKind, ShapeDesc, SlotChangeKind, StructuralEpoch, MIN_OBJECT_COST,
    SHAPES,
};
pub use shared::{
    process_lookup_hash, NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText,
};
use shared::{SHARED_BYTES_DATA_OFFSET, SHARED_BYTES_LEN_OFFSET};
use std::hash::{BuildHasherDefault, Hasher};
pub use value_array::{
    ValueArray, VALUE_ARRAY_CAPACITY_OFFSET, VALUE_ARRAY_DATA_OFFSET, VALUE_ARRAY_LEN_OFFSET,
    VALUE_ARRAY_SIZE,
};

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;
/// The first collection point for a heap with a larger hard limit.
const INITIAL_COLLECTION_BYTES: usize = 4 << 20;

/// Shift from an object slot to its JIT page index.
pub const JIT_PAGE_SHIFT: u32 = 10;
/// Mask from an object slot to its JIT page offset.
pub const JIT_PAGE_MASK: u32 = (PAGE_SLOTS as u32) - 1;

/// Borrowed native view of the canonical object table.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JitHeapView {
    pub pages: *const usize,
    pub page_count: usize,
    pub slot_count: usize,
}

impl JitHeapView {
    /// One empty view for native regions without direct heap access.
    pub const EMPTY: JitHeapView = JitHeapView {
        pages: std::ptr::null(),
        page_count: 0,
        slot_count: 0,
    };
}

fn next_collection_threshold(used_bytes: usize, cap_bytes: usize) -> usize {
    let live_target = used_bytes.saturating_mul(2);
    cap_bytes.min(INITIAL_COLLECTION_BYTES.max(live_target))
}

/// One object header.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Header {
    frozen: u8,
    reserved: [u8; 7],
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

/// One live object-table payload.
#[repr(C)]
struct LiveEntry {
    header: Header,
    object: Object,
}

/// One safe object-table state.
#[repr(C, u32)]
enum EntryState {
    Dead = 0,
    Live(LiveEntry) = 1,
}

/// One object-table entry.
#[repr(C)]
struct Entry {
    generation: u32,
    state: EntryState,
}

impl Entry {
    fn live(&self) -> Option<(&Header, &Object)> {
        match &self.state {
            EntryState::Dead => None,
            EntryState::Live(live) => Some((&live.header, &live.object)),
        }
    }

    fn live_mut(&mut self) -> Option<(&mut Header, &mut Object)> {
        match &mut self.state {
            EntryState::Dead => None,
            EntryState::Live(live) => Some((&mut live.header, &mut live.object)),
        }
    }

    fn object(&self) -> Option<&Object> {
        self.live().map(|(_, object)| object)
    }

    fn object_mut(&mut self) -> Option<&mut Object> {
        self.live_mut().map(|(_, object)| object)
    }

    fn is_live(&self) -> bool {
        matches!(self.state, EntryState::Live(_))
    }

    fn replace(&mut self, header: Header, object: Object) {
        debug_assert!(!self.is_live());
        self.state = EntryState::Live(LiveEntry { header, object });
    }

    fn take(&mut self) -> Option<(Header, Object)> {
        match std::mem::replace(&mut self.state, EntryState::Dead) {
            EntryState::Dead => None,
            EntryState::Live(live) => Some((live.header, live.object)),
        }
    }
}

#[repr(C)]
struct InstanceLayout {
    class: u32,
    fields: ValueArray,
    env: Witness,
}

#[repr(C)]
struct ListLayout {
    items: ValueArray,
    epoch: shape::StructuralEpoch,
}

#[repr(C)]
struct TupleLayout {
    items: ValueArray,
}

const fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

const ENTRY_STATE_PAYLOAD_OFFSET: usize = align_up(
    std::mem::size_of::<u32>(),
    std::mem::align_of::<LiveEntry>(),
);
const OBJECT_PAYLOAD_OFFSET: usize =
    align_up(std::mem::size_of::<u32>(), std::mem::align_of::<Object>());

/// Size of one canonical object-table entry.
pub const JIT_ENTRY_SIZE: usize = std::mem::size_of::<Entry>();
/// Byte offset of the object generation.
pub const JIT_ENTRY_GENERATION_OFFSET: usize = std::mem::offset_of!(Entry, generation);
/// Byte offset of the live-state tag.
pub const JIT_ENTRY_LIVE_OFFSET: usize = std::mem::offset_of!(Entry, state);
/// Tag of one live object-table entry.
pub const JIT_ENTRY_LIVE_TAG: u32 = 1;
/// Byte offset of the frozen flag.
pub const JIT_ENTRY_FROZEN_OFFSET: usize = std::mem::offset_of!(Entry, state)
    + ENTRY_STATE_PAYLOAD_OFFSET
    + std::mem::offset_of!(LiveEntry, header)
    + std::mem::offset_of!(Header, frozen);
/// Byte offset of the object tag.
pub const JIT_ENTRY_OBJECT_TAG_OFFSET: usize = std::mem::offset_of!(Entry, state)
    + ENTRY_STATE_PAYLOAD_OFFSET
    + std::mem::offset_of!(LiveEntry, object);
/// Stable tag of immutable String data.
pub const JIT_OBJECT_STR: u32 = 0;
/// Stable tag of one class instance.
pub const JIT_OBJECT_INSTANCE: u32 = 1;
/// Stable tag of one list.
pub const JIT_OBJECT_LIST: u32 = 2;
/// Stable tag of one map.
pub const JIT_OBJECT_MAP: u32 = 3;
/// Stable tag of one tuple.
pub const JIT_OBJECT_TUPLE: u32 = 4;
/// Stable tag of one closure.
pub const JIT_OBJECT_CLOSURE: u32 = 5;
/// Stable tag of immutable binary data.
pub const JIT_OBJECT_BYTES: u32 = 8;
/// Byte offset of an instance class.
pub const JIT_INSTANCE_CLASS_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(InstanceLayout, class);
/// Byte offset of an instance field array.
pub const JIT_INSTANCE_FIELDS_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(InstanceLayout, fields);
/// Byte offset of a list item array.
pub const JIT_LIST_ITEMS_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ListLayout, items);
/// Byte offset of a list structural epoch.
pub const JIT_LIST_EPOCH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ListLayout, epoch);
/// Byte offset of a tuple item array.
pub const JIT_TUPLE_ITEMS_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(TupleLayout, items);
/// Byte offset of the immutable byte data pointer.
pub const JIT_BYTES_DATA_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_DATA_OFFSET;
/// Byte offset of the immutable byte length.
pub const JIT_BYTES_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_LEN_OFFSET;

const _: () = assert!(JIT_ENTRY_GENERATION_OFFSET == 0);
const _: () = assert!(std::mem::size_of::<Header>() == shape::HEADER_COST);

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
    /// Stable addresses of canonical object pages.
    page_addresses: Vec<usize>,
    /// The latest generation of every slot ever allocated.
    generations: Vec<u32>,
    /// The current number of addressable object slots.
    slots: usize,
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
            page_addresses: Vec::new(),
            generations: Vec::new(),
            slots: 0,
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
        self.slots
    }

    /// Return one native view of the canonical object table.
    pub fn jit_view(&self) -> JitHeapView {
        JitHeapView {
            pages: self.page_addresses.as_ptr(),
            page_count: self.page_addresses.len(),
            slot_count: self.slot_count(),
        }
    }

    fn try_add_page(&mut self) -> bool {
        if self.pages.try_reserve(1).is_err() || self.page_addresses.try_reserve(1).is_err() {
            return false;
        }
        let mut page = Vec::new();
        if page.try_reserve_exact(PAGE_SLOTS).is_err() {
            return false;
        }
        let address = page.as_ptr() as usize;
        self.pages.push(page);
        self.page_addresses.push(address);
        true
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
        match (builder_entry.object_mut(), source_entry.object()) {
            (Some(Object::StrBuilder(builder)), Some(Object::Str(source)))
            | (Some(Object::StrBuilder(builder)), Some(Object::Substring(source))) => {
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
            frozen: u8::from(object.shape().born_frozen),
            reserved: [0; 7],
            bytes: base,
            shared: shared_key,
        };
        self.used_bytes += cost;
        self.live += 1;
        if let Some(slot) = self.free.pop() {
            let entry = self.entry_mut(slot);
            debug_assert!(!entry.is_live());
            let generation = entry.generation;
            entry.replace(header, object);
            return ObjRef { slot, generation };
        }

        let need_page = self
            .pages
            .last()
            .map(|page| page.len() == PAGE_SLOTS)
            .unwrap_or(true);
        if need_page {
            let page = Vec::with_capacity(PAGE_SLOTS);
            self.page_addresses.push(page.as_ptr() as usize);
            self.pages.push(page);
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
            state: EntryState::Live(LiveEntry { header, object }),
        });
        self.slots = slot + 1;
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
        entry.object()
    }

    /// Read an object. The reference must be live and current.
    pub fn get(&self, r: ObjRef) -> &Object {
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.object().expect("object reference is live")
    }

    /// Write access to an object. The caller must check the frozen bit
    /// first and must recompute the charged bytes after growth with
    /// `recharge`.
    pub fn get_mut(&mut self, r: ObjRef) -> &mut Object {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.object_mut().expect("object reference is live")
    }

    /// True when the object carries the frozen bit.
    pub fn is_frozen(&self, r: ObjRef) -> bool {
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.live().is_some_and(|(header, _)| header.frozen != 0)
    }

    /// Set the frozen bit of one object. Freezing is monotonic, and
    /// only `lm-graph` calls this after a whole graph validates.
    pub fn set_frozen(&mut self, r: ObjRef) {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        let (header, _) = entry.live_mut().expect("live object");
        header.frozen = 1;
    }

    /// Update the charged byte count of one object after a mutation.
    pub fn recharge(&mut self, r: ObjRef) {
        let (old_cost, old_shared, new_cost, new_shared) = {
            let entry = self.entry(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live().expect("live object");
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
        let (header, _) = entry.live_mut().expect("live object");
        header.bytes = new_cost;
        header.shared = new_shared_key;
        self.used_bytes = self.used_bytes - old_cost - released + new_cost + added;
    }

    /// Update one object that has no shared allocation.
    pub fn recharge_local(&mut self, r: ObjRef) {
        let (old_cost, new_cost) = {
            let entry = self.entry_mut(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live_mut().expect("live object");
            debug_assert!(header.shared.is_none());
            debug_assert!(object.shared_allocation().is_none());
            let old_cost = header.bytes;
            let new_cost = object.heap_base_cost();
            header.bytes = new_cost;
            (old_cost, new_cost)
        };
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
        let (header, _) = entry.take().expect("live object");
        entry.generation = entry.generation.wrapping_add(1);
        self.generations[slot as usize] = entry.generation;
        let shared = self.remove_shared(header.shared);
        let released = header.bytes + shared;
        self.used_bytes -= released;
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
        for (page_idx, page) in self.pages.iter_mut().enumerate() {
            for (idx, entry) in page.iter_mut().enumerate() {
                let slot = (page_idx * PAGE_SLOTS + idx) as u32;
                if !entry.is_live() || keep(slot) {
                    continue;
                }
                let (header, _) = entry.take().expect("live object");
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
        self.set_next_collection_threshold();
    }

    /// Release trailing empty pages and their work tables.
    pub fn trim_free_pages(&mut self) {
        while self
            .pages
            .last()
            .is_some_and(|page| page.iter().all(|entry| !entry.is_live()))
        {
            self.pages.pop();
            self.page_addresses.pop();
        }
        let slots = self.pages.iter().map(Vec::len).sum();
        self.slots = slots;
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
        self.page_addresses = std::mem::take(&mut compacted.page_addresses);
        self.generations = std::mem::take(&mut compacted.generations);
        self.slots = compacted.slots;
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
                if let Some((header, object)) = entry.live() {
                    let r = ObjRef {
                        slot: (page_idx * PAGE_SLOTS + idx) as u32,
                        generation: entry.generation,
                    };
                    f(r, header.frozen != 0, object);
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
            items: vec![Value::Int(1)].into(),
            epoch: Default::default(),
        });
        assert_eq!(heap.get(a), &str_obj("hello"));
        assert_eq!(
            heap.get(b),
            &Object::List {
                items: vec![Value::Int(1)].into(),
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
            items: vec![].into(),
            epoch: Default::default(),
        });
        assert!(heap.is_frozen(s));
        assert!(!heap.is_frozen(l));
    }

    #[test]
    fn native_instance_offsets_name_the_canonical_object() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Instance {
            class: 17,
            fields: vec![Value::Int(4), Value::Bool(true)].into(),
            env: lm_value::Witness::EMPTY,
        });
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;
        let EntryState::Live(live) = &entry.state else {
            panic!("the entry is live");
        };
        let Object::Instance { class, fields, .. } = &live.object else {
            panic!("the object is an instance");
        };
        assert_eq!(entry.generation, reference.generation);
        assert_eq!(
            std::ptr::from_ref(class) as usize - base,
            JIT_INSTANCE_CLASS_OFFSET
        );
        assert_eq!(
            std::ptr::from_ref(fields) as usize - base,
            JIT_INSTANCE_FIELDS_OFFSET
        );
        assert_eq!(fields.as_slice(), [Value::Int(4), Value::Bool(true)]);

        // SAFETY: The constants name initialized fields of this live entry.
        unsafe {
            assert_eq!(
                ((base + JIT_ENTRY_LIVE_OFFSET) as *const u32).read(),
                JIT_ENTRY_LIVE_TAG
            );
            assert_eq!(
                ((base + JIT_ENTRY_OBJECT_TAG_OFFSET) as *const u32).read(),
                JIT_OBJECT_INSTANCE
            );
        }
    }

    #[test]
    fn native_list_layout_reads_the_only_array_record() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::List {
            items: vec![Value::Int(1)].into(),
            epoch: Default::default(),
        });
        if let Object::List { items, .. } = heap.get_mut(reference) {
            items.reserve(1_024);
            items.push(Value::Int(2));
        }
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;
        let Object::List { items, epoch } = heap.get(reference) else {
            panic!("the object remains a list");
        };
        assert_eq!(
            std::ptr::from_ref(items) as usize - base,
            JIT_LIST_ITEMS_OFFSET
        );
        assert_eq!(
            std::ptr::from_ref(epoch) as usize - base,
            JIT_LIST_EPOCH_OFFSET
        );
        assert_eq!(items.as_slice(), [Value::Int(1), Value::Int(2)]);
    }

    #[test]
    fn native_tuple_layout_keeps_the_canonical_header() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Tuple {
            items: vec![Value::Unit].into(),
        });
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;
        let EntryState::Live(live) = &entry.state else {
            panic!("the entry is live");
        };
        let Object::Tuple { items } = &live.object else {
            panic!("the object is a tuple");
        };
        assert_eq!(
            std::ptr::from_ref(items) as usize - base,
            JIT_TUPLE_ITEMS_OFFSET
        );
        assert_eq!(live.header.frozen, 1);
    }

    #[test]
    fn native_bytes_layout_names_the_immutable_view() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Bytes(SharedBytes::from(&[3, 5, 8])));
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;
        let Object::Bytes(bytes) = heap.get(reference) else {
            panic!("the object remains binary data");
        };

        // SAFETY: The constants name initialized fields of this live entry.
        unsafe {
            assert_eq!(
                ((base + JIT_ENTRY_OBJECT_TAG_OFFSET) as *const u32).read(),
                JIT_OBJECT_BYTES
            );
            let data = ((base + JIT_BYTES_DATA_OFFSET) as *const usize).read();
            let len = ((base + JIT_BYTES_LEN_OFFSET) as *const usize).read();
            assert_eq!(data, bytes.as_slice().as_ptr() as usize);
            assert_eq!(len, bytes.len());
            assert_eq!(
                std::slice::from_raw_parts(data as *const u8, len),
                [3, 5, 8]
            );
        }
    }

    #[test]
    fn native_pages_match_slot_addressing() {
        let mut heap = Heap::new(64 << 20);
        let mut last = None;
        for _ in 0..=PAGE_SLOTS {
            last = Some(heap.alloc(Object::Tuple {
                items: vec![].into(),
            }));
        }
        let reference = last.expect("one object exists");
        let expected = heap.entry(reference.slot);
        let view = heap.jit_view();
        assert_eq!(view.page_count, 2);
        assert_eq!(view.slot_count, PAGE_SLOTS + 1);

        // SAFETY: The view names two complete canonical entry pages.
        let pages = unsafe { std::slice::from_raw_parts(view.pages, view.page_count) };
        let page = pages[reference.slot as usize >> JIT_PAGE_SHIFT] as *const Entry;
        // SAFETY: The masked slot stays inside one complete canonical page.
        let actual = unsafe { page.add(reference.slot as usize & JIT_PAGE_MASK as usize) };
        assert_eq!(actual, std::ptr::from_ref(expected));
    }

    #[test]
    fn freeing_a_slot_updates_its_canonical_generation() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Tuple {
            items: vec![].into(),
        });
        heap.free(reference);
        let entry = heap.entry(reference.slot);
        assert!(!entry.is_live());
        assert_eq!(entry.generation, reference.generation.wrapping_add(1));
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
            items: vec![].into(),
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
                items: vec![].into(),
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
        let swept = heap.entry(stale.slot);
        assert!(!swept.is_live());
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
        let entry = heap.entry(root.slot);
        assert_eq!(entry.generation, root.generation);
        assert!(entry.is_live());
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
