//! Native activation storage and typed slow-path boundaries.

use crate::Failure;
use lm_heap::JitHeapView;
use lm_value::Value;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

pub(super) const RUNTIME_OK: u32 = 0;
const RUNTIME_INTERPRETER: u32 = 1;
pub(super) const RUNTIME_HEAP_LIMIT: u32 = 2;
pub(super) const RUNTIME_STACK_LIMIT: u32 = 3;
pub(super) const RUNTIME_FAULT_FLAG: u32 = 1 << 31;
pub(super) const RUNTIME_MAP_VACANT: u32 = 4;

const INITIAL_NATIVE_SCALARS: usize = 4_096;
const INITIAL_NATIVE_FRAMES: usize = 256;
pub(super) const TYPE_ENVIRONMENT_CACHE_WAYS: usize = 4;
const INITIAL_TYPE_ENVIRONMENT_CACHE_SETS: usize = 16;
const MAX_TYPE_ENVIRONMENT_CACHE_SETS: usize = 1_024;
const TYPE_ENVIRONMENT_CACHE_CLAIMED: u64 = u64::MAX;
pub(super) const RESOLVED_CALL_CACHE_WAYS: usize = 4;
const INITIAL_RESOLVED_CALL_CACHE_SETS: usize = 16;
const MAX_RESOLVED_CALL_CACHE_SETS: usize = 1_024;
const RESOLVED_CALL_CACHE_CLAIMED: u64 = u64::MAX;

/// The native local changed during this activation.
pub const LOCAL_DIRTY: u8 = 1;
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
    pub(super) entries: *const usize,
    pub(super) entry_count: u32,
    pub(super) max_stack_values: u32,
    pub(super) base_frames: u32,
    pub(super) max_frames: u32,
    pub(super) heap_pages: *const usize,
    pub(super) heap_page_count: usize,
    pub(super) heap_slot_count: usize,
    pub(super) heap_used_bytes: *mut usize,
    pub(super) heap_collection_threshold: usize,
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
pub(super) type RawMapPutCommit =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, u64, u64, u64, u32, u32) -> u32;
pub(super) type RawMapPutDiscard =
    unsafe extern "C" fn(*mut c_void, u64, u64, u64, u64, u64, u32) -> u32;

/// Fixed native entry points for typed runtime slow paths.
#[repr(C)]
pub(super) struct RawNativeFunctions {
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
    pub(super) map_put_discard: RawMapPutDiscard,
    pub(super) map_put_probe: RawMapLookup,
    pub(super) map_put_commit: RawMapPutCommit,
    pub(super) value_equal: RawValueEqual,
    pub(super) text_compare: RawObjectBinary,
    pub(super) bytes_compare: RawObjectBinary,
    pub(super) text_hash: RawObjectUnary,
    pub(super) bytes_hash: RawObjectUnary,
    pub(super) freeze_graph: RawObjectUnary,
    pub(super) digest_value: RawDigest,
}

pub(super) type NativeFunction = unsafe extern "C" fn(
    *mut u64,
    *mut u64,
    *mut u8,
    *mut u64,
    *mut u64,
    u64,
    u32,
    *mut c_void,
    *const RawNativeFunctions,
    *mut u64,
    *mut u64,
    *mut u64,
    *mut u8,
    *mut RawExit,
    *mut RawNativeActivation,
);

/// Reusable scalar and frame storage for one native turn.
#[derive(Debug, Default)]
pub struct NativeActivation {
    pub(super) scalars: Vec<u64>,
    pub(super) tags: Vec<u64>,
    pub(super) states: Vec<u8>,
    pub(super) frames: Vec<RawNativeFrame>,
    pub(super) scalar_len: usize,
    pub(super) frame_len: usize,
    pub(super) changed_from: usize,
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
    pub heap: JitHeapView,
    pub class_parents: &'a [u32],
    pub dispatch_rows: &'a [NativeDispatchRow],
    pub dispatch_methods: &'a [u32],
    pub literals: NativeLiteralView,
    pub type_store_id: u64,
    pub type_environments: NativeTypeEnvironmentView,
    pub resolved_calls: NativeResolvedCallView,
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

    /// Return the local initialization and mutation states.
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
        {
            return Err(Failure::BackendUnavailable);
        }
        self.scalars.resize(scalar_capacity, 0);
        self.tags.resize(scalar_capacity, 0);
        self.states.resize(scalar_capacity, 0);
        self.frames
            .resize(frame_capacity, RawNativeFrame::default());
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
        };
        self.scalar_len = window;
        self.frame_len = 1;
        self.changed_from = 0;
        Ok(())
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

/// One checked instance-allocation result.
#[derive(Debug, Clone, Copy)]
pub enum AllocationResult {
    Value {
        bits: u64,
        heap: Option<JitHeapView>,
    },
    HeapLimit,
    Interpreter,
}

/// One checked value result from a fixed runtime helper.
#[derive(Debug, Clone, Copy)]
pub enum RuntimeValueResult {
    Value { bits: u64, tag: u64 },
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
    pub root_bits: &'a [u64],
    pub root_tags: &'a [u64],
    pub root_states: &'a [u8],
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
    /// Active root payloads.
    pub root_bits: &'a [u64],
    /// Active root tags.
    pub root_tags: &'a [u64],
    /// Active root states.
    pub root_states: &'a [u8],
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
    /// Active root payloads.
    pub root_bits: &'a [u64],
    /// Active root tags.
    pub root_tags: &'a [u64],
    /// Active root states.
    pub root_states: &'a [u8],
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// Typed runtime slow paths for one native activation.
pub trait NativeRuntime {
    /// Allocate one instance with its exact environment and active roots.
    fn allocate_instance(
        &mut self,
        class: u32,
        environment: u32,
        root_bits: &[u64],
        root_tags: &[u64],
        root_states: &[u8],
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
    fn reserve_list(&mut self, request: ListReserveRequest<'_>) -> ListReserveResult;

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

    /// Probe one map insertion without semantic mutation.
    fn map_put_probe(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> MapPutProbeResult;

    /// Insert one map value without returning its previous value.
    fn map_put_discard(&mut self, request: MapPutDiscardRequest<'_>) -> RuntimeUnitResult;

    /// Commit one previously probed map insertion.
    fn map_put_commit(&mut self, request: MapPutCommitRequest<'_>) -> RuntimeUnitResult;

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
    /// Active root payloads.
    pub root_bits: &'a [u64],
    /// Active root tags.
    pub root_tags: &'a [u64],
    /// Active root states.
    pub root_states: &'a [u8],
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
    /// Active root payloads.
    pub root_bits: &'a [u64],
    /// Active root tags.
    pub root_tags: &'a [u64],
    /// Active root states.
    pub root_states: &'a [u8],
    /// True when this frame can collect.
    pub allow_collection: bool,
}

/// One checked list-reserve result.
#[derive(Debug, Clone, Copy)]
pub enum ListReserveResult {
    Done { heap: JitHeapView },
    HeapLimit,
    Interpreter,
}

/// One typed request to reserve additional list capacity.
pub struct ListReserveRequest<'a> {
    /// Canonical list reference bits.
    pub reference: u64,
    /// Requested additional capacity.
    pub additional: i64,
    /// Active root payloads.
    pub root_bits: &'a [u64],
    /// Active root tags.
    pub root_tags: &'a [u64],
    /// Active root states.
    pub root_states: &'a [u8],
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
    pub root_bits: &'a [u64],
    pub root_tags: &'a [u64],
    pub root_states: &'a [u8],
    pub allow_collection: bool,
}

/// One typed map insertion without a result value.
pub struct MapPutDiscardRequest<'a> {
    pub reference: u64,
    pub key_bits: u64,
    pub key_tag: u64,
    pub value_bits: u64,
    pub value_tag: u64,
    pub root_bits: &'a [u64],
    pub root_tags: &'a [u64],
    pub root_states: &'a [u8],
    pub allow_collection: bool,
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
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside the activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let nested = unsafe { (*context.activation).frame_len > 1 };
    let response = runtime.allocate_instance(
        class,
        environment,
        root_bits,
        root_tags,
        root_states,
        allow_collection != 0 && !nested,
    );
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
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Every root has one canonical state slot.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let nested = unsafe { (*context.activation).frame_len > 1 };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = runtime.digest_value(DigestRequest {
        reference,
        ty,
        environment,
        root_bits,
        root_tags,
        root_states,
        allow_collection: allow_collection != 0 && !nested,
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
    if root_count > context.root_capacity || capture_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let nested = unsafe { (*context.activation).frame_len > 1 };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = runtime.allocate_closure(ClosureAllocationRequest {
        function,
        environment,
        capture_bits: &root_bits[capture_start..capture_end],
        capture_tags: &root_tags[capture_start..capture_end],
        root_bits,
        root_tags,
        root_states,
        allow_collection: allow_collection != 0 && !nested,
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
    if root_count > context.root_capacity || capture_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
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
        capture_bits: &root_bits[capture_start..capture_end],
        capture_tags: &root_tags[capture_start..capture_end],
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
    if root_count > context.root_capacity || item_end > root_count {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let nested = unsafe { (*context.activation).frame_len > 1 };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    let response = allocate(
        runtime,
        ValueArrayAllocationRequest {
            item_bits: &root_bits[item_start..item_end],
            item_tags: &root_tags[item_start..item_end],
            root_bits,
            root_tags,
            root_states,
            allow_collection: allow_collection != 0 && !nested,
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
                    (*activation).heap_pages = heap.pages;
                    (*activation).heap_page_count = heap.page_count;
                    (*activation).heap_slot_count = heap.slot_count;
                    (*activation).heap_used_bytes = heap.used_bytes;
                    (*activation).heap_collection_threshold = heap.collection_threshold;
                }
                result.write(bits);
            }
            RUNTIME_OK
        }
        AllocationResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        AllocationResult::Interpreter => RUNTIME_INTERPRETER,
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
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside the activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    // SAFETY: The activation remains live during this call.
    let allow_collection = unsafe { (*context.activation).frame_len <= 1 };
    match runtime.grow_list(ListGrowthRequest {
        reference,
        value_bits,
        value_tag,
        root_bits,
        root_tags,
        root_states,
        allow_collection,
    }) {
        ListGrowthResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                (*context.activation).heap_pages = heap.pages;
                (*context.activation).heap_page_count = heap.page_count;
                (*context.activation).heap_slot_count = heap.slot_count;
                (*context.activation).heap_used_bytes = heap.used_bytes;
                (*context.activation).heap_collection_threshold = heap.collection_threshold;
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
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside the activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    // SAFETY: The activation remains live during this call.
    let allow_collection = unsafe { (*context.activation).frame_len <= 1 };
    match runtime.insert_list(ListInsertRequest {
        reference,
        index,
        value_bits,
        value_tag,
        root_bits,
        root_tags,
        root_states,
        allow_collection,
    }) {
        ListGrowthResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                (*context.activation).heap_pages = heap.pages;
                (*context.activation).heap_page_count = heap.page_count;
                (*context.activation).heap_slot_count = heap.slot_count;
                (*context.activation).heap_used_bytes = heap.used_bytes;
                (*context.activation).heap_collection_threshold = heap.collection_threshold;
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
    if context.is_null() {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside the activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    // SAFETY: The activation remains live during this call.
    let allow_collection = unsafe { (*context.activation).frame_len <= 1 };
    match runtime.reserve_list(ListReserveRequest {
        reference,
        additional,
        root_bits,
        root_tags,
        root_states,
        allow_collection,
    }) {
        ListReserveResult::Done { heap } => {
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                (*context.activation).heap_pages = heap.pages;
                (*context.activation).heap_page_count = heap.page_count;
                (*context.activation).heap_slot_count = heap.slot_count;
                (*context.activation).heap_used_bytes = heap.used_bytes;
                (*context.activation).heap_collection_threshold = heap.collection_threshold;
            }
            RUNTIME_OK
        }
        ListReserveResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        ListReserveResult::Interpreter => RUNTIME_INTERPRETER,
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
    match runtime.values_equal(left_bits, left_tag, right_bits, right_tag) {
        RuntimeValueResult::Value { bits, tag } => {
            // SAFETY: The caller provides two writable result words.
            unsafe {
                result.write(bits);
                result.add(1).write(tag);
            }
            RUNTIME_OK
        }
        RuntimeValueResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeValueResult::Interpreter => RUNTIME_INTERPRETER,
    }
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
    root_count: u32,
) -> u32 {
    if context.is_null() || vacant > 1 {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawRuntimeContext<R>>() };
    if context.runtime.is_null() || context.activation.is_null() {
        return RUNTIME_INTERPRETER;
    }
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let allow_collection = unsafe { (*context.activation).frame_len <= 1 };
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
        root_bits,
        root_tags,
        root_states,
        allow_collection,
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
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return RUNTIME_INTERPRETER;
    }
    // SAFETY: The checked count stays inside each activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Every root has one canonical tag slot.
    let root_tags = unsafe { std::slice::from_raw_parts(context.root_tags, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let allow_collection = unsafe { (*context.activation).frame_len <= 1 };
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    match runtime.map_put_discard(MapPutDiscardRequest {
        reference,
        key_bits,
        key_tag,
        value_bits,
        value_tag,
        root_bits,
        root_tags,
        root_states,
        allow_collection,
    }) {
        RuntimeUnitResult::Done => RUNTIME_OK,
        RuntimeUnitResult::Fault(fault) => runtime_fault_status(fault),
        RuntimeUnitResult::Interpreter => RUNTIME_INTERPRETER,
    }
}
