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

mod byte_array;
pub mod shape;
mod shared;
mod text_views;
mod value_array;

use lm_value::{ObjRef, Value, Witness};
pub use shape::{
    dump_shapes, BoundaryPolicy, CodeHandleKind, FaultSite, MapEntry, MapEntryArray, MapIndex,
    Object, PortableCode, PortableCodeKind, ShapeDesc, SlotChangeKind, StructuralEpoch,
    EMPTY_MAP_ENTRY, MAP_ENTRY_KEY_OFFSET, MAP_ENTRY_SEMANTIC_HASH_OFFSET, MAP_ENTRY_SIZE,
    MAP_ENTRY_VALUE_OFFSET, MAP_INDEX_BUILT_OFFSET, MAP_INDEX_EPOCH_OFFSET, MAP_INDEX_LIVE_OFFSET,
    MAP_INDEX_SLOTS_DATA_OFFSET, MAP_INDEX_SLOTS_LEN_OFFSET, MAP_SLOT_ENTRY_OFFSET,
    MAP_SLOT_HASH_OFFSET, MAP_SLOT_SIZE, MIN_OBJECT_COST, SHAPES,
};
pub use shared::{
    keyed_lookup_hash, process_lookup_hash, process_lookup_key, NativeByteBuffer,
    NativeStringBuilder, SharedBytes, SharedText, TextRef, TextViewBatch,
};
use shared::{
    BYTE_BUFFER_ACTIVE_OFFSET, BYTE_BUFFER_CAPACITY_OFFSET, BYTE_BUFFER_DATA_OFFSET,
    BYTE_BUFFER_LEN_OFFSET, SHARED_BYTES_DATA_OFFSET, SHARED_BYTES_LEN_OFFSET,
    SHARED_BYTES_LOOKUP_HASH_OFFSET, SHARED_BYTES_SEMANTIC_HASH_OFFSET,
    SHARED_TEXT_BYTE_LEN_OFFSET, SHARED_TEXT_DATA_OFFSET, SHARED_TEXT_LOOKUP_HASH_OFFSET,
    SHARED_TEXT_SCALAR_LEN_OFFSET, SHARED_TEXT_SEMANTIC_HASH_OFFSET, STRING_BUILDER_ACTIVE_OFFSET,
    STRING_BUILDER_ASCII_OFFSET, STRING_BUILDER_BYTE_LEN_OFFSET, STRING_BUILDER_CAPACITY_OFFSET,
    STRING_BUILDER_DATA_OFFSET, STRING_BUILDER_SCALAR_LEN_OFFSET, TEXT_VIEW_BYTE_LEN_OFFSET,
    TEXT_VIEW_DATA_OFFSET, TEXT_VIEW_LOOKUP_HASH_OFFSET, TEXT_VIEW_SCALAR_LEN_OFFSET,
    TEXT_VIEW_SEMANTIC_HASH_OFFSET,
};
use std::hash::{BuildHasherDefault, Hasher};
use text_views::TextViewTable;
pub use text_views::{
    JIT_TEXT_VIEW_ENTRY_SIZE, JIT_TEXT_VIEW_GENERATION_OFFSET, JIT_TEXT_VIEW_PAGE_MASK,
    JIT_TEXT_VIEW_PAGE_SHIFT, JIT_TEXT_VIEW_PAYLOAD_OFFSET, JIT_TEXT_VIEW_ROOT_OFFSET,
    TEXT_VIEW_GENERATION_TAG,
};
pub use value_array::{
    OwnedArray, OwnedSlice, ValueArray, OWNED_ARRAY_CAPACITY_OFFSET, OWNED_ARRAY_DATA_OFFSET,
    OWNED_ARRAY_LEN_OFFSET, OWNED_ARRAY_SIZE, OWNED_SLICE_DATA_OFFSET, OWNED_SLICE_LEN_OFFSET,
    OWNED_SLICE_SIZE, VALUE_ARRAY_CAPACITY_OFFSET, VALUE_ARRAY_DATA_OFFSET, VALUE_ARRAY_EMPTY_DATA,
    VALUE_ARRAY_LEN_OFFSET, VALUE_ARRAY_SIZE,
};

/// Object-table slots per page.
const PAGE_SLOTS: usize = 1024;
const OBJECT_GENERATION_MASK: u32 = 0x7fff_ffff;
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
    pub text_view_pages: *const usize,
    pub text_view_page_count: usize,
    pub text_view_slot_count: usize,
    pub slots: *mut usize,
    pub free: *mut OwnedArray<u32>,
    pub live: *mut usize,
    pub used_bytes: *mut usize,
    pub collection_threshold: usize,
    pub lookup_hash_key: u64,
}

impl JitHeapView {
    /// One empty view for native regions without direct heap access.
    pub const EMPTY: JitHeapView = JitHeapView {
        pages: std::ptr::null(),
        page_count: 0,
        slot_count: 0,
        text_view_pages: std::ptr::null(),
        text_view_page_count: 0,
        text_view_slot_count: 0,
        slots: std::ptr::null_mut(),
        free: std::ptr::null_mut(),
        live: std::ptr::null_mut(),
        used_bytes: std::ptr::null_mut(),
        collection_threshold: 0,
        lookup_hash_key: 0,
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
    shared: SharedKey,
}

/// One stable optional shared-allocation key.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct SharedKey {
    present: u8,
    reserved: [u8; 7],
    key: usize,
}

impl SharedKey {
    const NONE: SharedKey = SharedKey {
        present: 0,
        reserved: [0; 7],
        key: 0,
    };

    fn new(key: Option<usize>) -> SharedKey {
        match key {
            Some(key) => SharedKey {
                present: 1,
                reserved: [0; 7],
                key,
            },
            None => SharedKey::NONE,
        }
    }

    fn get(self) -> Option<usize> {
        (self.present != 0).then_some(self.key)
    }
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

fn remove_shared_references(
    allocations: &mut SharedAllocations,
    key: usize,
    references: usize,
) -> usize {
    let Some(charge) = allocations.get_mut(&key) else {
        debug_assert!(false, "a shared allocation has a heap charge");
        return 0;
    };
    charge.references = charge
        .references
        .checked_sub(references)
        .expect("a shared allocation has enough references");
    if charge.references != 0 {
        return 0;
    }
    allocations
        .remove(&key)
        .map(|charge| charge.capacity)
        .unwrap_or(0)
}

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
    fn dead(generation: u32) -> Entry {
        Entry {
            generation,
            state: EntryState::Dead,
        }
    }

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

/// One fixed canonical object page.
struct ObjectPage {
    entries: Box<[Entry]>,
}

impl std::ops::Deref for ObjectPage {
    type Target = [Entry];

    fn deref(&self) -> &[Entry] {
        &self.entries
    }
}

impl std::ops::DerefMut for ObjectPage {
    fn deref_mut(&mut self) -> &mut [Entry] {
        &mut self.entries
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
struct MapLayout {
    entries: MapEntryArray,
    index: MapIndex,
}

#[repr(C)]
struct TupleLayout {
    items: ValueArray,
}

#[repr(C)]
struct ClosureLayout {
    func: u32,
    captures: ValueArray,
    env: Witness,
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
/// Byte offset of the charged object bytes.
pub const JIT_ENTRY_BYTES_OFFSET: usize = std::mem::offset_of!(Entry, state)
    + ENTRY_STATE_PAYLOAD_OFFSET
    + std::mem::offset_of!(LiveEntry, header)
    + std::mem::offset_of!(Header, bytes);
/// Byte offset of the shared-allocation presence flag.
pub const JIT_ENTRY_SHARED_PRESENT_OFFSET: usize = std::mem::offset_of!(Entry, state)
    + ENTRY_STATE_PAYLOAD_OFFSET
    + std::mem::offset_of!(LiveEntry, header)
    + std::mem::offset_of!(Header, shared)
    + std::mem::offset_of!(SharedKey, present);
/// Byte offset of the shared-allocation key.
pub const JIT_ENTRY_SHARED_KEY_OFFSET: usize = std::mem::offset_of!(Entry, state)
    + ENTRY_STATE_PAYLOAD_OFFSET
    + std::mem::offset_of!(LiveEntry, header)
    + std::mem::offset_of!(Header, shared)
    + std::mem::offset_of!(SharedKey, key);
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
/// Stable tag of one mutable string builder.
pub const JIT_OBJECT_STRING_BUILDER: u32 = 6;
/// Stable tag of one mutable byte buffer.
pub const JIT_OBJECT_BYTE_BUFFER: u32 = 7;
/// Stable tag of immutable binary data.
pub const JIT_OBJECT_BYTES: u32 = 8;
/// Stable tag of one immutable text view.
pub const JIT_OBJECT_SUBSTRING: u32 = 9;
/// Stable tag of one graph digest.
pub const JIT_OBJECT_DIGEST: u32 = 20;
/// Byte offset of the string-builder data pointer.
pub const JIT_STRING_BUILDER_DATA_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_DATA_OFFSET;
/// Byte offset of the string-builder byte length.
pub const JIT_STRING_BUILDER_BYTE_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_BYTE_LEN_OFFSET;
/// Byte offset of the string-builder capacity.
pub const JIT_STRING_BUILDER_CAPACITY_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_CAPACITY_OFFSET;
/// Byte offset of the string-builder scalar length.
pub const JIT_STRING_BUILDER_SCALAR_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_SCALAR_LEN_OFFSET;
/// Byte offset of the string-builder ASCII flag.
pub const JIT_STRING_BUILDER_ASCII_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_ASCII_OFFSET;
/// Byte offset of the string-builder active flag.
pub const JIT_STRING_BUILDER_ACTIVE_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + STRING_BUILDER_ACTIVE_OFFSET;
/// Byte offset of the byte-buffer data pointer.
pub const JIT_BYTE_BUFFER_DATA_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + BYTE_BUFFER_DATA_OFFSET;
/// Byte offset of the byte-buffer length.
pub const JIT_BYTE_BUFFER_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + BYTE_BUFFER_LEN_OFFSET;
/// Byte offset of the byte-buffer capacity.
pub const JIT_BYTE_BUFFER_CAPACITY_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + BYTE_BUFFER_CAPACITY_OFFSET;
/// Byte offset of the byte-buffer active flag.
pub const JIT_BYTE_BUFFER_ACTIVE_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + BYTE_BUFFER_ACTIVE_OFFSET;
/// Byte offset of an instance class.
pub const JIT_INSTANCE_CLASS_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(InstanceLayout, class);
/// Byte offset of an instance field array.
pub const JIT_INSTANCE_FIELDS_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(InstanceLayout, fields);
/// Byte offset of an instance type environment.
pub const JIT_INSTANCE_ENV_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(InstanceLayout, env);
/// Byte offset of a list item array.
pub const JIT_LIST_ITEMS_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ListLayout, items);
/// Byte offset of a list structural epoch.
pub const JIT_LIST_EPOCH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ListLayout, epoch);
/// Byte offset of the live map-entry count.
pub const JIT_MAP_LIVE_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, index)
    + MAP_INDEX_LIVE_OFFSET;
/// Byte offset of the map-entry data pointer.
pub const JIT_MAP_ENTRIES_DATA_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, entries)
    + OWNED_ARRAY_DATA_OFFSET;
/// Byte offset of the map-entry count.
pub const JIT_MAP_ENTRIES_LEN_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, entries)
    + OWNED_ARRAY_LEN_OFFSET;
/// Byte offset of the map-entry capacity.
pub const JIT_MAP_ENTRIES_CAPACITY_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, entries)
    + OWNED_ARRAY_CAPACITY_OFFSET;
/// Logical heap charge for one canonical map entry.
pub const JIT_MAP_ENTRY_COST: usize = shape::ENTRY_COST;
/// Byte offset of the indexed map-entry count.
pub const JIT_MAP_INDEX_BUILT_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, index)
    + MAP_INDEX_BUILT_OFFSET;
/// Byte offset of the map-slot data pointer.
pub const JIT_MAP_INDEX_SLOTS_DATA_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, index)
    + MAP_INDEX_SLOTS_DATA_OFFSET;
/// Byte offset of the map-slot count.
pub const JIT_MAP_INDEX_SLOTS_LEN_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, index)
    + MAP_INDEX_SLOTS_LEN_OFFSET;
/// Byte offset of the map structural epoch.
pub const JIT_MAP_EPOCH_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(MapLayout, index)
    + MAP_INDEX_EPOCH_OFFSET;
/// Byte offset of a tuple item array.
pub const JIT_TUPLE_ITEMS_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(TupleLayout, items);
/// Byte offset of a closure function.
pub const JIT_CLOSURE_FUNCTION_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ClosureLayout, func);
/// Byte offset of a closure capture array.
pub const JIT_CLOSURE_CAPTURES_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET
    + OBJECT_PAYLOAD_OFFSET
    + std::mem::offset_of!(ClosureLayout, captures);
/// Byte offset of a closure type environment.
pub const JIT_CLOSURE_ENV_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + std::mem::offset_of!(ClosureLayout, env);
/// Byte offset of the immutable byte data pointer.
pub const JIT_BYTES_DATA_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_DATA_OFFSET;
/// Byte offset of the immutable byte length.
pub const JIT_BYTES_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_LEN_OFFSET;
/// Byte offset of the cached byte semantic hash.
pub const JIT_BYTES_SEMANTIC_HASH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_SEMANTIC_HASH_OFFSET;
/// Byte offset of the cached byte lookup hash.
pub const JIT_BYTES_LOOKUP_HASH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_BYTES_LOOKUP_HASH_OFFSET;
/// Byte offset of the visible UTF-8 byte length.
pub const JIT_TEXT_BYTE_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_TEXT_BYTE_LEN_OFFSET;
/// Byte offset of the visible Unicode scalar length.
pub const JIT_TEXT_SCALAR_LEN_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_TEXT_SCALAR_LEN_OFFSET;
/// Byte offset of the visible UTF-8 data pointer.
pub const JIT_TEXT_DATA_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_TEXT_DATA_OFFSET;
/// Byte offset of the cached text semantic hash.
pub const JIT_TEXT_SEMANTIC_HASH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_TEXT_SEMANTIC_HASH_OFFSET;
/// Byte offset of the cached text lookup hash.
pub const JIT_TEXT_LOOKUP_HASH_OFFSET: usize =
    JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET + SHARED_TEXT_LOOKUP_HASH_OFFSET;
/// Byte offset of data inside a normalized text payload.
pub const JIT_TEXT_PAYLOAD_DATA_OFFSET: usize = TEXT_VIEW_DATA_OFFSET;
/// Byte offset of byte length inside a normalized text payload.
pub const JIT_TEXT_PAYLOAD_BYTE_LEN_OFFSET: usize = TEXT_VIEW_BYTE_LEN_OFFSET;
/// Byte offset of scalar length inside a normalized text payload.
pub const JIT_TEXT_PAYLOAD_SCALAR_LEN_OFFSET: usize = TEXT_VIEW_SCALAR_LEN_OFFSET;
/// Byte offset of semantic hash inside a normalized text payload.
pub const JIT_TEXT_PAYLOAD_SEMANTIC_HASH_OFFSET: usize = TEXT_VIEW_SEMANTIC_HASH_OFFSET;
/// Byte offset of lookup hash inside a normalized text payload.
pub const JIT_TEXT_PAYLOAD_LOOKUP_HASH_OFFSET: usize = TEXT_VIEW_LOOKUP_HASH_OFFSET;
/// Byte offset of the graph digest bytes.
pub const JIT_DIGEST_BYTES_OFFSET: usize = JIT_ENTRY_OBJECT_TAG_OFFSET + OBJECT_PAYLOAD_OFFSET;

const _: () = assert!(JIT_ENTRY_GENERATION_OFFSET == 0);
const _: () = assert!(std::mem::size_of::<Header>() == shape::HEADER_COST);
const _: () = assert!(std::mem::size_of::<SharedKey>() == 16);

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
    normal_slots: usize,
    seen: Vec<u32>,
    ordinal: Vec<u32>,
    /// Reached objects in canonical first-encounter order. The index
    /// is the traversal ordinal.
    order: Vec<ObjRef>,
}

impl GraphScratch {
    /// Start one walk over a table of `slots` slots. The epoch step
    /// invalidates the previous walk without touching the tables.
    pub fn begin(&mut self, normal_slots: usize, text_view_slots: usize) {
        let slots = normal_slots.saturating_add(text_view_slots);
        if self.seen.len() < slots {
            self.seen.resize(slots, 0);
            self.ordinal.resize(slots, 0);
        }
        self.normal_slots = normal_slots;
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
    pub fn seen(&self, reference: ObjRef) -> bool {
        self.seen[self.index(reference)] == self.epoch
    }

    /// Record the first encounter of one slot and return its
    /// canonical traversal ordinal.
    #[inline]
    pub fn record(&mut self, r: ObjRef) -> u32 {
        let ordinal = self.order.len() as u32;
        let index = self.index(r);
        self.seen[index] = self.epoch;
        self.ordinal[index] = ordinal;
        self.order.push(r);
        ordinal
    }

    /// The canonical traversal ordinal of one reached slot.
    #[inline]
    pub fn ordinal(&self, reference: ObjRef) -> u32 {
        debug_assert!(self.seen(reference), "the walk reached this slot");
        self.ordinal[self.index(reference)]
    }

    /// Get the work-table index of one reference.
    pub fn index_of(&self, reference: ObjRef) -> usize {
        self.index(reference)
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

    fn index(&self, reference: ObjRef) -> usize {
        if TextViewTable::is_reference(reference) {
            self.normal_slots + reference.slot as usize
        } else {
            reference.slot as usize
        }
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
    pages: Vec<ObjectPage>,
    /// Stable addresses of canonical object pages.
    page_addresses: Vec<usize>,
    /// The latest generation of every slot ever allocated.
    generations: Vec<u32>,
    /// The current number of addressable object slots.
    slots: usize,
    free: OwnedArray<u32>,
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
    /// Canonical digests of frozen objects, keyed by reference and type.
    digests: std::collections::HashMap<ObjRef, DigestCacheEntry>,
    /// Shared immutable allocations referenced by this heap.
    shared_allocations: SharedAllocations,
    /// Compact canonical storage for immutable text views.
    text_views: TextViewTable,
}

impl Heap {
    pub fn new(cap_bytes: usize) -> Heap {
        Heap {
            pages: Vec::new(),
            page_addresses: Vec::new(),
            generations: Vec::new(),
            slots: 0,
            free: OwnedArray::new(),
            live: 0,
            used_bytes: 0,
            cap_bytes,
            collection_threshold: cap_bytes.min(INITIAL_COLLECTION_BYTES),
            collections: 0,
            host_roots: Vec::new(),
            scratch: GraphScratch::default(),
            digests: std::collections::HashMap::new(),
            shared_allocations: SharedAllocations::default(),
            text_views: TextViewTable::default(),
        }
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            live: self.live,
            slots: self.reference_slot_count(),
            pages: self.pages.len() + self.text_views.page_count(),
            free: self.free.len() + self.text_views.free_count(),
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

    /// Get the total number of slots in both storage classes.
    pub fn reference_slot_count(&self) -> usize {
        self.slots.saturating_add(self.text_views.slot_count())
    }

    /// Get the number of compact text-view slots.
    pub fn text_view_slot_count(&self) -> usize {
        self.text_views.slot_count()
    }

    /// Map one reference to a graph work-table index.
    pub fn reference_index(&self, reference: ObjRef) -> Option<usize> {
        if TextViewTable::is_reference(reference) {
            self.text_views.get(reference)?;
            self.slots.checked_add(reference.slot as usize)
        } else {
            self.try_get(reference).map(|_| reference.slot as usize)
        }
    }

    /// Test whether one reference names live canonical storage.
    pub fn is_live_reference(&self, reference: ObjRef) -> bool {
        if TextViewTable::is_reference(reference) {
            self.text_views.get(reference).is_some()
        } else {
            self.try_get(reference).is_some()
        }
    }

    /// Return one native view of the canonical object table.
    pub fn jit_view(&mut self) -> JitHeapView {
        JitHeapView {
            pages: self.page_addresses.as_ptr(),
            page_count: self.page_addresses.len(),
            slot_count: self.slot_count(),
            text_view_pages: self.text_views.page_addresses(),
            text_view_page_count: self.text_views.page_count(),
            text_view_slot_count: self.text_views.slot_count(),
            slots: std::ptr::from_mut(&mut self.slots),
            free: std::ptr::from_mut(&mut self.free),
            live: std::ptr::from_mut(&mut self.live),
            used_bytes: std::ptr::from_mut(&mut self.used_bytes),
            collection_threshold: self.collection_threshold,
            lookup_hash_key: process_lookup_key(),
        }
    }

    fn try_add_page(&mut self) -> bool {
        if self.pages.try_reserve(1).is_err() || self.page_addresses.try_reserve(1).is_err() {
            return false;
        }
        let page_start = self.pages.len().saturating_mul(PAGE_SLOTS);
        let page_end = match page_start.checked_add(PAGE_SLOTS) {
            Some(page_end) => page_end,
            None => return false,
        };
        let mut entries = Vec::new();
        if entries.try_reserve_exact(PAGE_SLOTS).is_err()
            || self
                .generations
                .try_reserve(page_end.saturating_sub(self.generations.len()))
                .is_err()
        {
            return false;
        }
        for slot in page_start..page_end {
            entries.push(Entry::dead(
                self.generations.get(slot).copied().unwrap_or(0),
            ));
        }
        self.generations.resize(page_end, 0);
        let page = ObjectPage {
            entries: entries.into_boxed_slice(),
        };
        let address = page.as_ptr() as usize;
        self.pages.push(page);
        self.page_addresses.push(address);
        true
    }

    fn add_page(&mut self) {
        let page_start = self.pages.len() * PAGE_SLOTS;
        let page_end = page_start + PAGE_SLOTS;
        self.generations.resize(page_end, 0);
        let mut entries = Vec::with_capacity(PAGE_SLOTS);
        for slot in page_start..page_end {
            entries.push(Entry::dead(self.generations[slot]));
        }
        let page = ObjectPage {
            entries: entries.into_boxed_slice(),
        };
        self.page_addresses.push(page.as_ptr() as usize);
        self.pages.push(page);
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

    #[inline]
    fn add_shared(&mut self, shared: Option<(usize, usize)>) -> usize {
        self.add_shared_references(shared, 1)
    }

    #[inline]
    fn add_shared_references(
        &mut self,
        shared: Option<(usize, usize)>,
        references: usize,
    ) -> usize {
        if references == 0 {
            return 0;
        }
        let Some((key, capacity)) = shared else {
            return 0;
        };
        match self.shared_allocations.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                debug_assert_eq!(entry.get().capacity, capacity);
                entry.get_mut().references = entry
                    .get()
                    .references
                    .checked_add(references)
                    .expect("the shared reference count fits");
                0
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(SharedCharge {
                    capacity,
                    references,
                });
                capacity
            }
        }
    }

    fn remove_shared(&mut self, key: Option<usize>) -> usize {
        let Some(key) = key else {
            return 0;
        };
        remove_shared_references(&mut self.shared_allocations, key, 1)
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
        if TextViewTable::is_reference(source) {
            let Some(source) = self.text_views.get(source).map(TextRef::to_shared) else {
                return false;
            };
            return match self.try_get_mut(builder) {
                Some(Object::StrBuilder(builder)) => builder.append(&source),
                _ => false,
            };
        }
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
            shared: SharedKey::new(shared_key),
        };
        self.used_bytes += cost;
        self.live += 1;
        self.install_entry(header, object)
    }

    #[inline]
    fn install_entry(&mut self, header: Header, object: Object) -> ObjRef {
        if let Some(slot) = self.free.pop() {
            let entry = self.entry_mut(slot);
            debug_assert!(!entry.is_live());
            let generation = entry.generation;
            entry.replace(header, object);
            return ObjRef { slot, generation };
        }

        if self.slots == self.pages.len() * PAGE_SLOTS {
            self.add_page();
        }
        let slot = self.slots;
        let entry = self.entry_mut(slot as u32);
        debug_assert!(!entry.is_live());
        let generation = entry.generation;
        entry.replace(header, object);
        self.slots = slot + 1;
        ObjRef {
            slot: slot as u32,
            generation,
        }
    }

    /// Get the base heap cost for one list of shared text views.
    pub fn text_view_list_base_cost(count: usize) -> Option<usize> {
        count
            .checked_mul(MIN_OBJECT_COST + std::mem::size_of::<Value>())?
            .checked_add(MIN_OBJECT_COST)
    }

    /// Split one text reference into compact descriptors with its current owner.
    pub fn try_split_text_view_batch(
        &self,
        source: ObjRef,
        separator: &str,
    ) -> Option<Result<TextViewBatch, std::collections::TryReserveError>> {
        let text = self.text(source)?;
        let owner = TextViewTable::is_reference(source).then_some(source);
        Some(text.try_split_view_batch(separator, owner))
    }

    /// Split one text reference into compact line descriptors with its current owner.
    pub fn try_line_text_view_batch(
        &self,
        source: ObjRef,
    ) -> Option<Result<TextViewBatch, std::collections::TryReserveError>> {
        let text = self.text(source)?;
        let owner = TextViewTable::is_reference(source).then_some(source);
        Some(text.try_line_view_batch(owner))
    }

    /// Allocate shared text views and their list as one heap batch.
    pub fn try_alloc_text_view_list(&mut self, batch: TextViewBatch) -> Option<ObjRef> {
        let count = batch.len();
        let base = Self::text_view_list_base_cost(count)?;
        let object_count = count.checked_add(1)?;
        if !self.text_views.can_install_batch(&batch) {
            return None;
        }
        let shared = batch.shared_allocation();
        let shared_cost = shared
            .filter(|(key, _)| !self.shared_allocations.contains_key(key))
            .map(|(_, capacity)| capacity)
            .unwrap_or(0);
        let cost = base.checked_add(shared_cost)?;
        if self.would_exceed_batch(cost, object_count) {
            return None;
        }
        let mut values = Vec::new();
        if values.try_reserve_exact(count).is_err()
            || !self.reserve_precharged_slots(1)
            || !self
                .text_views
                .try_reserve_batch(count, batch.needs_new_owner())
        {
            return None;
        }
        let charged_shared = self.add_shared_references(shared, count);
        debug_assert_eq!(charged_shared, shared_cost);
        let view_base = count * MIN_OBJECT_COST;
        self.used_bytes += view_base + charged_shared;
        self.live += count;
        self.text_views.install_batch(batch, &mut values);
        Some(self.alloc(Object::List {
            items: values.into(),
            epoch: StructuralEpoch::default(),
        }))
    }

    /// Allocate one object with a fallible table reservation.
    pub fn try_alloc(&mut self, object: Object) -> Result<ObjRef, Object> {
        if self.would_exceed(self.allocation_cost(&object)) {
            return Err(object);
        }
        if self.free.is_empty()
            && self.slots == self.pages.len() * PAGE_SLOTS
            && !self.try_add_page()
        {
            return Err(object);
        }
        Ok(self.alloc(object))
    }

    /// Read an object. Return `None` for a stale or dead reference.
    #[inline]
    pub fn try_get(&self, r: ObjRef) -> Option<&Object> {
        if TextViewTable::is_reference(r) {
            return None;
        }
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
        assert!(
            !TextViewTable::is_reference(r),
            "use Heap::text for a compact text view"
        );
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.object().expect("object reference is live")
    }

    /// Read one String or Substring from either storage class.
    #[inline]
    pub fn text(&self, reference: ObjRef) -> Option<TextRef<'_>> {
        if TextViewTable::is_reference(reference) {
            return self.text_views.get(reference);
        }
        match self.try_get(reference)? {
            Object::Str(text) | Object::Substring(text) => Some(text.text_ref()),
            _ => None,
        }
    }

    /// Clone one object from either storage class.
    pub fn try_clone_object(&self, reference: ObjRef) -> Option<Object> {
        if let Some(text) = self.text_views.get(reference) {
            return Some(Object::Substring(text.to_shared()));
        }
        self.try_get(reference).cloned()
    }

    /// Test whether a reference names one compact text view.
    pub fn is_compact_text(&self, reference: ObjRef) -> bool {
        self.text_views.get(reference).is_some()
    }

    /// Write access to an object. The caller must check the frozen bit
    /// first and must recompute the charged bytes after growth with
    /// `recharge`.
    pub fn try_get_mut(&mut self, r: ObjRef) -> Option<&mut Object> {
        if TextViewTable::is_reference(r) {
            return None;
        }
        let entry = self
            .pages
            .get_mut(r.slot as usize / PAGE_SLOTS)?
            .get_mut(r.slot as usize % PAGE_SLOTS)?;
        if entry.generation != r.generation {
            return None;
        }
        entry.object_mut()
    }

    /// Write one live object. The caller must check its frozen bit.
    pub fn get_mut(&mut self, r: ObjRef) -> &mut Object {
        let entry = self.entry_mut(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.object_mut().expect("object reference is live")
    }

    /// True when the object carries the frozen bit.
    pub fn is_frozen(&self, r: ObjRef) -> bool {
        if TextViewTable::is_reference(r) {
            return self.text_views.get(r).is_some();
        }
        let entry = self.entry(r.slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        entry.live().is_some_and(|(header, _)| header.frozen != 0)
    }

    /// Set the frozen bit of one object. Freezing is monotonic, and
    /// only `lm-graph` calls this after a whole graph validates.
    pub fn set_frozen(&mut self, r: ObjRef) {
        if TextViewTable::is_reference(r) {
            assert!(self.text_views.get(r).is_some(), "live text-view reference");
            return;
        }
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
                header.shared.get(),
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
        header.shared = SharedKey::new(new_shared_key);
        self.used_bytes = self.used_bytes - old_cost - released + new_cost + added;
    }

    /// Update one object that has no shared allocation.
    pub fn recharge_local(&mut self, r: ObjRef) {
        let (old_cost, new_cost) = {
            let entry = self.entry_mut(r.slot);
            assert_eq!(entry.generation, r.generation, "stale object reference");
            let (header, object) = entry.live_mut().expect("live object");
            debug_assert!(header.shared.get().is_none());
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
        if TextViewTable::is_reference(r) {
            let key = self.text_views.free(r);
            let shared = remove_shared_references(&mut self.shared_allocations, key, 1);
            self.used_bytes -= MIN_OBJECT_COST + shared;
            self.live -= 1;
            self.digests.remove(&r);
            return;
        }
        let slot = r.slot;
        let entry = self.entry_mut(slot);
        assert_eq!(entry.generation, r.generation, "stale object reference");
        let (header, _) = entry.take().expect("live object");
        entry.generation = entry.generation.wrapping_add(1) & OBJECT_GENERATION_MASK;
        self.generations[slot as usize] = entry.generation;
        let shared = self.remove_shared(header.shared.get());
        let released = header.bytes + shared;
        self.used_bytes -= released;
        self.live -= 1;
        self.free.push(slot);
        self.digests.remove(&r);
    }

    /// Reserve canonical slots for precharged native instances.
    pub fn reserve_precharged_slots(&mut self, count: usize) -> bool {
        let unused = self
            .pages
            .len()
            .saturating_mul(PAGE_SLOTS)
            .saturating_sub(self.slots);
        let mut available = self.free.len().saturating_add(unused);
        while available < count {
            if !self.try_add_page() {
                return false;
            }
            available = available.saturating_add(PAGE_SLOTS);
        }
        true
    }

    /// Install one instance whose native storage owns its heap charge.
    pub fn materialize_precharged_instance(
        &mut self,
        class: u32,
        fields: ValueArray,
        environment: u32,
        frozen: bool,
    ) -> ObjRef {
        let object = Object::Instance {
            class,
            fields,
            env: Witness(lm_value::TypeEnvId(environment)),
        };
        let header = Header {
            frozen: u8::from(frozen),
            reserved: [0; 7],
            bytes: object.heap_base_cost(),
            shared: SharedKey::NONE,
        };
        if let Some(slot) = self.free.pop() {
            let entry = self.entry_mut(slot);
            debug_assert!(!entry.is_live());
            let generation = entry.generation;
            entry.replace(header, object);
            return ObjRef { slot, generation };
        }

        debug_assert!(self.slots < self.pages.len().saturating_mul(PAGE_SLOTS));
        let slot = self.slots;
        let entry = self.entry_mut(slot as u32);
        debug_assert!(!entry.is_live());
        let generation = entry.generation;
        entry.replace(header, object);
        self.slots = slot + 1;
        ObjRef {
            slot: slot as u32,
            generation,
        }
    }

    /// Release native instance charges that have no canonical slots.
    pub fn release_precharged_instances(&mut self, bytes: usize, objects: usize) -> bool {
        let Some(used_bytes) = self.used_bytes.checked_sub(bytes) else {
            return false;
        };
        let Some(live) = self.live.checked_sub(objects) else {
            return false;
        };
        self.used_bytes = used_bytes;
        self.live = live;
        true
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
        match self.digests.get(&r) {
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
        let entry = self.digests.entry(r).or_insert_with(|| DigestCacheEntry {
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
    pub fn sweep(&mut self, mut keep: impl FnMut(ObjRef) -> bool) {
        self.collections = self.collections.saturating_add(1);
        let mut freed_bytes = 0usize;
        let mut freed = 0usize;
        // Most heaps hold no digest at all. The lookup per freed slot
        // costs a hash, so the empty case skips it.
        let has_digests = !self.digests.is_empty();
        let shared_allocations = &mut self.shared_allocations;
        let mut pending_shared: Option<(usize, usize)> = None;
        let mut free = self.free.vector();
        for (page_idx, page) in self.pages.iter_mut().enumerate() {
            for (idx, entry) in page.iter_mut().enumerate() {
                let slot = (page_idx * PAGE_SLOTS + idx) as u32;
                let reference = ObjRef {
                    slot,
                    generation: entry.generation,
                };
                if !entry.is_live() || keep(reference) {
                    continue;
                }
                let (header, _) = entry.take().expect("live object");
                freed_bytes += header.bytes;
                if let Some(key) = header.shared.get() {
                    match pending_shared {
                        Some((pending, references)) if pending == key => {
                            pending_shared = Some((
                                pending,
                                references
                                    .checked_add(1)
                                    .expect("the freed reference count fits"),
                            ));
                        }
                        Some((pending, references)) => {
                            freed_bytes +=
                                remove_shared_references(shared_allocations, pending, references);
                            pending_shared = Some((key, 1));
                        }
                        None => pending_shared = Some((key, 1)),
                    }
                }
                freed += 1;
                entry.generation = entry.generation.wrapping_add(1) & OBJECT_GENERATION_MASK;
                self.generations[slot as usize] = entry.generation;
                free.push(slot);
                if has_digests {
                    self.digests.remove(&reference);
                }
            }
        }
        if let Some((key, references)) = pending_shared {
            freed_bytes += remove_shared_references(shared_allocations, key, references);
        }
        let compact = self.text_views.sweep(&mut keep, |key, references| {
            remove_shared_references(shared_allocations, key, references)
        });
        freed += compact.objects;
        freed_bytes += compact.objects * MIN_OBJECT_COST + compact.bytes;
        if has_digests && compact.objects != 0 {
            self.digests.retain(|reference, _| {
                !TextViewTable::is_reference(*reference)
                    || self.text_views.get(*reference).is_some()
            });
        }
        drop(free);
        self.used_bytes -= freed_bytes;
        self.live -= freed;
        self.set_next_collection_threshold();
    }

    /// Release trailing empty pages and their work tables.
    pub fn trim_free_pages(&mut self) {
        let old_page_count = self.pages.len();
        while self
            .pages
            .last()
            .is_some_and(|page| page.iter().all(|entry| !entry.is_live()))
        {
            self.pages.pop();
            self.page_addresses.pop();
        }
        if self.pages.len() != old_page_count {
            self.slots = self.pages.len() * PAGE_SLOTS;
        }
        self.free.retain(|slot| (*slot as usize) < self.slots);
        self.free.shrink_to_fit();
        self.text_views.trim_free_pages();
        self.scratch.trim(self.reference_slot_count());
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
                .map(|held| held.wrapping_add(1) & OBJECT_GENERATION_MASK)
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
                .try_clone_object(*reference)
                .ok_or(CompactError::InvalidReference)?
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
        self.text_views = std::mem::take(&mut compacted.text_views);
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
        self.text_views.for_each_live(|reference, text| {
            let object = Object::Substring(text.to_shared());
            f(reference, true, &object);
        });
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
    fn one_text_view_batch_shares_one_backing_charge() {
        let mut heap = Heap::new(1 << 20);
        let text = SharedText::from("alpha beta");
        let capacity = text.retained_capacity();
        let source = heap.alloc(Object::Str(text.clone()));
        let source_cost = MIN_OBJECT_COST + capacity;
        let views = text.try_split_view_batch(" ").expect("the text ranges fit");
        let list = heap
            .try_alloc_text_view_list(views)
            .expect("the text range batch fits");

        let Object::List { items: values, .. } = heap.get(list) else {
            panic!("the batch returns one list");
        };
        assert_eq!(values.len(), 2);
        assert_eq!(heap.live_count(), 4);
        assert_eq!(
            heap.used_bytes(),
            source_cost + 3 * MIN_OBJECT_COST + 2 * std::mem::size_of::<Value>()
        );
        for (value, expected) in values.iter().zip(["alpha", "beta"]) {
            let Value::Obj(reference) = value else {
                panic!("the batch returns object values");
            };
            let piece = heap
                .text(*reference)
                .expect("the batch allocates text views");
            assert_eq!(piece.as_str(), expected);
        }

        heap.sweep(|reference| reference == source);
        assert_eq!(heap.live_count(), 1);
        assert_eq!(heap.used_bytes(), source_cost);
        heap.free(source);
        assert_eq!(heap.used_bytes(), 0);
    }

    #[test]
    fn nested_text_view_batches_reuse_one_owner_record() {
        let mut heap = Heap::new(1 << 20);
        let text = SharedText::from("alpha,beta;gamma,delta");
        let outer = text
            .try_split_view_batch(";")
            .expect("the outer ranges fit");
        let outer_list = heap
            .try_alloc_text_view_list(outer)
            .expect("the outer batch allocates");
        let first = match heap.get(outer_list) {
            Object::List { items, .. } => items[0].as_obj().expect("the first item is text"),
            _ => panic!("a split returns one list"),
        };
        let inner = heap
            .try_split_text_view_batch(first, ",")
            .expect("the compact source is text")
            .expect("the inner ranges fit");
        let inner_list = heap
            .try_alloc_text_view_list(inner)
            .expect("the inner batch allocates");

        assert_eq!(heap.text_views.root_count(), 1);
        let inner_views = match heap.get(inner_list) {
            Object::List { items, .. } => items
                .iter()
                .map(|value| value.as_obj().expect("an inner item is text"))
                .collect::<Vec<_>>(),
            _ => panic!("a split returns one list"),
        };
        assert_eq!(
            inner_views
                .iter()
                .map(|reference| heap.text(*reference).expect("the view is live").as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );

        heap.sweep(|_| false);
        assert_eq!(heap.text_views.root_count(), 0);
    }

    #[test]
    fn text_view_slots_reject_stale_generations() {
        let mut heap = Heap::new(1 << 20);
        let first = SharedText::from("alpha beta")
            .try_split_view_batch(" ")
            .expect("the first batch fits");
        let first_list = heap
            .try_alloc_text_view_list(first)
            .expect("the first batch allocates");
        let first_views = match heap.get(first_list) {
            Object::List { items, .. } => items
                .iter()
                .map(|value| value.as_obj().expect("a split result is text"))
                .collect::<Vec<_>>(),
            _ => panic!("a split returns one list"),
        };
        for reference in &first_views {
            heap.free(*reference);
            assert!(heap.text(*reference).is_none());
        }
        heap.free(first_list);

        let second = SharedText::from("gamma delta")
            .try_split_view_batch(" ")
            .expect("the second batch fits");
        let second_list = heap
            .try_alloc_text_view_list(second)
            .expect("the second batch allocates");
        let second_views = match heap.get(second_list) {
            Object::List { items, .. } => items
                .iter()
                .map(|value| value.as_obj().expect("a split result is text"))
                .collect::<Vec<_>>(),
            _ => panic!("a split returns one list"),
        };
        for current in second_views {
            let stale = first_views
                .iter()
                .find(|reference| reference.slot == current.slot)
                .expect("the compact table reuses its free slots");
            assert_ne!(current.generation, stale.generation);
            assert!(heap.is_compact_text(current));
        }
    }

    #[test]
    fn compaction_materializes_compact_text_views() {
        let mut heap = Heap::new(1 << 20);
        let batch = SharedText::from("alpha beta")
            .try_split_view_batch(" ")
            .expect("the text ranges fit");
        let mut list = heap
            .try_alloc_text_view_list(batch)
            .expect("the text ranges allocate");

        heap.compact_live(std::slice::from_mut(&mut list))
            .expect("the compact heap compacts");

        let views = match heap.get(list) {
            Object::List { items, .. } => items
                .iter()
                .map(|value| value.as_obj().expect("a split result is text"))
                .collect::<Vec<_>>(),
            _ => panic!("a split returns one list"),
        };
        assert_eq!(heap.live_count(), 3);
        assert_eq!(heap.text_view_slot_count(), 0);
        assert_eq!(
            heap.text(views[0])
                .expect("the first view is live")
                .as_str(),
            "alpha"
        );
        assert_eq!(
            heap.text(views[1])
                .expect("the second view is live")
                .as_str(),
            "beta"
        );
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
        let Object::Instance { class, fields, env } = &live.object else {
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
        assert_eq!(
            std::ptr::from_ref(env) as usize - base,
            JIT_INSTANCE_ENV_OFFSET
        );
        assert_eq!(
            std::ptr::from_ref(&live.header.bytes) as usize - base,
            JIT_ENTRY_BYTES_OFFSET
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
    fn precharged_native_instances_keep_exact_heap_counts() {
        let mut heap = Heap::new(1 << 20);
        let fields: ValueArray = vec![Value::Int(4), Value::Bool(true)].into();
        let object = Object::Instance {
            class: 17,
            fields: fields.clone(),
            env: Witness(lm_value::TypeEnvId(9)),
        };
        let bytes = object.heap_base_cost();
        heap.used_bytes = bytes * 2;
        heap.live = 2;

        assert!(heap.reserve_precharged_slots(1));
        let reference = heap.materialize_precharged_instance(17, fields, 9, true);
        assert_eq!(heap.live_count(), 2);
        assert_eq!(heap.used_bytes(), bytes * 2);
        assert!(heap.is_frozen(reference));
        assert_eq!(heap.get(reference), &object);

        assert!(heap.release_precharged_instances(bytes, 1));
        assert_eq!(heap.live_count(), 1);
        assert_eq!(heap.used_bytes(), bytes);
        heap.free(reference);
        assert_eq!(heap.live_count(), 0);
        assert_eq!(heap.used_bytes(), 0);
    }

    #[test]
    fn native_closure_offsets_name_the_canonical_object() {
        let mut heap = Heap::new(1 << 20);
        let reference = heap.alloc(Object::Closure {
            func: 17,
            captures: vec![Value::Int(4), Value::Bool(true)].into(),
            env: Witness::EMPTY,
        });
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;
        let EntryState::Live(live) = &entry.state else {
            panic!("the entry is live");
        };
        let Object::Closure {
            func,
            captures,
            env,
        } = &live.object
        else {
            panic!("the object is a closure");
        };
        assert_eq!(
            std::ptr::from_ref(func) as usize - base,
            JIT_CLOSURE_FUNCTION_OFFSET
        );
        assert_eq!(
            std::ptr::from_ref(captures) as usize - base,
            JIT_CLOSURE_CAPTURES_OFFSET
        );
        assert_eq!(
            std::ptr::from_ref(env) as usize - base,
            JIT_CLOSURE_ENV_OFFSET
        );
        assert_eq!(captures.as_slice(), [Value::Int(4), Value::Bool(true)]);
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
    fn native_map_layout_names_entries_and_lookup_slots() {
        let mut heap = Heap::new(1 << 20);
        let hash = 0x1234_5678_9abc_def0;
        let mut index = MapIndex::with_live(StructuralEpoch(7), 0);
        index.push_live(hash, 0);
        let reference = heap.alloc(Object::Map {
            entries: vec![MapEntry {
                key: Value::Int(4),
                value: Value::Bool(true),
                semantic_hash: 4,
            }]
            .into(),
            index,
        });
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;

        // SAFETY: The constants name initialized fields of this live entry.
        unsafe {
            assert_eq!(
                ((base + JIT_ENTRY_OBJECT_TAG_OFFSET) as *const u32).read(),
                JIT_OBJECT_MAP
            );
            assert_eq!(((base + JIT_MAP_LIVE_OFFSET) as *const u32).read(), 1);
            assert_eq!(((base + JIT_MAP_EPOCH_OFFSET) as *const u32).read(), 7);
            assert_eq!(
                ((base + JIT_MAP_ENTRIES_LEN_OFFSET) as *const usize).read(),
                1
            );
            assert!(((base + JIT_MAP_ENTRIES_CAPACITY_OFFSET) as *const usize).read() >= 1);
            assert_eq!(
                ((base + JIT_MAP_INDEX_BUILT_OFFSET) as *const u32).read(),
                1
            );
            let entries =
                ((base + JIT_MAP_ENTRIES_DATA_OFFSET) as *const *const u8).read() as usize;
            assert_eq!(
                ((entries + MAP_ENTRY_KEY_OFFSET) as *const Value).read(),
                Value::Int(4)
            );
            assert_eq!(
                ((entries + MAP_ENTRY_VALUE_OFFSET) as *const Value).read(),
                Value::Bool(true)
            );
            assert_eq!(
                ((entries + MAP_ENTRY_SEMANTIC_HASH_OFFSET) as *const i64).read(),
                4
            );
            let slots =
                ((base + JIT_MAP_INDEX_SLOTS_DATA_OFFSET) as *const *const u8).read() as usize;
            let slot_count = ((base + JIT_MAP_INDEX_SLOTS_LEN_OFFSET) as *const usize).read();
            let occupied = (0..slot_count)
                .map(|slot| slots + slot * MAP_SLOT_SIZE)
                .find(|slot| {
                    ((*slot + MAP_SLOT_ENTRY_OFFSET) as *const u32).read() != EMPTY_MAP_ENTRY
                })
                .expect("one lookup slot is occupied");
            assert_eq!(
                ((occupied + MAP_SLOT_HASH_OFFSET) as *const u64).read(),
                hash
            );
            assert_eq!(((occupied + MAP_SLOT_ENTRY_OFFSET) as *const u32).read(), 0);
        }
    }

    #[test]
    fn native_digest_layout_names_the_canonical_bytes() {
        let mut heap = Heap::new(1 << 20);
        let bytes = [0x5a; 32];
        let reference = heap.alloc(Object::NativeDigest(bytes));
        let entry = heap.entry(reference.slot);
        let base = std::ptr::from_ref(entry) as usize;

        // SAFETY: The constants name initialized fields of this live entry.
        unsafe {
            assert_eq!(
                ((base + JIT_ENTRY_OBJECT_TAG_OFFSET) as *const u32).read(),
                JIT_OBJECT_DIGEST
            );
            assert_eq!(
                std::slice::from_raw_parts((base + JIT_DIGEST_BYTES_OFFSET) as *const u8, 32),
                bytes
            );
        }
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
    fn native_text_layout_names_both_text_shapes() {
        let mut heap = Heap::new(1 << 20);
        let text = SharedText::from("aé猫z");
        let view = text.scalar_slice(1, 2).expect("the scalar range is valid");
        let string = heap.alloc(Object::Str(text));
        let substring = heap.alloc(Object::Substring(view));

        for (reference, tag, byte_len, scalar_len) in [
            (string, JIT_OBJECT_STR, 7, 4),
            (substring, JIT_OBJECT_SUBSTRING, 5, 2),
        ] {
            let entry = heap.entry(reference.slot);
            let base = std::ptr::from_ref(entry) as usize;
            let expected = match heap.get(reference) {
                Object::Str(text) | Object::Substring(text) => text.as_str().as_bytes(),
                _ => panic!("the object is text"),
            };
            // SAFETY: The constants name initialized fields of this live entry.
            unsafe {
                assert_eq!(
                    ((base + JIT_ENTRY_OBJECT_TAG_OFFSET) as *const u32).read(),
                    tag
                );
                assert_eq!(
                    ((base + JIT_TEXT_BYTE_LEN_OFFSET) as *const usize).read(),
                    byte_len
                );
                assert_eq!(
                    ((base + JIT_TEXT_SCALAR_LEN_OFFSET) as *const usize).read(),
                    scalar_len
                );
                let data = ((base + JIT_TEXT_DATA_OFFSET) as *const usize).read();
                assert_eq!(
                    std::slice::from_raw_parts(data as *const u8, byte_len),
                    expected
                );
            }
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
        let expected = std::ptr::from_ref(heap.entry(reference.slot));
        let view = heap.jit_view();
        assert_eq!(view.page_count, 2);
        assert_eq!(view.slot_count, PAGE_SLOTS + 1);
        // SAFETY: The view names this live heap charge counter.
        assert_eq!(unsafe { view.used_bytes.read() }, heap.used_bytes());
        // SAFETY: The view names this heap's allocation counters.
        assert_eq!(unsafe { view.slots.read() }, heap.slot_count());
        // SAFETY: The view names this heap's allocation counters.
        assert_eq!(unsafe { view.live.read() }, heap.live_count());
        // SAFETY: The view names this heap's canonical free-slot array.
        assert!(unsafe { &*view.free }.is_empty());
        assert_eq!(view.collection_threshold, heap.collection_threshold);

        // SAFETY: The view names two complete canonical entry pages.
        let pages = unsafe { std::slice::from_raw_parts(view.pages, view.page_count) };
        let page = pages[reference.slot as usize >> JIT_PAGE_SHIFT] as *const Entry;
        // SAFETY: The masked slot stays inside one complete canonical page.
        let actual = unsafe { page.add(reference.slot as usize & JIT_PAGE_MASK as usize) };
        assert_eq!(actual, expected);
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
        scratch.begin(4, 0);
        let a = ObjRef {
            slot: 1,
            generation: 0,
        };
        assert!(!scratch.seen(a));
        assert_eq!(scratch.record(a), 0);
        assert!(scratch.seen(a));
        scratch.begin(4, 0);
        assert!(!scratch.seen(a));
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
        heap.sweep(|reference| reference == root);
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
