//! Native regions over verified scalar LMBC.

use cranelift_jit::JITModule;
use std::ffi::c_void;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const MAX_REGION_INSTRUCTIONS: usize = 65_536;
const MAX_REGION_LOCALS: usize = 1_024;
const MAX_REGION_STACK: usize = 1_024;
// This budget leaves ample host stack space around generated calls.
const NATIVE_STACK_BUDGET: u32 = 256 * 1024;
// This reserve covers call setup outside the reported Cranelift frame.
const NATIVE_FRAME_OVERHEAD: u32 = 128;

const EXIT_FUEL: u32 = 1;
const EXIT_RETURN: u32 = 2;
const EXIT_INTEGER_OVERFLOW: u32 = 3;
const EXIT_DIVIDE_BY_ZERO: u32 = 4;
const EXIT_INVALID_ENTRY: u32 = 6;
const EXIT_TYPE_MISMATCH: u32 = 7;
const EXIT_UNINITIALIZED_FIELD: u32 = 8;
const EXIT_CALL: u32 = 9;
const EXIT_HEAP_LIMIT: u32 = 11;
const EXIT_EFFECT: u32 = 12;
const EXIT_STACK_LIMIT: u32 = 13;
const EXIT_GROW_ACTIVATION: u32 = 14;
const EXIT_TYPE_RESOLUTION: u32 = 15;
const EXIT_REPLAY: u32 = 16;
const EXIT_LITERAL: u32 = 17;
const EXIT_UNREACHABLE: u32 = 18;
const EXIT_TYPE_ENVIRONMENT: u32 = 19;
const EXIT_INTERFACE_CALL: u32 = 20;
const EXIT_GENERIC_VIRTUAL_CALL: u32 = 21;
const EXIT_CALLBACK_CALL: u32 = 22;
const EXIT_GUEST_FAULT: u32 = 23;
const EXIT_GROW_ROOTS: u32 = 24;
const EXIT_BOUNDARY: u32 = 25;
const EXIT_POLL: u32 = 26;
const EXIT_STACK_ROLLOVER: u32 = 27;
const EXIT_INLINE_CALL: u32 = 28;

mod activation;
mod opcode;
mod summary;

pub use activation::{
    AllocationResult, CallbackAllocationRequest, CallbackAllocationResult,
    ClosureAllocationRequest, CollectionReserveRequest, CollectionReserveResult, DigestRequest,
    HeapOperationRequest, HeapOperationResult, ListGrowthRequest, ListGrowthResult,
    ListInsertRequest, MapInsertHashedRequest, MapPutCommitRequest, MapPutDiscardRequest,
    MapPutProbeResult, NativeActivation, NativeDispatchRow, NativeExecution, NativeFrameView,
    NativeImageSlot, NativeImageSlotView, NativeLiteralView, NativePendingInstance, NativePoll,
    NativePreparation, NativeResolvedCallCache, NativeResolvedCallView, NativeRootBuffers,
    NativeRootBuffersMut, NativeRootError, NativeRoots, NativeRuntime, NativeTypeEnvironmentCache,
    NativeTypeEnvironmentView, PollSchedule, RuntimeUnitResult, RuntimeValueResult,
    ValueArrayAllocationRequest, LOCAL_INITIALIZED,
};
use activation::{
    NativeFunction, NativeRuntimeFunctions, RawExit, RawNativeActivation, RawRuntimeContext,
};
pub use opcode::{
    instruction_treatment, ExitBehavior, FaultStack, InstructionTreatment, TreatmentClass,
};
pub use summary::{FunctionBehavior, FunctionBehaviors};

/// One native compilation or execution failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The function uses an unsupported operation or type.
    Unsupported(UnsupportedReason),
    /// The backend cannot compile or execute this region.
    BackendUnavailable,
}

/// One host-owned native compiler.
pub struct JitEngine {
    isa: OnceLock<Option<cranelift_codegen::isa::OwnedTargetIsa>>,
    compilation_attempts: AtomicU64,
    compiled_regions: AtomicU64,
    compiled_code_bytes: AtomicU64,
    compiled_segments: AtomicU64,
    compiled_call_sites: AtomicU64,
    compiled_inlined_call_sites: AtomicU64,
    compiled_heap_read_sites: AtomicU64,
    compiled_heap_write_sites: AtomicU64,
    compiled_allocation_sites: AtomicU64,
    compiled_effect_sites: AtomicU64,
    compiled_interpreter_sites: AtomicU64,
}

impl Default for JitEngine {
    fn default() -> JitEngine {
        JitEngine {
            isa: OnceLock::new(),
            compilation_attempts: AtomicU64::new(0),
            compiled_regions: AtomicU64::new(0),
            compiled_code_bytes: AtomicU64::new(0),
            compiled_segments: AtomicU64::new(0),
            compiled_call_sites: AtomicU64::new(0),
            compiled_inlined_call_sites: AtomicU64::new(0),
            compiled_heap_read_sites: AtomicU64::new(0),
            compiled_heap_write_sites: AtomicU64::new(0),
            compiled_allocation_sites: AtomicU64::new(0),
            compiled_effect_sites: AtomicU64::new(0),
            compiled_interpreter_sites: AtomicU64::new(0),
        }
    }
}

impl fmt::Debug for JitEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("JitEngine").finish()
    }
}

/// One immutable compiled function region.
pub struct CompiledRegion {
    function: u32,
    code_size: usize,
    native_stack_bytes: u32,
    plan: RegionPlan,
    entry: NativeFunction,
    call_entry: usize,
    type_environment_sites: Vec<TypeEnvironmentSite>,
    interface_call_sites: Vec<InterfaceCallSite>,
    generic_virtual_call_sites: Vec<GenericVirtualCallSite>,
    call_value_sites: Vec<CallValueSite>,
    // The module owns the executable memory behind `entry`.
    module: Mutex<Option<JITModule>>,
}

struct TypeEnvironmentSite {
    function: u32,
    block: u32,
    instruction: u32,
    application: u32,
}

/// One verified interface-call cache site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterfaceCallSite {
    function: u32,
    block: u32,
    instruction: u32,
    interface: u32,
    method: u32,
    receiver_type: u32,
    application: u32,
    receiver_kind: ScalarKind,
    parameter_count: usize,
}

impl InterfaceCallSite {
    /// Return the caller function.
    pub fn function(self) -> u32 {
        self.function
    }

    /// Return the bytecode block.
    pub fn block(self) -> u32 {
        self.block
    }

    /// Return the bytecode instruction.
    pub fn instruction(self) -> u32 {
        self.instruction
    }

    /// Return the interface table index.
    pub fn interface(self) -> u32 {
        self.interface
    }

    /// Return the interface method index.
    pub fn method(self) -> u32 {
        self.method
    }

    /// Return the declared receiver type.
    pub fn receiver_type(self) -> u32 {
        self.receiver_type
    }

    /// Return the method type application.
    pub fn application(self) -> u32 {
        self.application
    }

    /// Return the receiver scalar representation.
    pub fn receiver_kind(self) -> ScalarKind {
        self.receiver_kind
    }

    /// Return the receiver and argument count.
    pub fn parameter_count(self) -> usize {
        self.parameter_count
    }
}

/// One verified generic virtual-call cache site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenericVirtualCallSite {
    function: u32,
    block: u32,
    instruction: u32,
    selector: u32,
    application: u32,
    receiver_kind: ScalarKind,
    parameter_count: usize,
}

/// One verified first-class call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallValueSite {
    function: u32,
    block: u32,
    instruction: u32,
    parameter_count: usize,
    callback: bool,
}

impl CallValueSite {
    /// Return the caller function.
    pub fn function(self) -> u32 {
        self.function
    }

    /// Return the bytecode block.
    pub fn block(self) -> u32 {
        self.block
    }

    /// Return the bytecode instruction.
    pub fn instruction(self) -> u32 {
        self.instruction
    }

    /// Return the argument count.
    pub fn parameter_count(self) -> usize {
        self.parameter_count
    }

    /// Return true when the callable is one machine callback.
    pub fn is_callback(self) -> bool {
        self.callback
    }
}

impl GenericVirtualCallSite {
    /// Return the caller function.
    pub fn function(self) -> u32 {
        self.function
    }

    /// Return the bytecode block.
    pub fn block(self) -> u32 {
        self.block
    }

    /// Return the bytecode instruction.
    pub fn instruction(self) -> u32 {
        self.instruction
    }

    /// Return the selector table index.
    pub fn selector(self) -> u32 {
        self.selector
    }

    /// Return the method type application.
    pub fn application(self) -> u32 {
        self.application
    }

    /// Return the receiver scalar representation.
    pub fn receiver_kind(self) -> ScalarKind {
        self.receiver_kind
    }

    /// Return the receiver and argument count.
    pub fn parameter_count(self) -> usize {
        self.parameter_count
    }
}

/// One stable native call target for a namespace function slot.
#[repr(C)]
pub struct NativeEntryCell {
    code: AtomicUsize,
    local_count: AtomicU32,
    max_stack: AtomicU32,
    max_stack_values: AtomicU32,
    max_roots: AtomicU32,
    native_stack_bytes: AtomicU32,
}

impl NativeEntryCell {
    /// Create one unpublished native entry.
    pub fn new() -> NativeEntryCell {
        NativeEntryCell {
            code: AtomicUsize::new(0),
            local_count: AtomicU32::new(0),
            max_stack: AtomicU32::new(0),
            max_stack_values: AtomicU32::new(0),
            max_roots: AtomicU32::new(0),
            native_stack_bytes: AtomicU32::new(0),
        }
    }

    /// Publish one compiled region after its owner retains the region.
    pub fn publish(&self, region: &CompiledRegion) -> Result<(), Failure> {
        self.prepare(region)?;
        self.publish_prepared(region);
        Ok(())
    }

    /// Store one region contract before code publication.
    pub fn prepare(&self, region: &CompiledRegion) -> Result<(), Failure> {
        let local_count = u32::try_from(region.plan.local_kinds.len())
            .map_err(|_| Failure::BackendUnavailable)?;
        let max_stack =
            u32::try_from(region.plan.max_stack).map_err(|_| Failure::BackendUnavailable)?;
        let max_stack_values =
            u32::try_from(region.plan.max_stack_values).map_err(|_| Failure::BackendUnavailable)?;
        let max_roots =
            u32::try_from(region.plan.max_roots).map_err(|_| Failure::BackendUnavailable)?;
        self.local_count.store(local_count, Ordering::Relaxed);
        self.max_stack.store(max_stack, Ordering::Relaxed);
        self.max_stack_values
            .store(max_stack_values, Ordering::Relaxed);
        self.max_roots.store(max_roots, Ordering::Relaxed);
        self.native_stack_bytes
            .store(region.native_stack_bytes, Ordering::Relaxed);
        Ok(())
    }

    /// Publish code after its owner retains the prepared region.
    pub fn publish_prepared(&self, region: &CompiledRegion) {
        self.code.store(region.call_entry, Ordering::Release);
    }
}

impl Default for NativeEntryCell {
    fn default() -> NativeEntryCell {
        NativeEntryCell::new()
    }
}

impl fmt::Debug for NativeEntryCell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeEntryCell")
            .field("published", &(self.code.load(Ordering::Acquire) != 0))
            .finish()
    }
}

impl fmt::Debug for CompiledRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledRegion")
            .field("segments", &self.plan.segments.len())
            .finish()
    }
}

impl Drop for CompiledRegion {
    fn drop(&mut self) {
        let module = self
            .module
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(module) = module {
            // SAFETY: The final region owner has released every native call.
            unsafe { module.free_memory() };
        }
    }
}

impl CompiledRegion {
    /// Return the namespace function slot.
    #[inline(always)]
    pub fn function(&self) -> u32 {
        self.function
    }

    /// Return the emitted machine-code size.
    #[inline(always)]
    pub fn code_size(&self) -> usize {
        self.code_size
    }

    /// Return the local scalar representations.
    #[inline(always)]
    pub fn local_kinds(&self) -> &[ScalarKind] {
        &self.plan.local_kinds
    }

    /// Return the function result representation.
    #[inline(always)]
    pub fn result_kind(&self) -> ScalarKind {
        self.plan.result_kind
    }

    /// Return the largest native operand depth.
    #[inline(always)]
    pub fn max_stack(&self) -> usize {
        self.plan.max_stack
    }

    /// Return the largest native collection-root count.
    #[inline(always)]
    pub fn max_roots(&self) -> usize {
        self.plan.max_roots
    }

    /// Return true when native execution can collect this machine.
    #[inline(always)]
    pub fn requires_complete_roots(&self) -> bool {
        self.plan.allocation_sites != 0 || self.plan.collection_sites != 0
    }

    /// Return true when this region reads the direct heap view.
    #[inline(always)]
    pub fn requires_heap_view(&self) -> bool {
        self.plan.heap_read_sites != 0 || self.plan.heap_write_sites != 0
    }

    /// Return the largest complete stack use above the root locals.
    #[inline(always)]
    pub fn max_stack_values(&self) -> usize {
        self.plan.max_stack_values
    }

    /// Return the plan for one exact program position.
    #[inline(always)]
    pub fn entry_plan(&self, block: u32, instruction: u32) -> Option<EntryPlan<'_>> {
        let index = self.plan.entries.get(&(block, instruction)).copied()?;
        let segment = &self.plan.segments[index];
        if segment.carries_reserved_prefix {
            return None;
        }
        Some(EntryPlan {
            index: index as u32,
            live_locals: &segment.live_in,
            operand_kinds: &segment.entry_stack,
        })
    }

    /// Return one internal entry for a retained native activation.
    #[inline(always)]
    pub fn resume_plan(&self, block: u32, instruction: u32) -> Option<EntryPlan<'_>> {
        self.entry_plan(block, instruction)
    }

    /// Return the distance from an interior position to its next entry.
    #[inline(always)]
    pub fn distance_to_entry(&self, block: u32, instruction: u32) -> Option<u32> {
        self.plan.distance_to_entry(block, instruction)
    }

    /// Return the operand representations at one exact entry.
    #[inline(always)]
    pub fn operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.operand_kinds(block, instruction)
    }

    /// Return operand representations for one native fault exit.
    #[inline(always)]
    pub fn fault_operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.fault_operand_kinds(block, instruction)
    }

    /// Return operand representations for one guarded interpreter replay.
    #[inline(always)]
    pub fn replay_operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.operand_kinds(block, instruction)
    }

    /// Return operand representations for one suspended native caller.
    pub fn suspended_operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.suspended_operand_kinds(block, instruction)
    }

    /// Return operand representations for one exact materialization exit.
    pub fn materialization_operand_kinds(
        &self,
        kind: ExitKind,
        block: u32,
        instruction: u32,
    ) -> Option<&[ScalarKind]> {
        self.plan
            .materialization_operand_kinds(kind, block, instruction)
    }

    /// Return the type application of one environment site.
    pub fn type_environment_application(&self, block: u32, instruction: u32) -> Option<u32> {
        self.type_environment_sites
            .iter()
            .find(|site| site.block == block && site.instruction == instruction)
            .map(|site| site.application)
    }

    /// Return one interface-call cache site.
    pub fn interface_call_site(&self, block: u32, instruction: u32) -> Option<InterfaceCallSite> {
        self.interface_call_sites
            .iter()
            .find(|site| site.block == block && site.instruction == instruction)
            .copied()
    }

    /// Return one generic virtual-call cache site.
    pub fn generic_virtual_call_site(
        &self,
        block: u32,
        instruction: u32,
    ) -> Option<GenericVirtualCallSite> {
        self.generic_virtual_call_sites
            .iter()
            .find(|site| site.block == block && site.instruction == instruction)
            .copied()
    }

    /// Return one first-class call site.
    pub fn call_value_site(&self, block: u32, instruction: u32) -> Option<CallValueSite> {
        self.call_value_sites
            .iter()
            .find(|site| site.block == block && site.instruction == instruction)
            .copied()
    }

    /// Execute native code over explicit scalar buffers.
    #[inline(always)]
    pub fn execute<R: NativeRuntime>(
        &self,
        runtime: &mut R,
        activation: &mut NativeActivation,
        input: NativeExecution<'_>,
    ) -> Result<ExecutionExit, Failure> {
        let NativeExecution {
            entry,
            entries,
            base_stack_values,
            max_stack_values,
            base_frames,
            max_frames,
            roots,
            root_tags,
            root_states,
            fuel,
            poll,
            heap,
            class_parents,
            dispatch_rows,
            dispatch_methods,
            literals,
            type_store_id,
            type_environments,
            resolved_calls,
            image_slots,
        } = input;
        let top_index = activation
            .frame_len
            .checked_sub(1)
            .ok_or(Failure::BackendUnavailable)?;
        if entry as usize >= self.plan.segments.len()
            || activation.frames[top_index].local_count as usize != self.plan.local_kinds.len()
            || (activation.frames[top_index].max_stack as usize) < self.plan.max_stack
            || roots.len() < self.plan.max_roots.max(1)
            || root_tags.len() < self.plan.max_roots.max(1)
            || root_states.len() < self.plan.max_roots.max(1)
            || ((self.plan.allocation_sites != 0 || self.plan.collection_sites != 0)
                && (heap.used_bytes.is_null()
                    || heap.slots.is_null()
                    || heap.free.is_null()
                    || heap.live.is_null()))
            || ((!self.type_environment_sites.is_empty() || self.plan.type_resolution_sites != 0)
                && (type_store_id == 0 || type_environments.entries.is_null()))
            || ((!self.interface_call_sites.is_empty()
                || !self.generic_virtual_call_sites.is_empty()
                || self.call_value_sites.iter().any(|site| site.callback))
                && (type_store_id == 0 || resolved_calls.entries.is_null()))
            || (image_slots.count != 0 && image_slots.entries.is_null())
        {
            return Err(Failure::BackendUnavailable);
        }
        let scalar_capacity =
            u32::try_from(activation.scalars.len()).map_err(|_| Failure::BackendUnavailable)?;
        let frame_capacity =
            u32::try_from(activation.frames.len()).map_err(|_| Failure::BackendUnavailable)?;
        let root_capacity = roots.len().min(root_tags.len()).min(root_states.len());
        let root_capacity =
            u32::try_from(root_capacity).map_err(|_| Failure::BackendUnavailable)?;
        if activation.frames[top_index].native_created == 0 {
            activation.frames[top_index].caller_stack_values =
                u32::try_from(base_stack_values).map_err(|_| Failure::BackendUnavailable)?;
        }
        activation.frames[top_index].native_stack_bytes = self.native_stack_bytes;
        let initial_fuel = poll.initial_fuel(fuel);
        let mut exit = RawExit::default();
        let mut runtime_result = [0u64; 4];
        let runtime_functions: &'static _ = &NativeRuntimeFunctions::<R>::TABLE;
        let virtual_available = activation.virtual_instances.iter().enumerate().fold(
            0u64,
            |available, (index, record)| {
                if record.active == 0 {
                    available | (1u64 << index)
                } else {
                    available
                }
            },
        );
        let mut raw_activation = RawNativeActivation {
            runtime_context: std::ptr::null_mut(),
            runtime_functions: std::ptr::from_ref(runtime_functions),
            allocation_result: runtime_result.as_mut_ptr(),
            roots: roots.as_mut_ptr(),
            root_tags: root_tags.as_mut_ptr(),
            root_states: root_states.as_mut_ptr(),
            exit: std::ptr::from_mut(&mut exit),
            scalars: activation.scalars.as_mut_ptr(),
            tags: activation.tags.as_mut_ptr(),
            states: activation.states.as_mut_ptr(),
            scalar_len: u32::try_from(activation.scalar_len)
                .map_err(|_| Failure::BackendUnavailable)?,
            scalar_capacity,
            frames: activation.frames.as_mut_ptr(),
            frame_len: u32::try_from(activation.frame_len)
                .map_err(|_| Failure::BackendUnavailable)?,
            frame_capacity,
            changed_from: u32::try_from(activation.changed_from)
                .map_err(|_| Failure::BackendUnavailable)?,
            virtual_instances: activation.virtual_instances.as_mut_ptr(),
            virtual_values: activation.virtual_values.as_mut_ptr(),
            virtual_available,
            virtual_request: 0,
            virtual_reserved: 0,
            root_code: self.call_entry,
            entries: entries.as_ptr(),
            entry_count: u32::try_from(entries.len()).map_err(|_| Failure::BackendUnavailable)?,
            max_stack_values: u32::try_from(max_stack_values)
                .map_err(|_| Failure::BackendUnavailable)?,
            base_frames: u32::try_from(base_frames).map_err(|_| Failure::BackendUnavailable)?,
            max_frames: u32::try_from(max_frames).map_err(|_| Failure::BackendUnavailable)?,
            root_capacity,
            heap_pages: heap.pages,
            heap_page_count: heap.page_count,
            heap_slot_count: heap.slot_count,
            heap_slots: heap.slots,
            heap_free: heap.free.cast(),
            heap_live: heap.live,
            heap_used_bytes: heap.used_bytes,
            heap_collection_threshold: heap.collection_threshold,
            inline_allocations: 0,
            pending_instance_allocations: 0,
            pending_instance_releases: 0,
            scalar_replaced_allocations: 0,
            lookup_hash_key: heap.lookup_hash_key,
            class_parents: class_parents.as_ptr(),
            class_count: class_parents.len(),
            dispatch_rows: dispatch_rows.as_ptr(),
            dispatch_row_count: dispatch_rows.len(),
            dispatch_methods: dispatch_methods.as_ptr(),
            dispatch_method_count: dispatch_methods.len(),
            literal_values: literals.values,
            literal_count: literals.count,
            type_store_id,
            type_environments: type_environments.entries,
            type_environment_mask: type_environments.mask,
            resolved_calls: resolved_calls.entries,
            resolved_call_mask: resolved_calls.mask,
            image_slots: image_slots.entries,
            image_slot_count: image_slots.count,
            poll_requested: poll.requested_pointer(),
            hard_fuel: fuel,
            poll_deadline: initial_fuel,
            poll_interval: poll.interval(),
        };
        let mut runtime_context = RawRuntimeContext {
            runtime: std::ptr::from_mut(runtime),
            activation: std::ptr::from_mut(&mut raw_activation),
            roots: roots.as_ptr(),
            root_tags: root_tags.as_ptr(),
            root_states: root_states.as_ptr(),
            root_capacity: root_capacity as usize,
        };
        raw_activation.runtime_context = std::ptr::from_mut(&mut runtime_context).cast::<c_void>();
        // SAFETY: The compiler bounds every access by the checked buffer lengths.
        // The generated function uses the exact `NativeFunction` C ABI.
        unsafe { (self.entry)(&mut raw_activation, entry) }
        runtime.record_inline_allocations(raw_activation.inline_allocations);
        runtime.record_pending_instances(
            raw_activation.pending_instance_allocations,
            raw_activation.pending_instance_releases,
        );
        runtime.record_scalar_replacements(raw_activation.scalar_replaced_allocations);
        if raw_activation.scalar_len > raw_activation.scalar_capacity
            || raw_activation.frame_len > raw_activation.frame_capacity
            || raw_activation.frame_len == 0
            || raw_activation.changed_from > raw_activation.frame_len
        {
            return Err(Failure::BackendUnavailable);
        }
        let top = activation.frames[raw_activation.frame_len as usize - 1];
        let end = (top.scalar_base as usize)
            .checked_add(top.local_count as usize)
            .and_then(|value| value.checked_add(top.max_stack as usize));
        if end.is_none_or(|value| value > raw_activation.scalar_len as usize)
            || top.operand_len > top.max_stack
        {
            return Err(Failure::BackendUnavailable);
        }
        activation.scalar_len = raw_activation.scalar_len as usize;
        activation.frame_len = raw_activation.frame_len as usize;
        activation.changed_from = raw_activation.changed_from as usize;
        let kind = decode_exit_kind(exit.kind).ok_or(Failure::BackendUnavailable)?;
        Ok(ExecutionExit {
            retired: exit.retired,
            kind,
            block: exit.block,
            instruction: exit.instruction,
            stack_len: exit.stack_len,
            result_tag: exit.result_tag,
            result: exit.result,
        })
    }
}

mod plan;

use plan::RegionPlan;
pub use plan::{
    type_has_native_representation, CompilerMetrics, EntryPlan, ExecutionExit, ExitKind,
    FunctionInput, ScalarKind, UnsupportedReason,
};

fn decode_exit_kind(kind: u32) -> Option<ExitKind> {
    Some(match kind {
        EXIT_FUEL => ExitKind::Fuel,
        EXIT_RETURN => ExitKind::Return,
        EXIT_INTEGER_OVERFLOW => ExitKind::IntegerOverflow,
        EXIT_DIVIDE_BY_ZERO => ExitKind::DivideByZero,
        EXIT_TYPE_MISMATCH => ExitKind::TypeMismatch,
        EXIT_UNINITIALIZED_FIELD => ExitKind::UninitializedField,
        EXIT_CALL => ExitKind::Call,
        EXIT_HEAP_LIMIT => ExitKind::HeapLimit,
        EXIT_EFFECT => ExitKind::Effect,
        EXIT_STACK_LIMIT => ExitKind::StackLimit,
        EXIT_GROW_ACTIVATION => ExitKind::GrowActivation,
        EXIT_TYPE_RESOLUTION => ExitKind::TypeResolution,
        EXIT_REPLAY => ExitKind::Replay,
        EXIT_LITERAL => ExitKind::Literal,
        EXIT_UNREACHABLE => ExitKind::Unreachable,
        EXIT_TYPE_ENVIRONMENT => ExitKind::TypeEnvironment,
        EXIT_INTERFACE_CALL => ExitKind::InterfaceCall,
        EXIT_GENERIC_VIRTUAL_CALL => ExitKind::GenericVirtualCall,
        EXIT_CALLBACK_CALL => ExitKind::CallbackCall,
        EXIT_GUEST_FAULT => ExitKind::GuestFault,
        EXIT_GROW_ROOTS => ExitKind::GrowRoots,
        EXIT_BOUNDARY => ExitKind::Boundary,
        EXIT_POLL => ExitKind::Poll,
        EXIT_STACK_ROLLOVER => ExitKind::StackRollover,
        EXIT_INLINE_CALL => ExitKind::InlineCall,
        EXIT_INVALID_ENTRY => return None,
        _ => return None,
    })
}
impl JitEngine {
    /// Compile one verified function for its current arena layout.
    #[cold]
    #[inline(never)]
    pub fn compile(&self, input: FunctionInput<'_>) -> Result<Arc<CompiledRegion>, Failure> {
        self.compilation_attempts.fetch_add(1, Ordering::Relaxed);
        let isa = self
            .isa
            .get_or_init(|| backend::native_isa().ok())
            .clone()
            .ok_or(Failure::BackendUnavailable)?;
        match compile_region(input, isa) {
            Ok(region) => {
                self.compiled_regions.fetch_add(1, Ordering::Relaxed);
                self.compiled_code_bytes
                    .fetch_add(region.code_size as u64, Ordering::Relaxed);
                self.compiled_segments
                    .fetch_add(region.plan.segments.len() as u64, Ordering::Relaxed);
                self.compiled_call_sites
                    .fetch_add(region.plan.call_sites as u64, Ordering::Relaxed);
                self.compiled_inlined_call_sites
                    .fetch_add(region.plan.inlined_call_sites as u64, Ordering::Relaxed);
                self.compiled_heap_read_sites
                    .fetch_add(region.plan.heap_read_sites as u64, Ordering::Relaxed);
                self.compiled_heap_write_sites
                    .fetch_add(region.plan.heap_write_sites as u64, Ordering::Relaxed);
                self.compiled_allocation_sites
                    .fetch_add(region.plan.allocation_sites as u64, Ordering::Relaxed);
                self.compiled_effect_sites
                    .fetch_add(region.plan.effect_sites as u64, Ordering::Relaxed);
                self.compiled_interpreter_sites
                    .fetch_add(region.plan.interpreter_sites as u64, Ordering::Relaxed);
                Ok(Arc::new(region))
            }
            Err(CompileError::Unsupported(reason)) => Err(Failure::Unsupported(reason)),
            Err(CompileError::Backend) => Err(Failure::BackendUnavailable),
        }
    }

    /// Return the current clock-free compilation counters.
    pub fn metrics(&self) -> CompilerMetrics {
        CompilerMetrics {
            compilation_attempts: self.compilation_attempts.load(Ordering::Relaxed),
            compiled_regions: self.compiled_regions.load(Ordering::Relaxed),
            compiled_code_bytes: self.compiled_code_bytes.load(Ordering::Relaxed),
            compiled_segments: self.compiled_segments.load(Ordering::Relaxed),
            compiled_call_sites: self.compiled_call_sites.load(Ordering::Relaxed),
            compiled_inlined_call_sites: self.compiled_inlined_call_sites.load(Ordering::Relaxed),
            compiled_heap_read_sites: self.compiled_heap_read_sites.load(Ordering::Relaxed),
            compiled_heap_write_sites: self.compiled_heap_write_sites.load(Ordering::Relaxed),
            compiled_allocation_sites: self.compiled_allocation_sites.load(Ordering::Relaxed),
            compiled_effect_sites: self.compiled_effect_sites.load(Ordering::Relaxed),
            compiled_interpreter_sites: self.compiled_interpreter_sites.load(Ordering::Relaxed),
        }
    }

    /// Reset every clock-free compilation counter.
    pub fn reset_metrics(&self) {
        self.compilation_attempts.store(0, Ordering::Relaxed);
        self.compiled_regions.store(0, Ordering::Relaxed);
        self.compiled_code_bytes.store(0, Ordering::Relaxed);
        self.compiled_segments.store(0, Ordering::Relaxed);
        self.compiled_call_sites.store(0, Ordering::Relaxed);
        self.compiled_inlined_call_sites.store(0, Ordering::Relaxed);
        self.compiled_heap_read_sites.store(0, Ordering::Relaxed);
        self.compiled_heap_write_sites.store(0, Ordering::Relaxed);
        self.compiled_allocation_sites.store(0, Ordering::Relaxed);
        self.compiled_effect_sites.store(0, Ordering::Relaxed);
        self.compiled_interpreter_sites.store(0, Ordering::Relaxed);
    }
}

/// Return true when one function fits the current native planner limits.
///
/// Planning can still reject unsupported types or function shapes.
pub fn is_candidate(function: &lm_bytecode::Func) -> bool {
    if function.local_types.len() > MAX_REGION_LOCALS {
        return false;
    }
    let mut instructions = 0usize;
    for _instruction in function.blocks.iter().flatten() {
        instructions = match instructions.checked_add(1) {
            Some(value) if value <= MAX_REGION_INSTRUCTIONS => value,
            _ => return false,
        };
    }
    instructions != 0
}

mod backend;

use backend::{compile_region, CompileError};
#[cfg(test)]
mod tests;
