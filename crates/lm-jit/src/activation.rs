//! Native activation storage and typed slow-path boundaries.

use crate::Failure;
use lm_heap::{JitHeapView, MIN_OBJECT_COST};
use lm_value::{ObjRef, Value, ValueTag, VALUE_SIZE};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub(super) const RUNTIME_OK: u32 = 0;
const RUNTIME_INTERPRETER: u32 = 1;
pub(super) const RUNTIME_HEAP_LIMIT: u32 = 2;
pub(super) const RUNTIME_STACK_LIMIT: u32 = 3;
pub(super) const RUNTIME_FAULT_FLAG: u32 = 1 << 31;
pub(super) const RUNTIME_MAP_VACANT: u32 = 4;
pub(super) const RUNTIME_COLLECTION_REQUIRED: u32 = 5;

const INITIAL_NATIVE_SCALARS: usize = 4_096;
const INITIAL_NATIVE_FRAMES: usize = 256;
pub(super) const VIRTUAL_INSTANCE_COUNT: usize = 64;
pub(super) const VIRTUAL_INSTANCE_FIELDS: usize = 16;
pub(super) const PENDING_INSTANCE_SLOT_BASE: u32 = u32::MAX - VIRTUAL_INSTANCE_COUNT as u32 + 1;
pub(super) const SCALAR_INSTANCE_COUNT: usize = 64;
pub(super) const SCALAR_INSTANCE_SLOT_BASE: u32 =
    PENDING_INSTANCE_SLOT_BASE - SCALAR_INSTANCE_COUNT as u32;
pub(super) const TYPE_ENVIRONMENT_CACHE_WAYS: usize = 4;
const INITIAL_TYPE_ENVIRONMENT_CACHE_SETS: usize = 16;
const MAX_TYPE_ENVIRONMENT_CACHE_SETS: usize = 1_024;
const TYPE_ENVIRONMENT_CACHE_CLAIMED: u64 = u64::MAX;
pub(super) const RESOLVED_CALL_CACHE_WAYS: usize = 4;
const INITIAL_RESOLVED_CALL_CACHE_SETS: usize = 16;
const MAX_RESOLVED_CALL_CACHE_SETS: usize = 1_024;
const RESOLVED_CALL_CACHE_CLAIMED: u64 = u64::MAX;

pub(super) const IMAGE_SLOT_EMPTY: u32 = 0;
pub(super) const IMAGE_SLOT_FUNCTION: u32 = 1;
pub(super) const IMAGE_SLOT_CLASS: u32 = 2;
const IMAGE_SLOT_VALUE: u32 = 3;
const IMAGE_SLOT_PROCESS: u32 = 4;

/// The native local contains an initialized value.
pub const LOCAL_INITIALIZED: u8 = 2;

#[repr(C)]
#[derive(Debug, Default)]
pub(super) struct RawExit {
    pub(super) retired: u64,
    pub(super) kind: u32,
    pub(super) block: u32,
    pub(super) instruction: u32,
    pub(super) stack_len: u32,
    pub(super) result_tag: u64,
    pub(super) result: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RawNativeFrame {
    pub(super) function: u32,
    pub(super) environment: u32,
    pub(super) capture_tag: u64,
    pub(super) capture_bits: u64,
    pub(super) capture_data: usize,
    pub(super) capture_len: usize,
    pub(super) block: u32,
    pub(super) instruction: u32,
    pub(super) resume_entry: u32,
    pub(super) scalar_base: u32,
    pub(super) local_count: u32,
    pub(super) max_stack: u32,
    pub(super) operand_len: u32,
    pub(super) native_created: u32,
    pub(super) caller_stack_values: u32,
    pub(super) native_stack_bytes: u32,
}

/// One transient instance whose fields stay in native scalar storage.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RawVirtualInstance {
    pub(super) active: u32,
    pub(super) references: u32,
    pub(super) object_bits: u64,
    pub(super) class: u32,
    pub(super) environment: u32,
    pub(super) field_count: u32,
    pub(super) frozen: u32,
}

#[repr(C)]
pub(super) struct RawNativeActivation {
    pub(super) scalars: *mut u64,
    pub(super) tags: *mut u64,
    pub(super) states: *mut u8,
    pub(super) scalar_len: u32,
    pub(super) scalar_capacity: u32,
    pub(super) frames: *mut RawNativeFrame,
    pub(super) frame_len: u32,
    pub(super) frame_capacity: u32,
    pub(super) changed_from: u32,
    pub(super) virtual_instances: *mut RawVirtualInstance,
    pub(super) virtual_values: *mut Value,
    pub(super) virtual_available: u64,
    pub(super) virtual_request: u32,
    pub(super) virtual_reserved: u32,
    pub(super) root_code: usize,
    pub(super) entries: *const usize,
    pub(super) entry_count: u32,
    pub(super) max_stack_values: u32,
    pub(super) base_frames: u32,
    pub(super) max_frames: u32,
    pub(super) root_capacity: u32,
    pub(super) heap_pages: *const usize,
    pub(super) heap_page_count: usize,
    pub(super) heap_slot_count: usize,
    pub(super) text_view_pages: *const usize,
    pub(super) text_view_page_count: usize,
    pub(super) text_view_slot_count: usize,
    pub(super) heap_slots: *mut usize,
    pub(super) heap_free: *mut c_void,
    pub(super) heap_live: *mut usize,
    pub(super) heap_used_bytes: *mut usize,
    pub(super) heap_collection_threshold: usize,
    pub(super) inline_allocations: u64,
    pub(super) pending_instance_allocations: u64,
    pub(super) pending_instance_releases: u64,
    pub(super) scalar_replaced_allocations: u64,
    pub(super) lookup_hash_key: u64,
    pub(super) class_parents: *const u32,
    pub(super) class_count: usize,
    pub(super) dispatch_rows: *const NativeDispatchRow,
    pub(super) dispatch_row_count: usize,
    pub(super) dispatch_methods: *const u32,
    pub(super) dispatch_method_count: usize,
    pub(super) literal_values: *const Value,
    pub(super) literal_count: usize,
    pub(super) type_store_id: u64,
    pub(super) type_environments: *const RawTypeEnvironmentCacheEntry,
    pub(super) type_environment_mask: u32,
    pub(super) resolved_calls: *const RawResolvedCallCacheEntry,
    pub(super) resolved_call_mask: u32,
    pub(super) image_slots: *const NativeImageSlot,
    pub(super) image_slot_count: usize,
    pub(super) poll_requested: *const u32,
    pub(super) hard_fuel: u64,
    pub(super) poll_deadline: u64,
    pub(super) poll_interval: u32,
    pub(super) runtime_context: *mut c_void,
    pub(super) runtime_functions: *const RawNativeFunctions,
    pub(super) allocation_result: *mut u64,
    pub(super) roots: *mut u64,
    pub(super) root_tags: *mut u64,
    pub(super) root_states: *mut u8,
    pub(super) exit: *mut RawExit,
}

/// One compact native view of an image slot target.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeImageSlot {
    pub(super) kind: u32,
    pub(super) first: u32,
    pub(super) second: u32,
}

impl NativeImageSlot {
    /// Create one empty image slot.
    pub const fn empty() -> NativeImageSlot {
        NativeImageSlot {
            kind: IMAGE_SLOT_EMPTY,
            first: 0,
            second: 0,
        }
    }

    /// Create one function image slot.
    pub const fn function(function: u32) -> NativeImageSlot {
        NativeImageSlot {
            kind: IMAGE_SLOT_FUNCTION,
            first: function,
            second: 0,
        }
    }

    /// Create one class image slot.
    pub const fn class(class: u32, constructor: u32) -> NativeImageSlot {
        NativeImageSlot {
            kind: IMAGE_SLOT_CLASS,
            first: class,
            second: constructor,
        }
    }

    /// Create one value image slot marker.
    pub const fn value() -> NativeImageSlot {
        NativeImageSlot {
            kind: IMAGE_SLOT_VALUE,
            first: 0,
            second: 0,
        }
    }

    /// Create one process image slot marker.
    pub const fn process() -> NativeImageSlot {
        NativeImageSlot {
            kind: IMAGE_SLOT_PROCESS,
            first: 0,
            second: 0,
        }
    }
}

/// One stable row in the native class dispatch table.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeDispatchRow {
    pub(super) base: u32,
    pub(super) len: usize,
    pub(super) start: usize,
}

impl NativeDispatchRow {
    /// Create one native dispatch row.
    pub fn new(base: u32, len: usize, start: usize) -> NativeDispatchRow {
        NativeDispatchRow { base, len, start }
    }
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct RawTypeEnvironmentCacheEntry {
    pub(super) store: AtomicU64,
    pub(super) function: AtomicU32,
    pub(super) block: AtomicU32,
    pub(super) instruction: AtomicU32,
    pub(super) parent: AtomicU32,
    pub(super) child: AtomicU32,
}

impl RawTypeEnvironmentCacheEntry {
    fn new() -> RawTypeEnvironmentCacheEntry {
        RawTypeEnvironmentCacheEntry {
            store: AtomicU64::new(0),
            function: AtomicU32::new(0),
            block: AtomicU32::new(0),
            instruction: AtomicU32::new(0),
            parent: AtomicU32::new(0),
            child: AtomicU32::new(0),
        }
    }

    fn matches(
        &self,
        store: u64,
        function: u32,
        block: u32,
        instruction: u32,
        parent: u32,
    ) -> bool {
        self.store.load(Ordering::Acquire) == store
            && self.function.load(Ordering::Relaxed) == function
            && self.block.load(Ordering::Relaxed) == block
            && self.instruction.load(Ordering::Relaxed) == instruction
            && self.parent.load(Ordering::Relaxed) == parent
    }

    fn publish(
        &self,
        store: u64,
        function: u32,
        block: u32,
        instruction: u32,
        parent: u32,
        child: u32,
    ) {
        self.store
            .store(TYPE_ENVIRONMENT_CACHE_CLAIMED, Ordering::Release);
        self.function.store(function, Ordering::Relaxed);
        self.block.store(block, Ordering::Relaxed);
        self.instruction.store(instruction, Ordering::Relaxed);
        self.parent.store(parent, Ordering::Relaxed);
        self.child.store(child, Ordering::Relaxed);
        self.store.store(store, Ordering::Release);
    }

    fn snapshot(&self) -> Option<(u64, u32, u32, u32, u32, u32)> {
        let store = self.store.load(Ordering::Acquire);
        if store == 0 || store == TYPE_ENVIRONMENT_CACHE_CLAIMED {
            return None;
        }
        Some((
            store,
            self.function.load(Ordering::Relaxed),
            self.block.load(Ordering::Relaxed),
            self.instruction.load(Ordering::Relaxed),
            self.parent.load(Ordering::Relaxed),
            self.child.load(Ordering::Relaxed),
        ))
    }
}

#[repr(C)]
#[derive(Debug)]
pub(super) struct RawResolvedCallCacheEntry {
    pub(super) store: AtomicU64,
    pub(super) function: AtomicU32,
    pub(super) block: AtomicU32,
    pub(super) instruction: AtomicU32,
    pub(super) parent: AtomicU32,
    pub(super) receiver: AtomicU64,
    pub(super) target: AtomicU32,
    pub(super) environment: AtomicU32,
    pub(super) capture_data: AtomicUsize,
    pub(super) capture_len: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedCallCacheRecord {
    store: u64,
    function: u32,
    block: u32,
    instruction: u32,
    parent: u32,
    receiver: u64,
    target: u32,
    environment: u32,
    capture_data: usize,
    capture_len: usize,
}

impl RawResolvedCallCacheEntry {
    fn new() -> RawResolvedCallCacheEntry {
        RawResolvedCallCacheEntry {
            store: AtomicU64::new(0),
            function: AtomicU32::new(0),
            block: AtomicU32::new(0),
            instruction: AtomicU32::new(0),
            parent: AtomicU32::new(0),
            receiver: AtomicU64::new(0),
            target: AtomicU32::new(0),
            environment: AtomicU32::new(0),
            capture_data: AtomicUsize::new(0),
            capture_len: AtomicUsize::new(0),
        }
    }

    fn matches(
        &self,
        store: u64,
        function: u32,
        block: u32,
        instruction: u32,
        parent: u32,
        receiver: u64,
    ) -> bool {
        self.store.load(Ordering::Acquire) == store
            && self.function.load(Ordering::Relaxed) == function
            && self.block.load(Ordering::Relaxed) == block
            && self.instruction.load(Ordering::Relaxed) == instruction
            && self.parent.load(Ordering::Relaxed) == parent
            && self.receiver.load(Ordering::Relaxed) == receiver
    }

    fn publish(&self, record: ResolvedCallCacheRecord) {
        self.store
            .store(RESOLVED_CALL_CACHE_CLAIMED, Ordering::Release);
        self.function.store(record.function, Ordering::Relaxed);
        self.block.store(record.block, Ordering::Relaxed);
        self.instruction
            .store(record.instruction, Ordering::Relaxed);
        self.parent.store(record.parent, Ordering::Relaxed);
        self.receiver.store(record.receiver, Ordering::Relaxed);
        self.target.store(record.target, Ordering::Relaxed);
        self.environment
            .store(record.environment, Ordering::Relaxed);
        self.capture_data
            .store(record.capture_data, Ordering::Relaxed);
        self.capture_len
            .store(record.capture_len, Ordering::Relaxed);
        self.store.store(record.store, Ordering::Release);
    }

    fn snapshot(&self) -> Option<ResolvedCallCacheRecord> {
        let store = self.store.load(Ordering::Acquire);
        if store == 0 || store == RESOLVED_CALL_CACHE_CLAIMED {
            return None;
        }
        Some(ResolvedCallCacheRecord {
            store,
            function: self.function.load(Ordering::Relaxed),
            block: self.block.load(Ordering::Relaxed),
            instruction: self.instruction.load(Ordering::Relaxed),
            parent: self.parent.load(Ordering::Relaxed),
            receiver: self.receiver.load(Ordering::Relaxed),
            target: self.target.load(Ordering::Relaxed),
            environment: self.environment.load(Ordering::Relaxed),
            capture_data: self.capture_data.load(Ordering::Relaxed),
            capture_len: self.capture_len.load(Ordering::Relaxed),
        })
    }
}

pub(super) fn type_environment_site_hash(function: u32, block: u32, instruction: u32) -> u32 {
    let mut value = function.wrapping_mul(0x9e37_79b9);
    value ^= block.rotate_left(11);
    value ^= instruction.wrapping_mul(0x85eb_ca6b);
    value ^= value >> 16;
    value
}

fn type_environment_cache_set(
    function: u32,
    block: u32,
    instruction: u32,
    parent: u32,
    sets: usize,
) -> usize {
    let parent = parent ^ (parent >> 16);
    (type_environment_site_hash(function, block, instruction) ^ parent) as usize & (sets - 1)
}

pub(super) fn resolved_call_cache_set(
    function: u32,
    block: u32,
    instruction: u32,
    parent: u32,
    receiver: u64,
    sets: usize,
) -> usize {
    let parent = parent ^ (parent >> 16);
    let receiver = receiver ^ (receiver >> 32);
    let receiver = receiver as u32 ^ (receiver as u32).rotate_left(7);
    (type_environment_site_hash(function, block, instruction) ^ parent ^ receiver) as usize
        & (sets - 1)
}

pub(super) type RawAllocateInstance =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, *mut u64) -> u32;
pub(super) type RawPrepareInstanceFields = unsafe extern "C" fn(u32, *mut u64) -> u32;
pub(super) type RawAllocateCapture =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, u32, u32, *mut u64) -> u32;
pub(super) type RawAllocateValues =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, *mut u64) -> u32;
pub(super) type RawGrowList = unsafe extern "C" fn(*mut c_void, u64, u64, u64, u32) -> u32;
pub(super) type RawInsertList = unsafe extern "C" fn(*mut c_void, u64, i64, u64, u64, u32) -> u32;
pub(super) type RawReserveList = unsafe extern "C" fn(*mut c_void, u64, i64, u32) -> u32;
pub(super) type RawMapLookup = unsafe extern "C" fn(*mut c_void, u64, u64, u64, *mut u64) -> u32;
pub(super) type RawValueEqual =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, *mut u64) -> u32;
pub(super) type RawObjectBinary = unsafe extern "C" fn(*mut c_void, u64, u64, *mut u64) -> u32;
pub(super) type RawObjectUnary = unsafe extern "C" fn(*mut c_void, u64, *mut u64) -> u32;
pub(super) type RawDigest =
    unsafe extern "C" fn(*mut c_void, u64, u32, u32, u32, u32, *mut u64) -> u32;
pub(super) type RawHeapOperation =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u32, *mut u64) -> u32;
pub(super) type RawMapPutCommit =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, u64, u64, u64, u32, u32, u32) -> u32;
pub(super) type RawMapPutDiscard =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, u64, u32, u32) -> u32;
pub(super) type RawMapInternTextRange =
    unsafe extern "C" fn(*mut c_void, u64, u64, i64, i64, u32, *mut u64) -> u32;
pub(super) type RawMapInsertHashed =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, u64, i64, i64, u32) -> u32;
pub(super) type RawBytesEqual = unsafe extern "C" fn(*const u8, *const u8, usize) -> u32;

/// Fixed native entry points for typed runtime slow paths.
#[repr(C)]
pub(super) struct RawNativeFunctions {
    pub(super) prepare_instance_fields: RawPrepareInstanceFields,
    pub(super) allocate_instance: RawAllocateInstance,
    pub(super) allocate_closure: RawAllocateCapture,
    pub(super) allocate_callback: RawAllocateCapture,
    pub(super) allocate_tuple: RawAllocateValues,
    pub(super) allocate_list: RawAllocateValues,
    pub(super) allocate_map: RawAllocateValues,
    pub(super) grow_list: RawGrowList,
    pub(super) insert_list: RawInsertList,
    pub(super) reserve_list: RawReserveList,
    pub(super) list_contains: RawMapLookup,
    pub(super) map_has: RawMapLookup,
    pub(super) map_at: RawMapLookup,
    pub(super) map_get: RawMapLookup,
    pub(super) map_next_index: RawMapLookup,
    pub(super) map_key_at: RawObjectBinary,
    pub(super) map_value_at: RawObjectBinary,
    pub(super) map_remove: RawMapLookup,
    pub(super) map_clear: RawObjectUnary,
    pub(super) map_reserve: RawReserveList,
    pub(super) map_probe: RawMapLookup,
    pub(super) map_probe_key: RawObjectBinary,
    pub(super) map_probe_value: RawObjectBinary,
    pub(super) map_probe_set_value: RawValueEqual,
    pub(super) map_probe_remove: RawObjectBinary,
    pub(super) map_insert_hashed: RawMapInsertHashed,
    pub(super) map_put_discard: RawMapPutDiscard,
    pub(super) map_put_probe: RawMapLookup,
    pub(super) map_put_commit: RawMapPutCommit,
    pub(super) map_intern_text_range: RawMapInternTextRange,
    pub(super) bytes_equal: RawBytesEqual,
    pub(super) value_equal: RawValueEqual,
    pub(super) text_compare: RawObjectBinary,
    pub(super) bytes_compare: RawObjectBinary,
    pub(super) text_hash: RawObjectUnary,
    pub(super) bytes_hash: RawObjectUnary,
    pub(super) freeze_graph: RawObjectUnary,
    pub(super) digest_value: RawDigest,
    pub(super) fault_code: RawHeapOperation,
    pub(super) fault_denied: RawHeapOperation,
    pub(super) dyn_pack: RawHeapOperation,
    pub(super) syntax_tree_root: RawHeapOperation,
    pub(super) syntax_kind: RawHeapOperation,
    pub(super) syntax_category: RawHeapOperation,
    pub(super) syntax_range_start: RawHeapOperation,
    pub(super) syntax_range_end: RawHeapOperation,
    pub(super) syntax_text: RawHeapOperation,
    pub(super) syntax_children: RawHeapOperation,
    pub(super) syntax_detach: RawHeapOperation,
    pub(super) syntax_build_token: RawHeapOperation,
    pub(super) syntax_build_trivia: RawHeapOperation,
    pub(super) syntax_build_node: RawHeapOperation,
    pub(super) syntax_to_tree: RawHeapOperation,
    pub(super) string_builder_new: RawHeapOperation,
    pub(super) string_builder_append_text: RawHeapOperation,
    pub(super) string_builder_append_int: RawHeapOperation,
    pub(super) string_builder_append_bool: RawHeapOperation,
    pub(super) string_builder_append_char: RawHeapOperation,
    pub(super) string_builder_append_float: RawHeapOperation,
    pub(super) string_builder_build: RawHeapOperation,
    pub(super) string_builder_finish: RawHeapOperation,
    pub(super) byte_buffer_new: RawHeapOperation,
    pub(super) byte_buffer_append: RawHeapOperation,
    pub(super) byte_buffer_build: RawHeapOperation,
    pub(super) byte_buffer_extend: RawHeapOperation,
    pub(super) byte_buffer_reserve: RawHeapOperation,
    pub(super) byte_buffer_finish: RawHeapOperation,
    pub(super) bytes_from_text: RawHeapOperation,
    pub(super) bytes_slice: RawHeapOperation,
    pub(super) bytes_concat: RawHeapOperation,
    pub(super) bytes_compact: RawHeapOperation,
    pub(super) bytes_text_view: RawHeapOperation,
    pub(super) bytes_bit_and: RawHeapOperation,
    pub(super) bytes_bit_or: RawHeapOperation,
    pub(super) bytes_bit_xor: RawHeapOperation,
    pub(super) bytes_bit_not: RawHeapOperation,
    pub(super) text_concat: RawHeapOperation,
    pub(super) text_starts_with: RawHeapOperation,
    pub(super) text_ends_with: RawHeapOperation,
    pub(super) text_contains: RawHeapOperation,
    pub(super) text_find_scalar: RawHeapOperation,
    pub(super) text_find_byte: RawHeapOperation,
    pub(super) text_trim: RawHeapOperation,
    pub(super) text_trim_start: RawHeapOperation,
    pub(super) text_trim_end: RawHeapOperation,
    pub(super) text_lower_ascii: RawHeapOperation,
    pub(super) text_upper_ascii: RawHeapOperation,
    pub(super) text_replace: RawHeapOperation,
    pub(super) text_parse_int_status: RawHeapOperation,
    pub(super) text_parse_int_value: RawHeapOperation,
    pub(super) text_pad_start: RawHeapOperation,
    pub(super) text_pad_end: RawHeapOperation,
    pub(super) bytes_ends_with: RawHeapOperation,
    pub(super) bytes_contains: RawHeapOperation,
    pub(super) text_split: RawHeapOperation,
    pub(super) text_lines: RawHeapOperation,
    pub(super) text_slice: RawHeapOperation,
    pub(super) text_slice_bytes: RawHeapOperation,
    pub(super) text_bytes: RawHeapOperation,
    pub(super) text_to_string: RawHeapOperation,
    pub(super) bytes_text: RawHeapOperation,
    pub(super) bytes_text_range: RawHeapOperation,
    pub(super) byte_buffer_find_from: RawHeapOperation,
    pub(super) bytes_starts_with: RawHeapOperation,
    pub(super) bytes_find_index: RawHeapOperation,
    pub(super) bytes_hex: RawHeapOperation,
    pub(super) bytes_is_utf8: RawHeapOperation,
    pub(super) digest_sha256: RawHeapOperation,
    pub(super) digest_crc32: RawHeapOperation,
    pub(super) digest_md5: RawHeapOperation,
    pub(super) compress_encode: RawHeapOperation,
    pub(super) compress_decode_status: RawHeapOperation,
    pub(super) compress_decode_value: RawHeapOperation,
    pub(super) text_parse_float_status: RawHeapOperation,
    pub(super) text_parse_float_value: RawHeapOperation,
    pub(super) float_fixed: RawHeapOperation,
    pub(super) regex_compile_status: RawHeapOperation,
    pub(super) regex_compile_value: RawHeapOperation,
    pub(super) regex_source: RawHeapOperation,
    pub(super) regex_is_match: RawHeapOperation,
    pub(super) regex_captures: RawHeapOperation,
    pub(super) regex_count: RawHeapOperation,
    pub(super) regex_split: RawHeapOperation,
    pub(super) regex_replace_all: RawHeapOperation,
    pub(super) regex_match_start: RawHeapOperation,
    pub(super) regex_match_end: RawHeapOperation,
    pub(super) regex_match_text: RawHeapOperation,
    pub(super) regex_match_group_count: RawHeapOperation,
    pub(super) regex_match_group: RawHeapOperation,
    pub(super) regex_match_named: RawHeapOperation,
}

/// One immutable helper table for one runtime implementation.
pub(super) struct NativeRuntimeFunctions<R>(std::marker::PhantomData<fn(&mut R)>);

impl<R: NativeRuntime> NativeRuntimeFunctions<R> {
    pub(super) const TABLE: RawNativeFunctions = RawNativeFunctions {
        prepare_instance_fields,
        allocate_instance: allocate_instance::<R>,
        allocate_closure: allocate_closure::<R>,
        allocate_callback: allocate_callback::<R>,
        allocate_tuple: allocate_tuple::<R>,
        allocate_list: allocate_list::<R>,
        allocate_map: allocate_map::<R>,
        grow_list: grow_list::<R>,
        insert_list: insert_list::<R>,
        reserve_list: reserve_list::<R>,
        list_contains: list_contains::<R>,
        map_has: map_has::<R>,
        map_at: map_at::<R>,
        map_get: map_get::<R>,
        map_next_index: map_next_index::<R>,
        map_key_at: map_key_at::<R>,
        map_value_at: map_value_at::<R>,
        map_remove: map_remove::<R>,
        map_clear: map_clear::<R>,
        map_reserve: reserve_map::<R>,
        map_probe: map_probe::<R>,
        map_probe_key: map_probe_key::<R>,
        map_probe_value: map_probe_value::<R>,
        map_probe_set_value: map_probe_set_value::<R>,
        map_probe_remove: map_probe_remove::<R>,
        map_insert_hashed: map_insert_hashed::<R>,
        map_put_discard: map_put_discard::<R>,
        map_put_probe: map_put_probe::<R>,
        map_put_commit: map_put_commit::<R>,
        map_intern_text_range: map_intern_text_range::<R>,
        bytes_equal,
        value_equal: values_equal::<R>,
        text_compare: text_compare::<R>,
        bytes_compare: bytes_compare::<R>,
        text_hash: text_hash::<R>,
        bytes_hash: bytes_hash::<R>,
        freeze_graph: freeze_graph::<R>,
        digest_value: digest_value::<R>,
        fault_code: fault_code::<R>,
        fault_denied: fault_denied::<R>,
        dyn_pack: dyn_pack::<R>,
        syntax_tree_root: syntax_tree_root::<R>,
        syntax_kind: syntax_kind::<R>,
        syntax_category: syntax_category::<R>,
        syntax_range_start: syntax_range_start::<R>,
        syntax_range_end: syntax_range_end::<R>,
        syntax_text: syntax_text::<R>,
        syntax_children: syntax_children::<R>,
        syntax_detach: syntax_detach::<R>,
        syntax_build_token: syntax_build_token::<R>,
        syntax_build_trivia: syntax_build_trivia::<R>,
        syntax_build_node: syntax_build_node::<R>,
        syntax_to_tree: syntax_to_tree::<R>,
        string_builder_new: string_builder_new::<R>,
        string_builder_append_text: string_builder_append_text::<R>,
        string_builder_append_int: string_builder_append_int::<R>,
        string_builder_append_bool: string_builder_append_bool::<R>,
        string_builder_append_char: string_builder_append_char::<R>,
        string_builder_append_float: string_builder_append_float::<R>,
        string_builder_build: string_builder_build::<R>,
        string_builder_finish: string_builder_finish::<R>,
        byte_buffer_new: byte_buffer_new::<R>,
        byte_buffer_append: byte_buffer_append::<R>,
        byte_buffer_build: byte_buffer_build::<R>,
        byte_buffer_extend: byte_buffer_extend::<R>,
        byte_buffer_reserve: byte_buffer_reserve::<R>,
        byte_buffer_finish: byte_buffer_finish::<R>,
        bytes_from_text: bytes_from_text::<R>,
        bytes_slice: bytes_slice::<R>,
        bytes_concat: bytes_concat::<R>,
        bytes_compact: bytes_compact::<R>,
        bytes_text_view: bytes_text_view::<R>,
        bytes_bit_and: bytes_bit_and::<R>,
        bytes_bit_or: bytes_bit_or::<R>,
        bytes_bit_xor: bytes_bit_xor::<R>,
        bytes_bit_not: bytes_bit_not::<R>,
        text_concat: text_concat::<R>,
        text_starts_with: text_starts_with::<R>,
        text_ends_with: text_ends_with::<R>,
        text_contains: text_contains::<R>,
        text_find_scalar: text_find_scalar::<R>,
        text_find_byte: text_find_byte::<R>,
        text_trim: text_trim::<R>,
        text_trim_start: text_trim_start::<R>,
        text_trim_end: text_trim_end::<R>,
        text_lower_ascii: text_lower_ascii::<R>,
        text_upper_ascii: text_upper_ascii::<R>,
        text_replace: text_replace::<R>,
        text_parse_int_status: text_parse_int_status::<R>,
        text_parse_int_value: text_parse_int_value::<R>,
        text_pad_start: text_pad_start::<R>,
        text_pad_end: text_pad_end::<R>,
        bytes_ends_with: bytes_ends_with::<R>,
        bytes_contains: bytes_contains::<R>,
        text_split: text_split::<R>,
        text_lines: text_lines::<R>,
        text_slice: text_slice::<R>,
        text_slice_bytes: text_slice_bytes::<R>,
        text_bytes: text_bytes::<R>,
        text_to_string: text_to_string::<R>,
        bytes_text: bytes_text::<R>,
        bytes_text_range: bytes_text_range::<R>,
        byte_buffer_find_from: byte_buffer_find_from::<R>,
        bytes_starts_with: bytes_starts_with::<R>,
        bytes_find_index: bytes_find_index::<R>,
        bytes_hex: bytes_hex::<R>,
        bytes_is_utf8: bytes_is_utf8::<R>,
        digest_sha256: digest_sha256::<R>,
        digest_crc32: digest_crc32::<R>,
        digest_md5: digest_md5::<R>,
        compress_encode: compress_encode::<R>,
        compress_decode_status: compress_decode_status::<R>,
        compress_decode_value: compress_decode_value::<R>,
        text_parse_float_status: text_parse_float_status::<R>,
        text_parse_float_value: text_parse_float_value::<R>,
        float_fixed: float_fixed::<R>,
        regex_compile_status: regex_compile_status::<R>,
        regex_compile_value: regex_compile_value::<R>,
        regex_source: regex_source::<R>,
        regex_is_match: regex_is_match::<R>,
        regex_captures: regex_captures::<R>,
        regex_count: regex_count::<R>,
        regex_split: regex_split::<R>,
        regex_replace_all: regex_replace_all::<R>,
        regex_match_start: regex_match_start::<R>,
        regex_match_end: regex_match_end::<R>,
        regex_match_text: regex_match_text::<R>,
        regex_match_group_count: regex_match_group_count::<R>,
        regex_match_group: regex_match_group::<R>,
        regex_match_named: regex_match_named::<R>,
    };
}

unsafe extern "C" fn bytes_equal(left: *const u8, right: *const u8, length: usize) -> u32 {
    if length == 0 || left == right {
        return 1;
    }
    if left.is_null() || right.is_null() {
        return 0;
    }
    // SAFETY: Native code passes two live byte ranges with this exact length.
    let left = unsafe { std::slice::from_raw_parts(left, length) };
    // SAFETY: Native code passes two live byte ranges with this exact length.
    let right = unsafe { std::slice::from_raw_parts(right, length) };
    u32::from(left == right)
}

pub(super) type NativeFunction = unsafe extern "C" fn(*mut RawNativeActivation, u32);

/// Reusable scalar and frame storage for one native turn.
#[derive(Debug, Default)]
pub struct NativeActivation {
    pub(super) scalars: Vec<u64>,
    pub(super) tags: Vec<u64>,
    pub(super) states: Vec<u8>,
    pub(super) frames: Vec<RawNativeFrame>,
    pub(super) virtual_instances: Vec<RawVirtualInstance>,
    pub(super) virtual_values: Vec<Value>,
    pub(super) scalar_len: usize,
    pub(super) frame_len: usize,
    pub(super) changed_from: usize,
}

/// One immutable transient instance view.
#[derive(Debug, Clone, Copy)]
pub struct NativePendingInstance<'a> {
    record: RawVirtualInstance,
    fields: &'a [Value],
}

impl<'a> NativePendingInstance<'a> {
    /// Return the transient object token.
    pub fn object_bits(self) -> u64 {
        self.record.object_bits
    }

    /// Return the relocated class index.
    pub fn class(self) -> u32 {
        self.record.class
    }

    /// Return the closed type environment.
    pub fn environment(self) -> u32 {
        self.record.environment
    }

    /// Return the number of native aliases.
    pub fn references(self) -> u32 {
        self.record.references
    }

    /// Return true when the constructor sealed this instance.
    pub fn frozen(self) -> bool {
        self.record.frozen != 0
    }

    /// Return the logical byte charge.
    pub fn byte_cost(self) -> usize {
        MIN_OBJECT_COST.saturating_add(self.fields.len().saturating_mul(VALUE_SIZE))
    }

    /// Return the canonical field values.
    pub fn fields(self) -> &'a [Value] {
        self.fields
    }
}

/// One machine-local cache of environment-dependent type metadata.
#[derive(Debug, Default)]
pub struct NativeTypeEnvironmentCache {
    entries: Vec<RawTypeEnvironmentCacheEntry>,
}

/// One machine-local polymorphic resolved-call cache.
#[derive(Debug, Default)]
pub struct NativeResolvedCallCache {
    entries: Vec<RawResolvedCallCacheEntry>,
}

/// One fixed native view of a machine-local type-environment cache.
#[derive(Debug, Clone, Copy)]
pub struct NativeTypeEnvironmentView {
    pub(super) entries: *const RawTypeEnvironmentCacheEntry,
    pub(super) mask: u32,
}

/// One fixed native view of a machine-local resolved-call cache.
#[derive(Debug, Clone, Copy)]
pub struct NativeResolvedCallView {
    pub(super) entries: *const RawResolvedCallCacheEntry,
    pub(super) mask: u32,
}

impl NativeResolvedCallView {
    /// One empty view for code without resolved calls.
    pub const EMPTY: NativeResolvedCallView = NativeResolvedCallView {
        entries: std::ptr::null(),
        mask: 0,
    };
}

impl NativeTypeEnvironmentView {
    /// One empty view for code without generic calls.
    pub const EMPTY: NativeTypeEnvironmentView = NativeTypeEnvironmentView {
        entries: std::ptr::null(),
        mask: 0,
    };
}

/// Root frame data and native scratch limits.
pub struct NativePreparation {
    pub function: u32,
    pub environment: u32,
    pub capture_tag: u64,
    pub capture_bits: u64,
    pub capture_data: usize,
    pub capture_len: usize,
    pub block: u32,
    pub instruction: u32,
    pub local_count: usize,
    pub max_stack: usize,
    pub operand_len: usize,
    pub scalar_limit: usize,
    pub frame_limit: usize,
}

/// Inputs for one bounded native execution.
pub struct NativeExecution<'a> {
    pub entry: u32,
    pub entries: &'a [usize],
    pub base_stack_values: usize,
    pub max_stack_values: usize,
    pub base_frames: usize,
    pub max_frames: usize,
    pub roots: &'a mut [u64],
    pub root_tags: &'a mut [u64],
    pub root_states: &'a mut [u8],
    pub fuel: u64,
    pub poll: NativePoll<'a>,
    pub heap: JitHeapView,
    pub class_parents: &'a [u32],
    pub dispatch_rows: &'a [NativeDispatchRow],
    pub dispatch_methods: &'a [u32],
    pub literals: NativeLiteralView,
    pub type_store_id: u64,
    pub type_environments: NativeTypeEnvironmentView,
    pub resolved_calls: NativeResolvedCallView,
    pub image_slots: NativeImageSlotView,
}

/// One optional native scheduler poll.
#[derive(Debug, Clone, Copy)]
pub struct NativePoll<'a> {
    requested: Option<&'a AtomicU32>,
    schedule: PollSchedule,
}

/// One deterministic sequence of execution polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollSchedule {
    first: u32,
    interval: u32,
}

impl PollSchedule {
    /// Create one poll schedule.
    pub const fn new(first: u32, interval: u32) -> PollSchedule {
        PollSchedule {
            first: if first == 0 { 1 } else { first },
            interval: if interval == 0 { 1 } else { interval },
        }
    }

    /// Get the distance between polls.
    pub const fn interval(self) -> u32 {
        self.interval
    }

    /// Get the distance to the next poll after retired instructions.
    pub fn remaining_after(self, retired: u64) -> u32 {
        let first = u64::from(self.first);
        if retired < first {
            return (first - retired) as u32;
        }
        let interval = u64::from(self.interval);
        let within = (retired - first) % interval;
        if within == 0 {
            self.interval
        } else {
            (interval - within) as u32
        }
    }

    /// Test whether retired instructions end at one poll.
    pub fn due_at(self, retired: u64) -> bool {
        let first = u64::from(self.first);
        retired >= first && (retired - first).is_multiple_of(u64::from(self.interval))
    }

    fn after_retirement(self, retired: u64) -> PollSchedule {
        PollSchedule {
            first: self.remaining_after(retired),
            interval: self.interval,
        }
    }
}

impl NativePoll<'static> {
    /// Disable native scheduler polling.
    pub const fn disabled() -> NativePoll<'static> {
        NativePoll {
            requested: None,
            schedule: PollSchedule::new(u32::MAX, u32::MAX),
        }
    }
}

impl<'a> NativePoll<'a> {
    /// Create one native scheduler poll.
    pub fn new(requested: &'a AtomicU32, first: u32, interval: u32) -> NativePoll<'a> {
        NativePoll {
            requested: Some(requested),
            schedule: PollSchedule::new(first, interval),
        }
    }

    pub(super) fn requested_pointer(self) -> *const u32 {
        self.requested
            .map_or(std::ptr::null(), |requested| requested.as_ptr())
    }

    pub(super) fn initial_fuel(self, hard_fuel: u64) -> u64 {
        hard_fuel.min(u64::from(self.schedule.first))
    }

    pub(super) const fn interval(self) -> u32 {
        self.schedule.interval
    }

    /// Move the first poll past retired instructions.
    pub fn after_retirement(self, retired: u64) -> NativePoll<'a> {
        NativePoll {
            schedule: self.schedule.after_retirement(retired),
            ..self
        }
    }
}

/// One fixed native view of image slot targets.
#[derive(Debug, Clone, Copy)]
pub struct NativeImageSlotView {
    pub(super) entries: *const NativeImageSlot,
    pub(super) count: usize,
}

impl NativeImageSlotView {
    /// One empty image slot view.
    pub const EMPTY: NativeImageSlotView = NativeImageSlotView {
        entries: std::ptr::null(),
        count: 0,
    };

    /// Create a view over fixed slot storage.
    ///
    /// # Safety
    ///
    /// The caller must retain the storage during native execution.
    pub unsafe fn from_raw_parts(
        entries: *const NativeImageSlot,
        count: usize,
    ) -> NativeImageSlotView {
        NativeImageSlotView { entries, count }
    }
}

/// One native view of a machine's canonical literal table.
#[derive(Debug, Clone, Copy)]
pub struct NativeLiteralView {
    pub(super) values: *const Value,
    pub(super) count: usize,
}

impl NativeLiteralView {
    /// One empty literal table.
    pub const EMPTY: NativeLiteralView = NativeLiteralView {
        values: std::ptr::null(),
        count: 0,
    };

    /// Create a view over storage that stays fixed during native execution.
    ///
    /// # Safety
    ///
    /// The caller must keep every value live and unchanged during native execution.
    pub unsafe fn from_raw_parts(values: *const Value, count: usize) -> NativeLiteralView {
        NativeLiteralView { values, count }
    }
}

/// Mutable canonical buffers for one native root frame.
pub type NativeRootBuffersMut<'a> = (
    &'a mut [u64],
    &'a mut [u64],
    &'a mut [u8],
    &'a mut [u64],
    &'a mut [u64],
);

/// Immutable canonical buffers for one native root frame.
pub type NativeRootBuffers<'a> = (&'a [u64], &'a [u64], &'a [u8], &'a [u64], &'a [u64]);

/// One materialized view of a live native frame.
pub struct NativeFrameView<'a> {
    frame: RawNativeFrame,
    locals: &'a [u64],
    local_tags: &'a [u64],
    states: &'a [u8],
    operands: &'a [u64],
    operand_tags: &'a [u64],
}

impl NativeFrameView<'_> {
    /// Return the first scalar slot of this frame.
    pub fn scalar_base(&self) -> usize {
        self.frame.scalar_base as usize
    }

    /// Return the namespace function slot.
    pub fn function(&self) -> u32 {
        self.frame.function
    }

    /// Return the canonical type environment.
    pub fn environment(&self) -> u32 {
        self.frame.environment
    }

    /// Return the canonical frame-capture tag.
    pub fn capture_tag(&self) -> u64 {
        self.frame.capture_tag
    }

    /// Return the canonical frame-capture payload.
    pub fn capture_bits(&self) -> u64 {
        self.frame.capture_bits
    }

    /// Return the immutable capture-array address.
    pub fn capture_data(&self) -> usize {
        self.frame.capture_data
    }

    /// Return the immutable capture-array length.
    pub fn capture_len(&self) -> usize {
        self.frame.capture_len
    }

    /// Return the current bytecode block.
    pub fn block(&self) -> u32 {
        self.frame.block
    }

    /// Return the current bytecode instruction.
    pub fn instruction(&self) -> u32 {
        self.frame.instruction
    }

    /// Return true when native code created this frame.
    pub fn native_created(&self) -> bool {
        self.frame.native_created != 0
    }

    /// Return the local scalar bits.
    pub fn locals(&self) -> &[u64] {
        self.locals
    }

    /// Return the local canonical value tags.
    pub fn local_tags(&self) -> &[u64] {
        self.local_tags
    }

    /// Return the local initialization states.
    pub fn states(&self) -> &[u8] {
        self.states
    }

    /// Return the operand scalar bits.
    pub fn operands(&self) -> &[u64] {
        self.operands
    }

    /// Return the operand canonical value tags.
    pub fn operand_tags(&self) -> &[u64] {
        self.operand_tags
    }
}

impl NativeActivation {
    /// Prepare one root frame without changing guest state.
    pub fn prepare_root(&mut self, input: NativePreparation) -> Result<(), Failure> {
        let NativePreparation {
            function,
            environment,
            capture_tag,
            capture_bits,
            capture_data,
            capture_len,
            block,
            instruction,
            local_count,
            max_stack,
            operand_len,
            scalar_limit,
            frame_limit,
        } = input;
        let window = local_count
            .checked_add(max_stack)
            .ok_or(Failure::BackendUnavailable)?;
        let scalar_capacity = INITIAL_NATIVE_SCALARS.max(window).min(scalar_limit);
        let frame_capacity = INITIAL_NATIVE_FRAMES.max(1).min(frame_limit);
        if window > scalar_capacity || frame_capacity == 0 || operand_len > max_stack {
            return Err(Failure::BackendUnavailable);
        }
        if self
            .scalars
            .try_reserve(scalar_capacity.saturating_sub(self.scalars.len()))
            .is_err()
            || self
                .tags
                .try_reserve(scalar_capacity.saturating_sub(self.tags.len()))
                .is_err()
            || self
                .states
                .try_reserve(scalar_capacity.saturating_sub(self.states.len()))
                .is_err()
            || self
                .frames
                .try_reserve(frame_capacity.saturating_sub(self.frames.len()))
                .is_err()
            || self
                .virtual_instances
                .try_reserve(VIRTUAL_INSTANCE_COUNT.saturating_sub(self.virtual_instances.len()))
                .is_err()
            || self
                .virtual_values
                .try_reserve(
                    VIRTUAL_INSTANCE_COUNT
                        .saturating_mul(VIRTUAL_INSTANCE_FIELDS)
                        .saturating_sub(self.virtual_values.len()),
                )
                .is_err()
        {
            return Err(Failure::BackendUnavailable);
        }
        self.scalars.resize(scalar_capacity, 0);
        self.tags.resize(scalar_capacity, 0);
        self.states.resize(scalar_capacity, 0);
        self.frames
            .resize(frame_capacity, RawNativeFrame::default());
        self.virtual_instances
            .resize(VIRTUAL_INSTANCE_COUNT, RawVirtualInstance::default());
        self.virtual_values.resize(
            VIRTUAL_INSTANCE_COUNT.saturating_mul(VIRTUAL_INSTANCE_FIELDS),
            Value::Uninit,
        );
        self.scalars[..window].fill(0);
        self.tags[..window].fill(0);
        self.states[..window].fill(0);
        self.frames[0] = RawNativeFrame {
            function,
            environment,
            capture_tag,
            capture_bits,
            capture_data,
            capture_len,
            block,
            instruction,
            resume_entry: 0,
            scalar_base: 0,
            local_count: u32::try_from(local_count).map_err(|_| Failure::BackendUnavailable)?,
            max_stack: u32::try_from(max_stack).map_err(|_| Failure::BackendUnavailable)?,
            operand_len: u32::try_from(operand_len).map_err(|_| Failure::BackendUnavailable)?,
            native_created: 0,
            caller_stack_values: 0,
            native_stack_bytes: 0,
        };
        self.scalar_len = window;
        self.frame_len = 1;
        self.changed_from = 0;
        self.clear_pending_instances();
        Ok(())
    }

    /// Return all transient instances that native code has not released.
    pub fn pending_instances(&self) -> impl Iterator<Item = NativePendingInstance<'_>> {
        self.virtual_instances
            .iter()
            .enumerate()
            .filter(|(_, record)| record.active != 0)
            .map(|(index, record)| {
                let start = index.saturating_mul(VIRTUAL_INSTANCE_FIELDS);
                let end = start.saturating_add(record.field_count as usize);
                NativePendingInstance {
                    record: *record,
                    fields: &self.virtual_values[start..end],
                }
            })
    }

    /// Forget all transient records after the heap consumes them.
    pub fn clear_pending_instances(&mut self) {
        for record in &mut self.virtual_instances {
            record.active = 0;
            record.references = 0;
        }
    }

    /// Replace one transient token with one canonical object reference.
    pub fn replace_pending_reference(&mut self, token: u64, reference: ObjRef) {
        let replacement = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
        for (bits, tag) in self.scalars[..self.scalar_len]
            .iter_mut()
            .zip(&self.tags[..self.scalar_len])
        {
            if *tag == ValueTag::Obj as u64 && *bits == token {
                *bits = replacement;
            }
        }
        for frame in &mut self.frames[..self.frame_len] {
            if frame.capture_tag == ValueTag::Obj as u64 && frame.capture_bits == token {
                frame.capture_bits = replacement;
            }
        }
        for value in &mut self.virtual_values {
            let Value::Obj(held) = value else {
                continue;
            };
            let held_bits = u64::from(held.slot) | (u64::from(held.generation) << 32);
            if held_bits == token {
                *held = reference;
            }
        }
    }

    /// Replace one typed object slot without dynamic tag storage.
    pub fn replace_pending_object_slot(&mut self, slot: usize, token: u64, reference: ObjRef) {
        let Some(bits) = self.scalars.get_mut(slot) else {
            return;
        };
        if *bits == token {
            *bits = u64::from(reference.slot) | (u64::from(reference.generation) << 32);
        }
    }

    /// Return all mutable root buffers.
    pub fn root_buffers_mut(&mut self) -> NativeRootBuffersMut<'_> {
        let locals = self.frames[0].local_count as usize;
        let stack = self.frames[0].max_stack as usize;
        let (local_bits, rest) = self.scalars.split_at_mut(locals);
        let operand_bits = &mut rest[..stack];
        let (local_tags, rest) = self.tags.split_at_mut(locals);
        let operand_tags = &mut rest[..stack];
        (
            local_bits,
            local_tags,
            &mut self.states[..locals],
            operand_bits,
            operand_tags,
        )
    }

    /// Return all immutable root buffers.
    pub fn root_buffers(&self) -> NativeRootBuffers<'_> {
        let locals = self.frames[0].local_count as usize;
        let stack = self.frames[0].max_stack as usize;
        (
            &self.scalars[..locals],
            &self.tags[..locals],
            &self.states[..locals],
            &self.scalars[locals..locals + stack],
            &self.tags[locals..locals + stack],
        )
    }

    /// Return every live native frame in call order.
    pub fn frames(&self) -> impl ExactSizeIterator<Item = NativeFrameView<'_>> {
        self.frames[..self.frame_len].iter().map(|frame| {
            let base = frame.scalar_base as usize;
            let locals = frame.local_count as usize;
            let operands = frame.operand_len as usize;
            let operand_base = base + locals;
            NativeFrameView {
                frame: *frame,
                locals: &self.scalars[base..operand_base],
                local_tags: &self.tags[base..operand_base],
                states: &self.states[base..operand_base],
                operands: &self.scalars[operand_base..operand_base + operands],
                operand_tags: &self.tags[operand_base..operand_base + operands],
            }
        })
    }

    /// Return one live native frame by call depth.
    pub fn frame(&self, index: usize) -> Option<NativeFrameView<'_>> {
        let frame = *self.frames.get(index).filter(|_| index < self.frame_len)?;
        let base = frame.scalar_base as usize;
        let locals = frame.local_count as usize;
        let operands = frame.operand_len as usize;
        let operand_base = base.checked_add(locals)?;
        let operand_end = operand_base.checked_add(operands)?;
        Some(NativeFrameView {
            frame,
            locals: self.scalars.get(base..operand_base)?,
            local_tags: self.tags.get(base..operand_base)?,
            states: self.states.get(base..operand_base)?,
            operands: self.scalars.get(operand_base..operand_end)?,
            operand_tags: self.tags.get(operand_base..operand_end)?,
        })
    }

    /// Return the top live native frame.
    pub fn top_frame(&self) -> Option<NativeFrameView<'_>> {
        self.frame(self.frame_len.checked_sub(1)?)
    }

    /// Return the live native frame count.
    pub fn frame_count(&self) -> usize {
        self.frame_len
    }

    /// Start one native execution with no persistent frame change.
    pub fn begin_execution(&mut self) {
        self.changed_from = self.frame_len;
    }

    /// Return the first frame that changed during native execution.
    pub fn changed_from(&self) -> usize {
        self.changed_from
    }

    /// Grow native stack storage outside generated code.
    pub fn grow(
        &mut self,
        required_scalars: usize,
        required_frames: usize,
        scalar_limit: usize,
        frame_limit: usize,
    ) -> Result<bool, Failure> {
        let scalar_target = growth_target(self.scalars.len(), required_scalars, scalar_limit)?;
        let frame_target = growth_target(self.frames.len(), required_frames, frame_limit)?;
        if scalar_target == self.scalars.len() && frame_target == self.frames.len() {
            return Ok(false);
        }
        self.scalars
            .try_reserve(scalar_target.saturating_sub(self.scalars.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.tags
            .try_reserve(scalar_target.saturating_sub(self.tags.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.states
            .try_reserve(scalar_target.saturating_sub(self.states.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.frames
            .try_reserve(frame_target.saturating_sub(self.frames.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.scalars.resize(scalar_target, 0);
        self.tags.resize(scalar_target, 0);
        self.states.resize(scalar_target, 0);
        self.frames.resize(frame_target, RawNativeFrame::default());
        Ok(true)
    }

    /// Finish one detached native return inside this activation.
    pub fn finish_detached_return(&mut self, tag: u64, result: u64) -> Result<(), Failure> {
        if self.frame_len <= 1 {
            return Err(Failure::BackendUnavailable);
        }
        let child = self.frames[self.frame_len - 1];
        let parent = &mut self.frames[self.frame_len - 2];
        if child.scalar_base as usize > self.scalar_len || parent.operand_len >= parent.max_stack {
            return Err(Failure::BackendUnavailable);
        }
        let operand = (parent.scalar_base as usize)
            .checked_add(parent.local_count as usize)
            .and_then(|base| base.checked_add(parent.operand_len as usize))
            .ok_or(Failure::BackendUnavailable)?;
        if operand >= child.scalar_base as usize || operand >= self.scalars.len() {
            return Err(Failure::BackendUnavailable);
        }
        self.scalars[operand] = result;
        self.tags[operand] = tag;
        parent.operand_len += 1;
        self.frame_len -= 1;
        self.scalar_len = child.scalar_base as usize;
        self.changed_from = self.changed_from.min(self.frame_len);
        Ok(())
    }

    /// Remove one effect request and install its reply in the top frame.
    pub fn finish_effect(
        &mut self,
        consumed: usize,
        block: u32,
        instruction: u32,
        tag: u64,
        result: u64,
    ) -> Result<u32, Failure> {
        let top_index = self
            .frame_len
            .checked_sub(1)
            .ok_or(Failure::BackendUnavailable)?;
        let frame = self.frames[top_index];
        let operand_len = frame.operand_len as usize;
        let prefix = operand_len
            .checked_sub(consumed)
            .ok_or(Failure::BackendUnavailable)?;
        if prefix >= frame.max_stack as usize {
            return Err(Failure::BackendUnavailable);
        }
        let slot = (frame.scalar_base as usize)
            .checked_add(frame.local_count as usize)
            .and_then(|base| base.checked_add(prefix))
            .ok_or(Failure::BackendUnavailable)?;
        if slot >= self.scalar_len || slot >= self.tags.len() {
            return Err(Failure::BackendUnavailable);
        }
        self.scalars[slot] = result;
        self.tags[slot] = tag;
        let frame = &mut self.frames[top_index];
        frame.block = block;
        frame.instruction = instruction;
        frame.operand_len = u32::try_from(prefix + 1).map_err(|_| Failure::BackendUnavailable)?;
        self.changed_from = self.changed_from.min(top_index);
        Ok(frame.operand_len)
    }
}

impl NativeTypeEnvironmentCache {
    /// Return a stable view for one native execution.
    pub fn view(&mut self) -> Result<NativeTypeEnvironmentView, Failure> {
        if self.entries.is_empty() {
            self.entries = new_type_environment_cache(INITIAL_TYPE_ENVIRONMENT_CACHE_SETS)
                .ok_or(Failure::BackendUnavailable)?;
        }
        let sets = self.entries.len() / TYPE_ENVIRONMENT_CACHE_WAYS;
        if !sets.is_power_of_two() {
            return Err(Failure::BackendUnavailable);
        }
        let mask = u32::try_from(sets - 1).map_err(|_| Failure::BackendUnavailable)?;
        Ok(NativeTypeEnvironmentView {
            entries: self.entries.as_ptr(),
            mask,
        })
    }

    /// Cache one environment-dependent value for one bytecode site.
    pub fn cache_type_site(
        &mut self,
        store: u64,
        function: u32,
        block: u32,
        instruction: u32,
        parent: u32,
        child: u32,
    ) -> bool {
        if store == 0 || store == TYPE_ENVIRONMENT_CACHE_CLAIMED {
            return false;
        }
        loop {
            let sets = self.entries.len() / TYPE_ENVIRONMENT_CACHE_WAYS;
            if !sets.is_power_of_two() {
                return false;
            }
            let first = type_environment_cache_set(function, block, instruction, parent, sets)
                * TYPE_ENVIRONMENT_CACHE_WAYS;
            let entries = &self.entries[first..first + TYPE_ENVIRONMENT_CACHE_WAYS];
            for entry in entries {
                if entry.matches(store, function, block, instruction, parent) {
                    return entry.child.load(Ordering::Relaxed) == child;
                }
            }
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.store.load(Ordering::Acquire) == 0)
            {
                entry.publish(store, function, block, instruction, parent, child);
                return true;
            }
            if !self.grow_type_environment_cache(sets) {
                return false;
            }
        }
    }

    fn grow_type_environment_cache(&mut self, current_sets: usize) -> bool {
        let mut target_sets = current_sets.saturating_mul(2);
        while target_sets <= MAX_TYPE_ENVIRONMENT_CACHE_SETS {
            let Some(target) = new_type_environment_cache(target_sets) else {
                return false;
            };
            if rehash_type_environment_cache(&self.entries, &target, target_sets) {
                self.entries = target;
                return true;
            }
            target_sets = target_sets.saturating_mul(2);
        }
        false
    }
}

impl NativeResolvedCallCache {
    /// Return a stable view for one native execution.
    pub fn view(&mut self) -> Result<NativeResolvedCallView, Failure> {
        if self.entries.is_empty() {
            self.entries = new_resolved_call_cache(INITIAL_RESOLVED_CALL_CACHE_SETS)
                .ok_or(Failure::BackendUnavailable)?;
        }
        let sets = self.entries.len() / RESOLVED_CALL_CACHE_WAYS;
        if !sets.is_power_of_two() {
            return Err(Failure::BackendUnavailable);
        }
        let mask = u32::try_from(sets - 1).map_err(|_| Failure::BackendUnavailable)?;
        Ok(NativeResolvedCallView {
            entries: self.entries.as_ptr(),
            mask,
        })
    }

    /// Cache one resolved target for one receiver shape.
    #[allow(clippy::too_many_arguments)]
    pub fn cache_call_site(
        &mut self,
        store: u64,
        function: u32,
        block: u32,
        instruction: u32,
        parent: u32,
        receiver: u64,
        target: u32,
        environment: u32,
        capture_data: usize,
        capture_len: usize,
    ) -> bool {
        if store == 0 || store == RESOLVED_CALL_CACHE_CLAIMED {
            return false;
        }
        let record = ResolvedCallCacheRecord {
            store,
            function,
            block,
            instruction,
            parent,
            receiver,
            target,
            environment,
            capture_data,
            capture_len,
        };
        loop {
            let sets = self.entries.len() / RESOLVED_CALL_CACHE_WAYS;
            if !sets.is_power_of_two() {
                return false;
            }
            let first =
                resolved_call_cache_set(function, block, instruction, parent, receiver, sets)
                    * RESOLVED_CALL_CACHE_WAYS;
            let entries = &self.entries[first..first + RESOLVED_CALL_CACHE_WAYS];
            for entry in entries {
                if entry.matches(store, function, block, instruction, parent, receiver) {
                    return entry.target.load(Ordering::Relaxed) == target
                        && entry.environment.load(Ordering::Relaxed) == environment
                        && entry.capture_data.load(Ordering::Relaxed) == capture_data
                        && entry.capture_len.load(Ordering::Relaxed) == capture_len;
                }
            }
            if let Some(entry) = entries
                .iter()
                .find(|entry| entry.store.load(Ordering::Acquire) == 0)
            {
                entry.publish(record);
                return true;
            }
            if self.grow_resolved_call_cache(sets) {
                continue;
            }
            let first =
                resolved_call_cache_set(function, block, instruction, parent, receiver, sets)
                    * RESOLVED_CALL_CACHE_WAYS;
            let replace = receiver as usize % RESOLVED_CALL_CACHE_WAYS;
            self.entries[first + replace].publish(record);
            return true;
        }
    }

    fn grow_resolved_call_cache(&mut self, current_sets: usize) -> bool {
        let mut target_sets = current_sets.saturating_mul(2);
        while target_sets <= MAX_RESOLVED_CALL_CACHE_SETS {
            let Some(target) = new_resolved_call_cache(target_sets) else {
                return false;
            };
            if rehash_resolved_call_cache(&self.entries, &target, target_sets) {
                self.entries = target;
                return true;
            }
            target_sets = target_sets.saturating_mul(2);
        }
        false
    }
}

fn new_type_environment_cache(sets: usize) -> Option<Vec<RawTypeEnvironmentCacheEntry>> {
    let count = sets.checked_mul(TYPE_ENVIRONMENT_CACHE_WAYS)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(count).ok()?;
    for _ in 0..count {
        entries.push(RawTypeEnvironmentCacheEntry::new());
    }
    Some(entries)
}

fn rehash_type_environment_cache(
    source: &[RawTypeEnvironmentCacheEntry],
    target: &[RawTypeEnvironmentCacheEntry],
    sets: usize,
) -> bool {
    for entry in source {
        let Some((store, function, block, instruction, parent, child)) = entry.snapshot() else {
            continue;
        };
        let first = type_environment_cache_set(function, block, instruction, parent, sets)
            * TYPE_ENVIRONMENT_CACHE_WAYS;
        let Some(empty) = target[first..first + TYPE_ENVIRONMENT_CACHE_WAYS]
            .iter()
            .find(|entry| entry.store.load(Ordering::Acquire) == 0)
        else {
            return false;
        };
        empty.publish(store, function, block, instruction, parent, child);
    }
    true
}

fn new_resolved_call_cache(sets: usize) -> Option<Vec<RawResolvedCallCacheEntry>> {
    let count = sets.checked_mul(RESOLVED_CALL_CACHE_WAYS)?;
    let mut entries = Vec::new();
    entries.try_reserve_exact(count).ok()?;
    for _ in 0..count {
        entries.push(RawResolvedCallCacheEntry::new());
    }
    Some(entries)
}

fn rehash_resolved_call_cache(
    source: &[RawResolvedCallCacheEntry],
    target: &[RawResolvedCallCacheEntry],
    sets: usize,
) -> bool {
    for entry in source {
        let Some(record) = entry.snapshot() else {
            continue;
        };
        let first = resolved_call_cache_set(
            record.function,
            record.block,
            record.instruction,
            record.parent,
            record.receiver,
            sets,
        ) * RESOLVED_CALL_CACHE_WAYS;
        let Some(empty) = target[first..first + RESOLVED_CALL_CACHE_WAYS]
            .iter()
            .find(|entry| entry.store.load(Ordering::Acquire) == 0)
        else {
            return false;
        };
        empty.publish(record);
    }
    true
}

fn growth_target(current: usize, required: usize, limit: usize) -> Result<usize, Failure> {
    if required > limit {
        return Err(Failure::BackendUnavailable);
    }
    let doubled = current.saturating_mul(2).min(limit);
    Ok(current.max(required).max(doubled))
}

pub(super) struct RawRuntimeContext<R> {
    pub(super) runtime: *mut R,
    pub(super) activation: *mut RawNativeActivation,
    pub(super) roots: *const u64,
    pub(super) root_tags: *const u64,
    pub(super) root_states: *const u8,
    pub(super) root_capacity: usize,
}

/// One complete root view for a native runtime operation.
#[derive(Clone, Copy)]
pub struct NativeRoots<'a> {
    bits: &'a [u64],
    tags: &'a [u64],
    states: &'a [u8],
    activation: &'a RawNativeActivation,
}

/// One failure while reading native roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeRootError {
    /// Native activation metadata is invalid.
    Invalid,
    /// Root storage cannot grow.
    Limit,
}

impl NativeRoots<'_> {
    /// Return the current-frame root payloads.
    pub fn bits(&self) -> &[u64] {
        self.bits
    }

    /// Return the current-frame root tags.
    pub fn tags(&self) -> &[u64] {
        self.tags
    }

    /// Return the current-frame root states.
    pub fn states(&self) -> &[u8] {
        self.states
    }

    /// Add every live object root from this native activation.
    pub fn extend_objects(&self, roots: &mut Vec<ObjRef>) -> Result<(), NativeRootError> {
        if self.bits.len() != self.tags.len() || self.bits.len() != self.states.len() {
            return Err(NativeRootError::Invalid);
        }
        let activation = self.activation;
        let frame_len = activation.frame_len as usize;
        let scalar_len = activation.scalar_len as usize;
        if frame_len == 0
            || frame_len > activation.frame_capacity as usize
            || scalar_len > activation.scalar_capacity as usize
            || activation.frames.is_null()
            || activation.scalars.is_null()
            || activation.tags.is_null()
            || activation.states.is_null()
        {
            return Err(NativeRootError::Invalid);
        }
        let reserve = self
            .bits
            .len()
            .checked_add(scalar_len)
            .and_then(|count| count.checked_add(frame_len))
            .ok_or(NativeRootError::Limit)?;
        roots
            .try_reserve(reserve)
            .map_err(|_| NativeRootError::Limit)?;
        extend_object_roots(roots, self.bits, self.tags, self.states);

        // SAFETY: The checks above bound every raw activation slice.
        let frames = unsafe { std::slice::from_raw_parts(activation.frames, frame_len) };
        // SAFETY: The checks above bound every raw activation slice.
        let scalars = unsafe { std::slice::from_raw_parts(activation.scalars, scalar_len) };
        // SAFETY: The checks above bound every raw activation slice.
        let tags = unsafe { std::slice::from_raw_parts(activation.tags, scalar_len) };
        // SAFETY: The checks above bound every raw activation slice.
        let states = unsafe { std::slice::from_raw_parts(activation.states, scalar_len) };
        for (index, frame) in frames.iter().enumerate() {
            if frame.capture_tag == ValueTag::Obj as u64 {
                roots.push(object_reference(frame.capture_bits));
            }
            if index + 1 == frame_len {
                continue;
            }
            let base = frame.scalar_base as usize;
            let local_count = frame.local_count as usize;
            let max_stack = frame.max_stack as usize;
            let operand_len = frame.operand_len as usize;
            let local_end = base
                .checked_add(local_count)
                .ok_or(NativeRootError::Invalid)?;
            let window_end = local_end
                .checked_add(max_stack)
                .ok_or(NativeRootError::Invalid)?;
            let operand_end = local_end
                .checked_add(operand_len)
                .ok_or(NativeRootError::Invalid)?;
            if operand_len > max_stack || window_end > scalar_len || operand_end > scalar_len {
                return Err(NativeRootError::Invalid);
            }
            extend_object_roots(
                roots,
                &scalars[base..local_end],
                &tags[base..local_end],
                &states[base..local_end],
            );
            for (&bits, &tag) in scalars[local_end..operand_end]
                .iter()
                .zip(&tags[local_end..operand_end])
            {
                if tag == ValueTag::Obj as u64 {
                    roots.push(object_reference(bits));
                }
            }
        }
        Ok(())
    }
}

fn extend_object_roots(roots: &mut Vec<ObjRef>, bits: &[u64], tags: &[u64], states: &[u8]) {
    roots.extend(
        bits.iter()
            .copied()
            .zip(tags.iter().copied())
            .zip(states.iter().copied())
            .filter(|((_, tag), state)| {
                *tag == ValueTag::Obj as u64 && state & LOCAL_INITIALIZED != 0
            })
            .map(|((bits, _), _)| object_reference(bits)),
    );
}

fn object_reference(bits: u64) -> ObjRef {
    ObjRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

unsafe fn runtime_roots<'a, R>(
    context: *const RawRuntimeContext<R>,
    count: usize,
) -> Option<NativeRoots<'a>> {
    if context.is_null() {
        return None;
    }
    // SAFETY: The native caller retains one live runtime context.
    let context = unsafe { &*context };
    if count > context.root_capacity || context.activation.is_null() {
        return None;
    }
    // SAFETY: The caller checked the shared root-buffer capacity.
    let bits = unsafe { std::slice::from_raw_parts(context.roots, count) };
    // SAFETY: Every root has one canonical tag slot.
    let tags = unsafe { std::slice::from_raw_parts(context.root_tags, count) };
    // SAFETY: Every root has one canonical state slot.
    let states = unsafe { std::slice::from_raw_parts(context.root_states, count) };
    // SAFETY: Native execution retains the activation during this call.
    let activation = unsafe { &*context.activation };
    Some(NativeRoots {
        bits,
        tags,
        states,
        activation,
    })
}

/// One checked instance-allocation result.
#[derive(Debug, Clone, Copy)]
pub enum AllocationResult {
    Value {
        bits: u64,
        heap: Option<JitHeapView>,
    },
    CollectionRequired,
    HeapLimit,
    Interpreter,
}

/// One checked value result from a fixed runtime helper.
#[derive(Debug, Clone, Copy)]
pub enum RuntimeValueResult {
    Value { bits: u64, tag: u64 },
    Missing,
    Fault(lm_abi::FaultCode),
    Interpreter,
}

/// One checked map insertion probe.
#[derive(Debug, Clone, Copy)]
pub enum MapPutProbeResult {
    Existing {
        position: u32,
        entry_count: u32,
        bits: u64,
        tag: u64,
    },
    Vacant {
        semantic_hash: i64,
        entry_count: u32,
    },
    Fault(lm_abi::FaultCode),
    Interpreter,
}

/// One checked runtime operation without a result value.
#[derive(Debug, Clone, Copy)]
pub enum RuntimeUnitResult {
    Done,
    Fault(lm_abi::FaultCode),
    Interpreter,
}

/// One checked heap operation result.
#[derive(Debug, Clone, Copy)]
pub enum HeapOperationResult {
    Value {
        bits: u64,
        heap: Option<JitHeapView>,
        /// True when `bits` identifies one new heap object.
        object: bool,
    },
    Fault(lm_abi::FaultCode),
    HeapLimit,
    Interpreter,
}

/// One fixed heap operation request.
pub struct HeapOperationRequest<'a> {
    pub first: u64,
    pub second: u64,
    pub third: u64,
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

/// One checked callback-allocation result.
#[derive(Debug, Clone, Copy)]
pub enum CallbackAllocationResult {
    Value { bits: u64 },
    StackLimit,
    Interpreter,
}

/// One typed closure-allocation request.
pub struct ClosureAllocationRequest<'a> {
    pub function: u32,
    pub environment: u32,
    pub capture_bits: &'a [u64],
    pub capture_tags: &'a [u64],
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

/// One typed callback-allocation request.
pub struct CallbackAllocationRequest<'a> {
    pub function: u32,
    pub environment: u32,
    pub capture_bits: &'a [u64],
    pub capture_tags: &'a [u64],
    pub owner_depth: u32,
}

/// One typed value-array allocation request.
pub struct ValueArrayAllocationRequest<'a> {
    /// Canonical item payloads.
    pub item_bits: &'a [u64],
    /// Canonical item tags.
    pub item_tags: &'a [u64],
    /// Complete native roots.
    pub roots: NativeRoots<'a>,
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// One typed graph-digest request.
pub struct DigestRequest<'a> {
    /// Canonical source object reference bits.
    pub reference: u64,
    /// Source module type index.
    pub ty: u32,
    /// Current closed type environment.
    pub environment: u32,
    /// Complete native roots.
    pub roots: NativeRoots<'a>,
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// Typed runtime slow paths for one native activation.
pub trait NativeRuntime {
    /// Record allocations completed without a runtime call.
    fn record_inline_allocations(&mut self, _count: u64) {}

    /// Record transient instance allocations and releases.
    fn record_pending_instances(&mut self, _allocations: u64, _releases: u64) {}

    /// Record constructor instances represented through scalar fields.
    fn record_scalar_replacements(&mut self, _allocations: u64) {}

    /// Allocate one instance with its exact environment and active roots.
    fn allocate_instance(
        &mut self,
        class: u32,
        environment: u32,
        roots: NativeRoots<'_>,
        allow_collection: bool,
    ) -> AllocationResult;

    /// Allocate one closure with exact captures and active roots.
    fn allocate_closure(&mut self, request: ClosureAllocationRequest<'_>) -> AllocationResult;

    /// Allocate one nonescaping callback with exact captures.
    fn allocate_callback(
        &mut self,
        request: CallbackAllocationRequest<'_>,
    ) -> CallbackAllocationResult;

    /// Allocate one tuple with exact item values.
    fn allocate_tuple(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult;

    /// Allocate one list with exact item values.
    fn allocate_list(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult;

    /// Allocate one map with exact key and value pairs.
    fn allocate_map(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult;

    /// Grow one list and append one canonical value.
    fn grow_list(&mut self, request: ListGrowthRequest<'_>) -> ListGrowthResult;

    /// Grow one list and insert one canonical value.
    fn insert_list(&mut self, request: ListInsertRequest<'_>) -> ListGrowthResult;

    /// Reserve additional capacity for one list.
    fn reserve_list(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult;

    /// Reserve additional capacity for one map.
    fn reserve_map(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult;

    /// Test one list item with structural value equality.
    fn list_contains(
        &mut self,
        reference: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult;

    /// Test one map key with the native map index.
    fn map_has(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult;

    /// Load one map value with the native map index.
    fn map_at(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult;

    /// Load one optional map value with the native map index.
    fn map_get(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult;

    /// Find the next live map entry for one iterator.
    fn map_next_index(&mut self, reference: u64, cursor: u64, expected: u64) -> RuntimeValueResult;

    /// Load one map key by its stable entry index.
    fn map_key_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult;

    /// Load one map value by its stable entry index.
    fn map_value_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult;

    /// Remove one optional map value by key.
    fn map_remove(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult;

    /// Remove all map entries.
    fn map_clear(&mut self, reference: u64) -> RuntimeValueResult;

    /// Continue one raw map-index probe.
    fn map_probe(&mut self, reference: u64, semantic: u64, prior: u64) -> RuntimeValueResult;

    /// Load one key from a raw map probe.
    fn map_probe_key(&mut self, reference: u64, token: u64) -> RuntimeValueResult;

    /// Load one value from a raw map probe.
    fn map_probe_value(&mut self, reference: u64, token: u64) -> RuntimeValueResult;

    /// Replace one value through a raw map probe.
    fn map_probe_set_value(
        &mut self,
        reference: u64,
        token: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult;

    /// Remove one value through a raw map probe.
    fn map_probe_remove(&mut self, reference: u64, token: u64) -> RuntimeValueResult;

    /// Insert one entry through a raw vacant probe.
    fn map_insert_hashed(&mut self, request: MapInsertHashedRequest<'_>) -> RuntimeUnitResult;

    /// Probe one map insertion without semantic mutation.
    fn map_put_probe(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> MapPutProbeResult;

    /// Insert one map value without returning its previous value.
    fn map_put_discard(&mut self, request: MapPutDiscardRequest<'_>) -> RuntimeUnitResult;

    /// Commit one previously probed map insertion.
    fn map_put_commit(&mut self, request: MapPutCommitRequest<'_>) -> RuntimeUnitResult;

    /// Intern one byte range as an owned String map key.
    fn map_intern_text_range(
        &mut self,
        request: MapInternTextRangeRequest<'_>,
    ) -> HeapOperationResult;

    /// Compare two canonical values with structural value equality.
    fn values_equal(
        &mut self,
        left_bits: u64,
        left_tag: u64,
        right_bits: u64,
        right_tag: u64,
    ) -> RuntimeValueResult;

    /// Compare two text values and return their signed ordering.
    fn compare_text(&mut self, left: u64, right: u64) -> RuntimeValueResult;

    /// Compare two byte values and return their signed ordering.
    fn compare_bytes(&mut self, left: u64, right: u64) -> RuntimeValueResult;

    /// Compute one stable text hash.
    fn hash_text(&mut self, reference: u64) -> RuntimeValueResult;

    /// Compute one stable byte hash.
    fn hash_bytes(&mut self, reference: u64) -> RuntimeValueResult;

    /// Freeze one complete reachable graph.
    fn freeze_graph(&mut self, reference: u64) -> RuntimeValueResult;

    /// Compute one typed graph digest and allocate its result.
    fn digest_value(&mut self, request: DigestRequest<'_>) -> AllocationResult;

    /// Convert one fault code to text.
    fn fault_code(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one policy-denied fault.
    fn fault_denied(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Package one canonical value with its closed static type.
    fn dyn_pack(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one syntax tree root.
    fn syntax_tree_root(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one syntax kind.
    fn syntax_kind(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one syntax category.
    fn syntax_category(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one syntax range start.
    fn syntax_range_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one syntax range end.
    fn syntax_range_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one syntax text view.
    fn syntax_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one list of immediate syntax children.
    fn syntax_children(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one compact syntax view.
    fn syntax_detach(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Build one syntax token.
    fn syntax_build_token(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Build one syntax trivia value.
    fn syntax_build_trivia(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Build one syntax node.
    fn syntax_build_node(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Convert one syntax node to a tree.
    fn syntax_to_tree(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Allocate one string builder.
    fn string_builder_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Append text to one string builder.
    fn string_builder_append_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult;

    /// Append one integer to one string builder.
    fn string_builder_append_int(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult;

    /// Append one Boolean value to one string builder.
    fn string_builder_append_bool(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult;

    /// Append one character to one string builder.
    fn string_builder_append_char(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult;

    /// Append one float to one string builder.
    fn string_builder_append_float(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult;

    /// Copy one string builder into immutable text.
    fn string_builder_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Finish one string builder.
    fn string_builder_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Allocate one byte buffer.
    fn byte_buffer_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Append one byte to one byte buffer.
    fn byte_buffer_append(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Copy one byte buffer into immutable bytes.
    fn byte_buffer_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Append immutable bytes to one byte buffer.
    fn byte_buffer_extend(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Reserve capacity in one byte buffer.
    fn byte_buffer_reserve(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Finish one byte buffer.
    fn byte_buffer_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Share immutable text as bytes.
    fn bytes_from_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one immutable byte slice.
    fn bytes_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Concatenate two immutable byte values.
    fn bytes_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compact one immutable byte value.
    fn bytes_compact(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one text view over immutable bytes.
    fn bytes_text_view(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Apply bitwise AND to two byte values.
    fn bytes_bit_and(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Apply bitwise OR to two byte values.
    fn bytes_bit_or(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Apply bitwise XOR to two byte values.
    fn bytes_bit_xor(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Invert one immutable byte value.
    fn bytes_bit_not(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Concatenate two text values.
    fn text_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test one text prefix.
    fn text_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test one text suffix.
    fn text_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test whether one text value contains another.
    fn text_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Find one text value by scalar index.
    fn text_find_scalar(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Find one text value by byte index.
    fn text_find_byte(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Remove outer whitespace from one text value.
    fn text_trim(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Remove leading whitespace from one text value.
    fn text_trim_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Remove trailing whitespace from one text value.
    fn text_trim_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Convert ASCII letters to lower case.
    fn text_lower_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Convert ASCII letters to upper case.
    fn text_upper_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Replace each text match.
    fn text_replace(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one integer parse status.
    fn text_parse_int_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one parsed integer value.
    fn text_parse_int_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Pad the start of one text value.
    fn text_pad_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Pad the end of one text value.
    fn text_pad_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test one immutable byte suffix.
    fn bytes_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test whether one byte value contains another.
    fn bytes_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Split one text value.
    fn text_split(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Split one text value into lines.
    fn text_lines(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one scalar-indexed text slice.
    fn text_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Create one byte-indexed text slice.
    fn text_slice_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Share one text value as immutable bytes.
    fn text_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Convert one text view to a string.
    fn text_to_string(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Decode immutable UTF-8 bytes as a string.
    fn bytes_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    fn bytes_text_range(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Find immutable bytes in one byte buffer.
    fn byte_buffer_find_from(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test one immutable byte prefix.
    fn bytes_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Find one immutable byte value.
    fn bytes_find_index(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Format immutable bytes as hexadecimal text.
    fn bytes_hex(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test whether immutable bytes contain valid UTF-8.
    fn bytes_is_utf8(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compute one SHA-256 digest.
    fn digest_sha256(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compute one CRC-32/ISO-HDLC checksum.
    fn digest_crc32(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compute one MD5 digest for compatibility protocols.
    fn digest_md5(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compress one immutable byte value.
    fn compress_encode(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Validate and retain one bounded decompression result.
    fn compress_decode_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Load one retained bounded decompression result.
    fn compress_decode_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one float parse status.
    fn text_parse_float_status(&mut self, request: HeapOperationRequest<'_>)
        -> HeapOperationResult;

    /// Return one parsed float value.
    fn text_parse_float_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Format one float with fixed precision.
    fn float_fixed(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one regular-expression compile status.
    fn regex_compile_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Compile one checked regular expression.
    fn regex_compile_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Copy one regular-expression source.
    fn regex_source(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Test one regular expression.
    fn regex_is_match(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Capture the first regular-expression match.
    fn regex_captures(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Count regular-expression matches.
    fn regex_count(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Split text with one regular expression.
    fn regex_split(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Replace all regular-expression matches.
    fn regex_replace_all(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one match start byte.
    fn regex_match_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one match end byte.
    fn regex_match_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Copy one complete match.
    fn regex_match_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Return one match group count.
    fn regex_match_group_count(&mut self, request: HeapOperationRequest<'_>)
        -> HeapOperationResult;

    /// Read one numbered match group.
    fn regex_match_group(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;

    /// Read one named match group.
    fn regex_match_named(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult;
}

/// One checked list-growth result.
#[derive(Debug, Clone, Copy)]
pub enum ListGrowthResult {
    Done { heap: JitHeapView },
    HeapLimit,
    Interpreter,
}

/// One typed request to append a value after list growth.
pub struct ListGrowthRequest<'a> {
    /// Canonical list reference bits.
    pub reference: u64,
    /// Canonical appended value bits.
    pub value_bits: u64,
    /// Canonical appended value tag.
    pub value_tag: u64,
    /// Complete native roots.
    pub roots: NativeRoots<'a>,
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// One typed request to insert a value after list growth.
pub struct ListInsertRequest<'a> {
    /// Canonical list reference bits.
    pub reference: u64,
    /// Requested insertion index.
    pub index: i64,
    /// Canonical inserted value bits.
    pub value_bits: u64,
    /// Canonical inserted value tag.
    pub value_tag: u64,
    /// Complete native roots.
    pub roots: NativeRoots<'a>,
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// One checked list-reserve result.
#[derive(Debug, Clone, Copy)]
pub enum CollectionReserveResult {
    Done { heap: JitHeapView },
    HeapLimit,
    Interpreter,
}

/// One typed request to reserve additional list capacity.
pub struct CollectionReserveRequest<'a> {
    /// Canonical list reference bits.
    pub reference: u64,
    /// Requested additional capacity.
    pub additional: i64,
    /// Complete native roots.
    pub roots: NativeRoots<'a>,
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// One typed map insertion commit.
pub struct MapPutCommitRequest<'a> {
    pub reference: u64,
    pub key_bits: u64,
    pub key_tag: u64,
    pub value_bits: u64,
    pub value_tag: u64,
    pub token: u64,
    pub entry_count: u64,
    pub vacant: bool,
    pub borrowed_string_key: bool,
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

/// One typed map insertion without a result value.
pub struct MapPutDiscardRequest<'a> {
    pub reference: u64,
    pub key_bits: u64,
    pub key_tag: u64,
    pub value_bits: u64,
    pub value_tag: u64,
    pub borrowed_string_key: bool,
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

/// One byte-range String interning request.
pub struct MapInternTextRangeRequest<'a> {
    pub map: u64,
    pub source: u64,
    pub start: i64,
    pub length: i64,
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

/// One raw hashed map insertion request.
pub struct MapInsertHashedRequest<'a> {
    pub reference: u64,
    pub key_bits: u64,
    pub key_tag: u64,
    pub value_bits: u64,
    pub value_tag: u64,
    pub semantic_hash: i64,
    pub token: i64,
    pub roots: NativeRoots<'a>,
    pub allow_collection: bool,
}

unsafe extern "C" fn prepare_instance_fields(count: u32, result: *mut u64) -> u32 {
    if result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let Ok(fields) = lm_heap::ValueArray::try_repeated(Value::Uninit, count as usize) else {
        return RUNTIME_HEAP_LIMIT;
    };
    let (data, len, capacity) = fields.into_raw_parts();
    // SAFETY: The native caller provides four writable result words.
    unsafe {
        result.write(data as usize as u64);
        result.add(1).write(len as u64);
        result.add(2).write(capacity as u64);
    }
    RUNTIME_OK
}

pub(super) unsafe extern "C" fn allocate_instance<R: NativeRuntime>(
    context: *mut c_void,
    class: u32,
    environment: u32,
    allow_collection: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    if allow_collection > 1 || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    let response = runtime.allocate_instance(class, environment, roots, allow_collection != 0);
    finish_object_allocation(context.activation, result, response)
}

pub(super) unsafe extern "C" fn digest_value<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    ty: u32,
    environment: u32,
    allow_collection: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() || allow_collection > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = runtime.digest_value(DigestRequest {
        reference,
        ty,
        environment,
        roots,
        allow_collection: allow_collection != 0,
    });
    finish_object_allocation(context.activation, result, response)
}

pub(super) unsafe extern "C" fn allocate_closure<R: NativeRuntime>(
    context: *mut c_void,
    function: u32,
    environment: u32,
    allow_collection: u32,
    capture_start: u32,
    capture_count: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() || allow_collection > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let root_count = root_count as usize;
    let capture_start = capture_start as usize;
    let capture_count = capture_count as usize;
    let Some(capture_end) = capture_start.checked_add(capture_count) else {
        return RUNTIME_INTERPRETER;
    };
    if capture_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count) }) else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = runtime.allocate_closure(ClosureAllocationRequest {
        function,
        environment,
        capture_bits: &roots.bits()[capture_start..capture_end],
        capture_tags: &roots.tags()[capture_start..capture_end],
        roots,
        allow_collection: allow_collection != 0,
    });
    finish_object_allocation(context.activation, result, response)
}

pub(super) unsafe extern "C" fn allocate_callback<R: NativeRuntime>(
    context: *mut c_void,
    function: u32,
    environment: u32,
    _allow_collection: u32,
    capture_start: u32,
    capture_count: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let root_count = root_count as usize;
    let capture_start = capture_start as usize;
    let capture_count = capture_count as usize;
    let Some(capture_end) = capture_start.checked_add(capture_count) else {
        return RUNTIME_INTERPRETER;
    };
    if capture_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count) }) else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The activation remains live during this call.
    let activation = unsafe { &*context.activation };
    let Some(owner_depth) = activation.base_frames.checked_add(activation.frame_len) else {
        return RUNTIME_STACK_LIMIT;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.allocate_callback(CallbackAllocationRequest {
        function,
        environment,
        capture_bits: &roots.bits()[capture_start..capture_end],
        capture_tags: &roots.tags()[capture_start..capture_end],
        owner_depth,
    }) {
        CallbackAllocationResult::Value { bits } => {
            // SAFETY: The caller provides one writable result slot.
            unsafe { result.write(bits) };
            RUNTIME_OK
        }
        CallbackAllocationResult::StackLimit => RUNTIME_STACK_LIMIT,
        CallbackAllocationResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn allocate_tuple<R: NativeRuntime>(
    context: *mut c_void,
    allow_collection: u32,
    item_start: u32,
    item_count: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked activation context.
    unsafe {
        allocate_values(
            context,
            allow_collection,
            item_start,
            item_count,
            root_count,
            result,
            R::allocate_tuple,
        )
    }
}

pub(super) unsafe extern "C" fn allocate_list<R: NativeRuntime>(
    context: *mut c_void,
    allow_collection: u32,
    item_start: u32,
    item_count: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked activation context.
    unsafe {
        allocate_values(
            context,
            allow_collection,
            item_start,
            item_count,
            root_count,
            result,
            R::allocate_list,
        )
    }
}

pub(super) unsafe extern "C" fn allocate_map<R: NativeRuntime>(
    context: *mut c_void,
    allow_collection: u32,
    item_start: u32,
    item_count: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked activation context.
    unsafe {
        allocate_values(
            context,
            allow_collection,
            item_start,
            item_count,
            root_count,
            result,
            R::allocate_map,
        )
    }
}

unsafe fn allocate_values<R: NativeRuntime>(
    context: *mut c_void,
    allow_collection: u32,
    item_start: u32,
    item_count: u32,
    root_count: u32,
    result: *mut u64,
    allocate: fn(&mut R, ValueArrayAllocationRequest<'_>) -> AllocationResult,
) -> u32 {
    if context.is_null() || result.is_null() || allow_collection > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let root_count = root_count as usize;
    let item_start = item_start as usize;
    let item_count = item_count as usize;
    let Some(item_end) = item_start.checked_add(item_count) else {
        return RUNTIME_INTERPRETER;
    };
    if item_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count) }) else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = allocate(
        runtime,
        ValueArrayAllocationRequest {
            item_bits: &roots.bits()[item_start..item_end],
            item_tags: &roots.tags()[item_start..item_end],
            roots,
            allow_collection: allow_collection != 0,
        },
    );
    finish_object_allocation(context.activation, result, response)
}

fn finish_object_allocation(
    activation: *mut RawNativeActivation,
    result: *mut u64,
    response: AllocationResult,
) -> u32 {
    match response {
        AllocationResult::Value { bits, heap } => {
            let slot_count = (bits as u32 as usize).saturating_add(1);
            // SAFETY: The native activation remains writable during this slow path.
            unsafe {
                (*activation).heap_slot_count = (*activation).heap_slot_count.max(slot_count);
                if let Some(heap) = heap {
                    update_heap_view(activation, heap);
                }
                result.write(bits);
            }
            RUNTIME_OK
        }
        AllocationResult::CollectionRequired => RUNTIME_COLLECTION_REQUIRED,
        AllocationResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        AllocationResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

/// Refresh one borrowed heap view after a runtime slow path.
unsafe fn update_heap_view(activation: *mut RawNativeActivation, heap: JitHeapView) {
    // SAFETY: The caller retains one writable native activation.
    unsafe {
        (*activation).heap_pages = heap.pages;
        (*activation).heap_page_count = heap.page_count;
        (*activation).heap_slot_count = heap.slot_count;
        (*activation).text_view_pages = heap.text_view_pages;
        (*activation).text_view_page_count = heap.text_view_page_count;
        (*activation).text_view_slot_count = heap.text_view_slot_count;
        (*activation).heap_slots = heap.slots;
        (*activation).heap_free = heap.free.cast();
        (*activation).heap_live = heap.live;
        (*activation).heap_used_bytes = heap.used_bytes;
        (*activation).heap_collection_threshold = heap.collection_threshold;
    }
}

pub(super) unsafe extern "C" fn grow_list<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    value_bits: u64,
    value_tag: u64,
    root_count: u32,
) -> u32 {
    if context.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.grow_list(ListGrowthRequest {
        reference,
        value_bits,
        value_tag,
        roots,
        allow_collection: true,
    }) {
        ListGrowthResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                update_heap_view(context.activation, heap);
            }
            RUNTIME_OK
        }
        ListGrowthResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        ListGrowthResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn insert_list<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    index: i64,
    value_bits: u64,
    value_tag: u64,
    root_count: u32,
) -> u32 {
    if context.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.insert_list(ListInsertRequest {
        reference,
        index,
        value_bits,
        value_tag,
        roots,
        allow_collection: true,
    }) {
        ListGrowthResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                update_heap_view(context.activation, heap);
            }
            RUNTIME_OK
        }
        ListGrowthResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        ListGrowthResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn reserve_list<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    additional: i64,
    root_count: u32,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { reserve_collection(context, reference, additional, root_count, R::reserve_list) }
}

pub(super) unsafe extern "C" fn reserve_map<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    additional: i64,
    root_count: u32,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { reserve_collection(context, reference, additional, root_count, R::reserve_map) }
}

unsafe fn reserve_collection<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    additional: i64,
    root_count: u32,
    operation: for<'a> fn(&mut R, CollectionReserveRequest<'a>) -> CollectionReserveResult,
) -> u32 {
    if context.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match operation(
        runtime,
        CollectionReserveRequest {
            reference,
            additional,
            roots,
            allow_collection: true,
        },
    ) {
        CollectionReserveResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                update_heap_view(context.activation, heap);
            }
            RUNTIME_OK
        }
        CollectionReserveResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        CollectionReserveResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn map_has<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { map_lookup(context, reference, key_bits, key_tag, result, R::map_has) }
}

pub(super) unsafe extern "C" fn list_contains<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    value_bits: u64,
    value_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe {
        map_lookup(
            context,
            reference,
            value_bits,
            value_tag,
            result,
            R::list_contains,
        )
    }
}

pub(super) unsafe extern "C" fn map_at<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { map_lookup(context, reference, key_bits, key_tag, result, R::map_at) }
}

pub(super) unsafe extern "C" fn map_get<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { map_lookup(context, reference, key_bits, key_tag, result, R::map_get) }
}

pub(super) unsafe extern "C" fn map_next_index<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    cursor: u64,
    expected: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe {
        map_lookup(
            context,
            reference,
            cursor,
            expected,
            result,
            R::map_next_index,
        )
    }
}

pub(super) unsafe extern "C" fn map_remove<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { map_lookup(context, reference, key_bits, key_tag, result, R::map_remove) }
}

pub(super) unsafe extern "C" fn map_probe<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    semantic: u64,
    prior: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { map_lookup(context, reference, semantic, prior, result, R::map_probe) }
}

pub(super) unsafe extern "C" fn map_key_at<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    index: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, reference, index, result, R::map_key_at) }
}

pub(super) unsafe extern "C" fn map_value_at<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    index: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, reference, index, result, R::map_value_at) }
}

pub(super) unsafe extern "C" fn map_probe_key<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    token: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, reference, token, result, R::map_probe_key) }
}

pub(super) unsafe extern "C" fn map_probe_value<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    token: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, reference, token, result, R::map_probe_value) }
}

pub(super) unsafe extern "C" fn map_probe_remove<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    token: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, reference, token, result, R::map_probe_remove) }
}

pub(super) unsafe extern "C" fn map_clear<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_unary(context, reference, result, R::map_clear) }
}

pub(super) unsafe extern "C" fn map_insert_hashed<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    value_bits: u64,
    value_tag: u64,
    semantic_hash: i64,
    token: i64,
    root_count: u32,
) -> u32 {
    if context.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.map_insert_hashed(MapInsertHashedRequest {
        reference,
        key_bits,
        key_tag,
        value_bits,
        value_tag,
        semantic_hash,
        token,
        roots,
        allow_collection: true,
    }) {
        RuntimeUnitResult::Done => RUNTIME_OK,
        RuntimeUnitResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeUnitResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

unsafe fn map_lookup<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
    lookup: fn(&mut R, u64, u64, u64) -> RuntimeValueResult,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match lookup(runtime, reference, key_bits, key_tag) {
        RuntimeValueResult::Value { bits, tag } => {
            // SAFETY: The caller provides two writable result words.
            unsafe {
                result.write(bits);
                result.add(1).write(tag);
            }
            RUNTIME_OK
        }
        RuntimeValueResult::Missing => RUNTIME_MAP_VACANT,
        RuntimeValueResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeValueResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn values_equal<R: NativeRuntime>(
    context: *mut c_void,
    left_bits: u64,
    left_tag: u64,
    right_bits: u64,
    right_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe {
        value_quaternary(
            context,
            left_bits,
            left_tag,
            right_bits,
            right_tag,
            result,
            R::values_equal,
        )
    }
}

pub(super) unsafe extern "C" fn map_probe_set_value<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    token: u64,
    value_bits: u64,
    value_tag: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe {
        value_quaternary(
            context,
            reference,
            token,
            value_bits,
            value_tag,
            result,
            R::map_probe_set_value,
        )
    }
}

unsafe fn value_quaternary<R: NativeRuntime>(
    context: *mut c_void,
    first: u64,
    second: u64,
    third: u64,
    fourth: u64,
    result: *mut u64,
    operation: fn(&mut R, u64, u64, u64, u64) -> RuntimeValueResult,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    write_runtime_value(operation(runtime, first, second, third, fourth), result)
}

pub(super) unsafe extern "C" fn text_compare<R: NativeRuntime>(
    context: *mut c_void,
    left: u64,
    right: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, left, right, result, R::compare_text) }
}

pub(super) unsafe extern "C" fn bytes_compare<R: NativeRuntime>(
    context: *mut c_void,
    left: u64,
    right: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_binary(context, left, right, result, R::compare_bytes) }
}

unsafe fn object_binary<R: NativeRuntime>(
    context: *mut c_void,
    left: u64,
    right: u64,
    result: *mut u64,
    operation: fn(&mut R, u64, u64) -> RuntimeValueResult,
) -> u32 {
    let Some(runtime) = (unsafe { runtime_pointer::<R>(context, result) }) else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The checked context retains one live runtime during this call.
    let runtime = unsafe { &mut *runtime };
    write_runtime_value(operation(runtime, left, right), result)
}

pub(super) unsafe extern "C" fn text_hash<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_unary(context, reference, result, R::hash_text) }
}

pub(super) unsafe extern "C" fn bytes_hash<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_unary(context, reference, result, R::hash_bytes) }
}

pub(super) unsafe extern "C" fn freeze_graph<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    result: *mut u64,
) -> u32 {
    // SAFETY: The native caller provides one checked runtime context.
    unsafe { object_unary(context, reference, result, R::freeze_graph) }
}

unsafe fn object_unary<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    result: *mut u64,
    operation: fn(&mut R, u64) -> RuntimeValueResult,
) -> u32 {
    let Some(runtime) = (unsafe { runtime_pointer::<R>(context, result) }) else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The checked context retains one live runtime during this call.
    let runtime = unsafe { &mut *runtime };
    write_runtime_value(operation(runtime, reference), result)
}

macro_rules! heap_operation_entry {
    ($entry:ident, $method:ident) => {
        pub(super) unsafe extern "C" fn $entry<R: NativeRuntime>(
            context: *mut c_void,
            first: u64,
            second: u64,
            third: u64,
            root_count: u32,
            result: *mut u64,
        ) -> u32 {
            // SAFETY: The native caller provides one checked activation context.
            unsafe {
                heap_operation(
                    context,
                    first,
                    second,
                    third,
                    root_count,
                    result,
                    R::$method,
                )
            }
        }
    };
}

heap_operation_entry!(fault_code, fault_code);
heap_operation_entry!(fault_denied, fault_denied);
heap_operation_entry!(dyn_pack, dyn_pack);
heap_operation_entry!(syntax_tree_root, syntax_tree_root);
heap_operation_entry!(syntax_kind, syntax_kind);
heap_operation_entry!(syntax_category, syntax_category);
heap_operation_entry!(syntax_range_start, syntax_range_start);
heap_operation_entry!(syntax_range_end, syntax_range_end);
heap_operation_entry!(syntax_text, syntax_text);
heap_operation_entry!(syntax_children, syntax_children);
heap_operation_entry!(syntax_detach, syntax_detach);
heap_operation_entry!(syntax_build_token, syntax_build_token);
heap_operation_entry!(syntax_build_trivia, syntax_build_trivia);
heap_operation_entry!(syntax_build_node, syntax_build_node);
heap_operation_entry!(syntax_to_tree, syntax_to_tree);
heap_operation_entry!(string_builder_new, string_builder_new);
heap_operation_entry!(string_builder_append_text, string_builder_append_text);
heap_operation_entry!(string_builder_append_int, string_builder_append_int);
heap_operation_entry!(string_builder_append_bool, string_builder_append_bool);
heap_operation_entry!(string_builder_append_char, string_builder_append_char);
heap_operation_entry!(string_builder_append_float, string_builder_append_float);
heap_operation_entry!(string_builder_build, string_builder_build);
heap_operation_entry!(string_builder_finish, string_builder_finish);
heap_operation_entry!(byte_buffer_new, byte_buffer_new);
heap_operation_entry!(byte_buffer_append, byte_buffer_append);
heap_operation_entry!(byte_buffer_build, byte_buffer_build);
heap_operation_entry!(byte_buffer_extend, byte_buffer_extend);
heap_operation_entry!(byte_buffer_reserve, byte_buffer_reserve);
heap_operation_entry!(byte_buffer_finish, byte_buffer_finish);
heap_operation_entry!(bytes_from_text, bytes_from_text);
heap_operation_entry!(bytes_slice, bytes_slice);
heap_operation_entry!(bytes_concat, bytes_concat);
heap_operation_entry!(bytes_compact, bytes_compact);
heap_operation_entry!(bytes_text_view, bytes_text_view);
heap_operation_entry!(bytes_bit_and, bytes_bit_and);
heap_operation_entry!(bytes_bit_or, bytes_bit_or);
heap_operation_entry!(bytes_bit_xor, bytes_bit_xor);
heap_operation_entry!(bytes_bit_not, bytes_bit_not);
heap_operation_entry!(text_concat, text_concat);
heap_operation_entry!(text_starts_with, text_starts_with);
heap_operation_entry!(text_ends_with, text_ends_with);
heap_operation_entry!(text_contains, text_contains);
heap_operation_entry!(text_find_scalar, text_find_scalar);
heap_operation_entry!(text_find_byte, text_find_byte);
heap_operation_entry!(text_trim, text_trim);
heap_operation_entry!(text_trim_start, text_trim_start);
heap_operation_entry!(text_trim_end, text_trim_end);
heap_operation_entry!(text_lower_ascii, text_lower_ascii);
heap_operation_entry!(text_upper_ascii, text_upper_ascii);
heap_operation_entry!(text_replace, text_replace);
heap_operation_entry!(text_parse_int_status, text_parse_int_status);
heap_operation_entry!(text_parse_int_value, text_parse_int_value);
heap_operation_entry!(text_pad_start, text_pad_start);
heap_operation_entry!(text_pad_end, text_pad_end);
heap_operation_entry!(bytes_ends_with, bytes_ends_with);
heap_operation_entry!(bytes_contains, bytes_contains);
heap_operation_entry!(text_split, text_split);
heap_operation_entry!(text_lines, text_lines);
heap_operation_entry!(text_slice, text_slice);
heap_operation_entry!(text_slice_bytes, text_slice_bytes);
heap_operation_entry!(text_bytes, text_bytes);
heap_operation_entry!(text_to_string, text_to_string);
heap_operation_entry!(bytes_text, bytes_text);
heap_operation_entry!(bytes_text_range, bytes_text_range);
heap_operation_entry!(byte_buffer_find_from, byte_buffer_find_from);
heap_operation_entry!(bytes_starts_with, bytes_starts_with);
heap_operation_entry!(bytes_find_index, bytes_find_index);
heap_operation_entry!(bytes_hex, bytes_hex);
heap_operation_entry!(bytes_is_utf8, bytes_is_utf8);
heap_operation_entry!(digest_sha256, digest_sha256);
heap_operation_entry!(digest_crc32, digest_crc32);
heap_operation_entry!(digest_md5, digest_md5);
heap_operation_entry!(compress_encode, compress_encode);
heap_operation_entry!(compress_decode_status, compress_decode_status);
heap_operation_entry!(compress_decode_value, compress_decode_value);
heap_operation_entry!(text_parse_float_status, text_parse_float_status);
heap_operation_entry!(text_parse_float_value, text_parse_float_value);
heap_operation_entry!(float_fixed, float_fixed);
heap_operation_entry!(regex_compile_status, regex_compile_status);
heap_operation_entry!(regex_compile_value, regex_compile_value);
heap_operation_entry!(regex_source, regex_source);
heap_operation_entry!(regex_is_match, regex_is_match);
heap_operation_entry!(regex_captures, regex_captures);
heap_operation_entry!(regex_count, regex_count);
heap_operation_entry!(regex_split, regex_split);
heap_operation_entry!(regex_replace_all, regex_replace_all);
heap_operation_entry!(regex_match_start, regex_match_start);
heap_operation_entry!(regex_match_end, regex_match_end);
heap_operation_entry!(regex_match_text, regex_match_text);
heap_operation_entry!(regex_match_group_count, regex_match_group_count);
heap_operation_entry!(regex_match_group, regex_match_group);
heap_operation_entry!(regex_match_named, regex_match_named);

#[allow(clippy::too_many_arguments)]
unsafe fn heap_operation<R: NativeRuntime>(
    context: *mut c_void,
    first: u64,
    second: u64,
    third: u64,
    root_count: u32,
    result: *mut u64,
    operation: fn(&mut R, HeapOperationRequest<'_>) -> HeapOperationResult,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = operation(
        runtime,
        HeapOperationRequest {
            first,
            second,
            third,
            roots,
            allow_collection: true,
        },
    );
    finish_heap_operation(context.activation, result, response)
}

fn finish_heap_operation(
    activation: *mut RawNativeActivation,
    result: *mut u64,
    response: HeapOperationResult,
) -> u32 {
    match response {
        HeapOperationResult::Value { bits, heap, object } => {
            // SAFETY: The checked caller provides one writable result word.
            unsafe {
                result.write(bits);
                if object {
                    let slot_count = (bits as u32 as usize).saturating_add(1);
                    (*activation).heap_slot_count = (*activation).heap_slot_count.max(slot_count);
                }
                if let Some(heap) = heap {
                    update_heap_view(activation, heap);
                }
            }
            RUNTIME_OK
        }
        HeapOperationResult::Fault(fault) => runtime_fault_status(fault),
        HeapOperationResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        HeapOperationResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

unsafe fn runtime_pointer<R: NativeRuntime>(
    context: *mut c_void,
    result: *mut u64,
) -> Option<*mut R> {
    if context.is_null() || result.is_null() {
        return None;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() {
        return None;
    }
    Some(context.runtime)
}

fn write_runtime_value(value: RuntimeValueResult, result: *mut u64) -> u32 {
    match value {
        RuntimeValueResult::Value { bits, tag } => {
            // SAFETY: The checked caller provides two writable result words.
            unsafe {
                result.write(bits);
                result.add(1).write(tag);
            }
            RUNTIME_OK
        }
        RuntimeValueResult::Missing => RUNTIME_MAP_VACANT,
        RuntimeValueResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeValueResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

fn runtime_fault_status(fault: lm_abi::FaultCode) -> u32 {
    let index = lm_abi::FAULT_CODES
        .iter()
        .position(|candidate| *candidate == fault)
        .and_then(|index| u32::try_from(index).ok());
    index.map_or(RUNTIME_INTERPRETER, |index| RUNTIME_FAULT_FLAG | index)
}

pub(super) unsafe extern "C" fn map_put_probe<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.map_put_probe(reference, key_bits, key_tag) {
        MapPutProbeResult::Existing {
            position,
            entry_count,
            bits,
            tag,
        } => {
            // SAFETY: The caller provides four writable result words.
            unsafe {
                result.write(bits);
                result.add(1).write(tag);
                result.add(2).write(u64::from(position));
                result.add(3).write(u64::from(entry_count));
            }
            RUNTIME_OK
        }
        MapPutProbeResult::Vacant {
            semantic_hash,
            entry_count,
        } => {
            // SAFETY: The caller provides four writable result words.
            unsafe {
                result.write(0);
                result.add(1).write(0);
                result.add(2).write(semantic_hash as u64);
                result.add(3).write(u64::from(entry_count));
            }
            RUNTIME_MAP_VACANT
        }
        MapPutProbeResult::Fault(fault) => runtime_fault_status(fault),
        MapPutProbeResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn map_put_commit<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    value_bits: u64,
    value_tag: u64,
    token: u64,
    entry_count: u64,
    vacant: u32,
    borrowed_string_key: u32,
    root_count: u32,
) -> u32 {
    if context.is_null() || vacant > 1 || borrowed_string_key > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.map_put_commit(MapPutCommitRequest {
        reference,
        key_bits,
        key_tag,
        value_bits,
        value_tag,
        token,
        entry_count,
        vacant: vacant != 0,
        borrowed_string_key: borrowed_string_key != 0,
        roots,
        allow_collection: true,
    }) {
        RuntimeUnitResult::Done => RUNTIME_OK,
        RuntimeUnitResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeUnitResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn map_put_discard<R: NativeRuntime>(
    context: *mut c_void,
    reference: u64,
    key_bits: u64,
    key_tag: u64,
    value_bits: u64,
    value_tag: u64,
    borrowed_string_key: u32,
    root_count: u32,
) -> u32 {
    if context.is_null() || borrowed_string_key > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.map_put_discard(MapPutDiscardRequest {
        reference,
        key_bits,
        key_tag,
        value_bits,
        value_tag,
        borrowed_string_key: borrowed_string_key != 0,
        roots,
        allow_collection: true,
    }) {
        RuntimeUnitResult::Done => RUNTIME_OK,
        RuntimeUnitResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeUnitResult::Interpreter => RUNTIME_INTERPRETER,
    }
}

pub(super) unsafe extern "C" fn map_intern_text_range<R: NativeRuntime>(
    context: *mut c_void,
    map: u64,
    source: u64,
    start: i64,
    length: i64,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The native caller supplies one bounded root count.
    let Some(roots) = (unsafe { runtime_roots(std::ptr::from_ref(context), root_count as usize) })
    else {
        return RUNTIME_INTERPRETER;
    };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = runtime.map_intern_text_range(MapInternTextRangeRequest {
        map,
        source,
        start,
        length,
        roots,
        allow_collection: true,
    });
    finish_heap_operation(context.activation, result, response)
}
