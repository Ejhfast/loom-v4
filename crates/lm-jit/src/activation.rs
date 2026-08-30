//! Native activation storage and typed slow-path boundaries.

use crate::Failure;
use lm_heap::JitHeapView;
use lm_value::Value;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub(super) const RUNTIME_OK: u32 = 0;
const RUNTIME_INTERPRETER: u32 = 1;
pub(super) const RUNTIME_HEAP_LIMIT: u32 = 2;

const INITIAL_NATIVE_SCALARS: usize = 4_096;
const INITIAL_NATIVE_FRAMES: usize = 256;
pub(super) const TYPE_ENVIRONMENT_CACHE_WAYS: usize = 4;
const INITIAL_TYPE_ENVIRONMENT_CACHE_SETS: usize = 16;
const MAX_TYPE_ENVIRONMENT_CACHE_SETS: usize = 1_024;
const TYPE_ENVIRONMENT_CACHE_CLAIMED: u64 = u64::MAX;

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
    pub(super) literal_values: *const Value,
    pub(super) literal_count: usize,
    pub(super) type_store_id: u64,
    pub(super) type_environments: *const RawTypeEnvironmentCacheEntry,
    pub(super) type_environment_mask: u32,
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

pub(super) type RawAllocateInstance =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, *mut u64) -> u32;
pub(super) type RawGrowList = unsafe extern "C" fn(*mut c_void, u64, u64, u64, u32) -> u32;
pub(super) type RawReserveList = unsafe extern "C" fn(*mut c_void, u64, i64, u32) -> u32;

/// Fixed native entry points for typed runtime slow paths.
#[repr(C)]
pub(super) struct RawNativeFunctions {
    pub(super) allocate_instance: RawAllocateInstance,
    pub(super) grow_list: RawGrowList,
    pub(super) reserve_list: RawReserveList,
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

/// One fixed native view of a machine-local type-environment cache.
#[derive(Debug, Clone, Copy)]
pub struct NativeTypeEnvironmentView {
    pub(super) entries: *const RawTypeEnvironmentCacheEntry,
    pub(super) mask: u32,
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
    pub literals: NativeLiteralView,
    pub type_store_id: u64,
    pub type_environments: NativeTypeEnvironmentView,
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

    /// Grow one list and append one canonical value.
    fn grow_list(&mut self, request: ListGrowthRequest<'_>) -> ListGrowthResult;

    /// Reserve additional capacity for one list.
    fn reserve_list(&mut self, request: ListReserveRequest<'_>) -> ListReserveResult;
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
    match response {
        AllocationResult::Value { bits, heap } => {
            let slot_count = (bits as u32 as usize).saturating_add(1);
            // SAFETY: The native activation remains writable during the slow path.
            unsafe {
                (*context.activation).heap_slot_count =
                    (*context.activation).heap_slot_count.max(slot_count);
                if let Some(heap) = heap {
                    (*context.activation).heap_pages = heap.pages;
                    (*context.activation).heap_page_count = heap.page_count;
                    (*context.activation).heap_slot_count = heap.slot_count;
                    (*context.activation).heap_used_bytes = heap.used_bytes;
                    (*context.activation).heap_collection_threshold = heap.collection_threshold;
                }
            }
            // SAFETY: The caller provides one writable result slot.
            unsafe { result.write(bits) };
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
