//! Native regions over verified scalar LMBC.

use cranelift_jit::JITModule;
use std::ffi::c_void;
use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const MAX_REGION_INSTRUCTIONS: usize = 65_536;
const MAX_REGION_LOCALS: usize = 1_024;
const MAX_REGION_STACK: usize = 1_024;

const EXIT_FUEL: u32 = 1;
const EXIT_RETURN: u32 = 2;
const EXIT_INTEGER_OVERFLOW: u32 = 3;
const EXIT_DIVIDE_BY_ZERO: u32 = 4;
const EXIT_INTERPRETER: u32 = 5;
const EXIT_INVALID_ENTRY: u32 = 6;
const EXIT_TYPE_MISMATCH: u32 = 7;
const EXIT_UNINITIALIZED_FIELD: u32 = 8;
const EXIT_CALL: u32 = 9;
const EXIT_ALLOCATION: u32 = 10;
const EXIT_HEAP_LIMIT: u32 = 11;
const EXIT_EFFECT: u32 = 12;
const EXIT_STACK_LIMIT: u32 = 13;
const EXIT_GROW_ACTIVATION: u32 = 14;

mod activation;

use activation::{
    allocate_instance, NativeFunction, RawAllocationContext, RawExit, RawNativeActivation,
};
pub use activation::{
    AllocationResult, AllocationRuntime, NativeActivation, NativeExecution, NativeFrameView,
    NativePreparation, LOCAL_DIRTY, LOCAL_INITIALIZED,
};

/// One native compilation or execution failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// The function uses an unsupported operation or type.
    Unsupported,
    /// The backend cannot compile or execute this region.
    BackendUnavailable,
}

/// One host-owned native compiler.
#[derive(Default)]
pub struct JitEngine {
    compilation_attempts: AtomicU64,
    compiled_regions: AtomicU64,
    compiled_segments: AtomicU64,
    compiled_call_sites: AtomicU64,
    compiled_heap_read_sites: AtomicU64,
    compiled_heap_write_sites: AtomicU64,
    compiled_allocation_sites: AtomicU64,
    compiled_effect_sites: AtomicU64,
    compiled_interpreter_sites: AtomicU64,
}

impl fmt::Debug for JitEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("JitEngine").finish()
    }
}

/// One immutable compiled function region.
pub struct CompiledRegion {
    function: u32,
    plan: RegionPlan,
    entry: NativeFunction,
    call_entry: usize,
    // The module owns the executable memory behind `entry`.
    module: Mutex<Option<JITModule>>,
}

/// One stable native call target for a namespace function slot.
#[repr(C)]
pub struct NativeEntryCell {
    code: AtomicUsize,
    local_count: AtomicU32,
    max_stack: AtomicU32,
    max_stack_values: AtomicU32,
}

impl NativeEntryCell {
    /// Create one unpublished native entry.
    pub fn new() -> NativeEntryCell {
        NativeEntryCell {
            code: AtomicUsize::new(0),
            local_count: AtomicU32::new(0),
            max_stack: AtomicU32::new(0),
            max_stack_values: AtomicU32::new(0),
        }
    }

    /// Publish one compiled region after its owner retains the region.
    pub fn publish(&self, region: &CompiledRegion) -> Result<(), Failure> {
        let local_count = u32::try_from(region.plan.local_kinds.len())
            .map_err(|_| Failure::BackendUnavailable)?;
        let max_stack =
            u32::try_from(region.plan.max_stack).map_err(|_| Failure::BackendUnavailable)?;
        let max_stack_values =
            u32::try_from(region.plan.max_stack_values).map_err(|_| Failure::BackendUnavailable)?;
        self.local_count.store(local_count, Ordering::Relaxed);
        self.max_stack.store(max_stack, Ordering::Relaxed);
        self.max_stack_values
            .store(max_stack_values, Ordering::Relaxed);
        self.code.store(region.call_entry, Ordering::Release);
        Ok(())
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
        self.plan.allocation_sites != 0
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

    /// Return the largest additional native call depth.
    #[inline(always)]
    pub fn additional_frames(&self) -> u32 {
        self.plan.additional_frames
    }

    /// Return the plan for one exact program position.
    #[inline(always)]
    pub fn entry_plan(&self, block: u32, instruction: u32) -> Option<EntryPlan<'_>> {
        let index = self.plan.entries.get(&(block, instruction)).copied()?;
        let segment = &self.plan.segments[index];
        Some(EntryPlan {
            index: index as u32,
            live_locals: &segment.live_in,
            operand_kinds: &segment.entry_stack,
        })
    }

    /// Return one internal entry for a retained native activation.
    #[inline(always)]
    pub fn resume_plan(&self, block: u32, instruction: u32) -> Option<EntryPlan<'_>> {
        if let Some(entry) = self.entry_plan(block, instruction) {
            return Some(entry);
        }
        let index = self
            .plan
            .resume_entries
            .get(&(block, instruction))
            .copied()?;
        let target_index = index.checked_sub(self.plan.segments.len() as u32)? as usize;
        let target = self.plan.resume_targets.get(target_index)?;
        let segment = self.plan.segments.get(target.segment)?;
        let (_, operand_kinds) = segment.fuel_stacks.get(target.offset)?;
        Some(EntryPlan {
            index,
            live_locals: &segment.live_in,
            operand_kinds,
        })
    }

    /// Return the distance from an interior position to its next entry.
    #[inline(always)]
    pub fn distance_to_entry(&self, block: u32, instruction: u32) -> Option<u32> {
        self.plan.distance_to_entry(block, instruction)
    }

    /// Return the operand representations at one exact entry.
    #[inline(always)]
    pub fn operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        if let Some(index) = self.plan.entries.get(&(block, instruction)).copied() {
            return Some(&self.plan.segments[index].entry_stack);
        }
        self.plan
            .segments
            .iter()
            .find(|segment| {
                segment.block == block
                    && segment.end.checked_sub(1) == Some(instruction)
                    && matches!(
                        segment.exit,
                        SegmentExit::Call { .. }
                            | SegmentExit::Effect { .. }
                            | SegmentExit::Interpreter { .. }
                    )
            })
            .map(|segment| segment.boundary_stack.as_slice())
            .or_else(|| {
                self.plan.segments.iter().find_map(|segment| {
                    segment
                        .fuel_stacks
                        .iter()
                        .find(|(at, _)| segment.block == block && *at == instruction)
                        .map(|(_, stack)| stack.as_slice())
                })
            })
            .or_else(|| {
                self.plan.segments.iter().find_map(|segment| {
                    segment
                        .replay_stacks
                        .iter()
                        .find(|(at, _)| segment.block == block && *at == instruction)
                        .map(|(_, stack)| stack.as_slice())
                })
            })
    }

    /// Return operand representations for one native fault exit.
    #[inline(always)]
    pub fn fault_operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.segments.iter().find_map(|segment| {
            segment
                .fault_stacks
                .iter()
                .find(|(at, _)| segment.block == block && *at == instruction)
                .map(|(_, stack)| stack.as_slice())
        })
    }

    /// Return operand representations for one suspended native caller.
    pub fn suspended_operand_kinds(&self, block: u32, instruction: u32) -> Option<&[ScalarKind]> {
        self.plan.segments.iter().find_map(|segment| {
            let SegmentExit::Call {
                target,
                fallthrough_ip,
            } = segment.exit
            else {
                return None;
            };
            if segment.block != block || fallthrough_ip != instruction {
                return None;
            }
            let parameters = self.plan.call_contracts.get(&target)?.params.len();
            let prefix = segment.boundary_stack.len().checked_sub(parameters)?;
            Some(&segment.boundary_stack[..prefix])
        })
    }

    /// Execute native code over explicit scalar buffers.
    #[inline(always)]
    pub fn execute<R: AllocationRuntime>(
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
            root_states,
            fuel,
            heap,
            class_parents,
        } = input;
        let top_index = activation
            .frame_len
            .checked_sub(1)
            .ok_or(Failure::BackendUnavailable)?;
        if entry as usize
            >= self
                .plan
                .segments
                .len()
                .checked_add(self.plan.resume_targets.len())
                .ok_or(Failure::BackendUnavailable)?
            || activation.frames[top_index].local_count as usize != self.plan.local_kinds.len()
            || (activation.frames[top_index].max_stack as usize) < self.plan.max_stack
            || roots.len() < self.plan.max_roots.max(1)
            || root_states.len() < self.plan.max_roots.max(1)
        {
            return Err(Failure::BackendUnavailable);
        }
        let scalar_capacity =
            u32::try_from(activation.scalars.len()).map_err(|_| Failure::BackendUnavailable)?;
        let frame_capacity =
            u32::try_from(activation.frames.len()).map_err(|_| Failure::BackendUnavailable)?;
        if activation.frames[top_index].native_created == 0 {
            activation.frames[top_index].caller_stack_values =
                u32::try_from(base_stack_values).map_err(|_| Failure::BackendUnavailable)?;
        }
        let top = activation.frames[top_index];
        let mut raw_activation = RawNativeActivation {
            scalars: activation.scalars.as_mut_ptr(),
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
            entries: entries.as_ptr(),
            entry_count: u32::try_from(entries.len()).map_err(|_| Failure::BackendUnavailable)?,
            max_stack_values: u32::try_from(max_stack_values)
                .map_err(|_| Failure::BackendUnavailable)?,
            base_frames: u32::try_from(base_frames).map_err(|_| Failure::BackendUnavailable)?,
            max_frames: u32::try_from(max_frames).map_err(|_| Failure::BackendUnavailable)?,
            heap_pages: heap.pages,
            heap_page_count: heap.page_count,
            heap_slot_count: heap.slot_count,
            class_parents: class_parents.as_ptr(),
            class_count: class_parents.len(),
        };
        let mut exit = RawExit::default();
        let mut allocation_result = 0u64;
        let mut allocation_context = RawAllocationContext {
            runtime: std::ptr::from_mut(runtime),
            activation: std::ptr::from_mut(&mut raw_activation),
            roots: roots.as_ptr(),
            root_states: root_states.as_ptr(),
            root_capacity: self.plan.max_roots,
        };
        // SAFETY: Each checked frame names one complete scalar window.
        let local_pointer = unsafe {
            activation
                .scalars
                .as_mut_ptr()
                .add(top.scalar_base as usize)
        };
        // SAFETY: The state table uses the same scalar window indexes.
        let local_state_pointer =
            unsafe { activation.states.as_mut_ptr().add(top.scalar_base as usize) };
        // SAFETY: The checked frame reserves its complete local window.
        let operand_pointer = unsafe { local_pointer.add(top.local_count as usize) };
        // SAFETY: The compiler bounds every access by the checked buffer lengths.
        // The generated function uses the exact `NativeFunction` C ABI.
        unsafe {
            (self.entry)(
                local_pointer,
                local_state_pointer,
                operand_pointer,
                fuel,
                entry,
                std::ptr::from_mut(&mut allocation_context).cast::<c_void>(),
                allocate_instance::<R>,
                std::ptr::from_mut(&mut allocation_result),
                roots.as_mut_ptr(),
                root_states.as_mut_ptr(),
                &mut exit,
                &mut raw_activation,
            );
        }
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
        let kind = match exit.kind {
            EXIT_FUEL => ExitKind::Fuel,
            EXIT_RETURN => ExitKind::Return,
            EXIT_INTEGER_OVERFLOW => ExitKind::IntegerOverflow,
            EXIT_DIVIDE_BY_ZERO => ExitKind::DivideByZero,
            EXIT_INTERPRETER => ExitKind::Interpreter,
            EXIT_TYPE_MISMATCH => ExitKind::TypeMismatch,
            EXIT_UNINITIALIZED_FIELD => ExitKind::UninitializedField,
            EXIT_CALL => ExitKind::Call,
            EXIT_ALLOCATION => ExitKind::Allocation,
            EXIT_HEAP_LIMIT => ExitKind::HeapLimit,
            EXIT_EFFECT => ExitKind::Effect,
            EXIT_STACK_LIMIT => ExitKind::StackLimit,
            EXIT_GROW_ACTIVATION => ExitKind::GrowActivation,
            EXIT_INVALID_ENTRY => return Err(Failure::BackendUnavailable),
            _ => return Err(Failure::BackendUnavailable),
        };
        Ok(ExecutionExit {
            retired: exit.retired,
            kind,
            block: exit.block,
            instruction: exit.instruction,
            stack_len: exit.stack_len,
            result: exit.result,
        })
    }
}

mod plan;

pub use plan::{CompilerMetrics, EntryPlan, ExecutionExit, ExitKind, FunctionInput, ScalarKind};
use plan::{RegionPlan, SegmentExit};
impl JitEngine {
    /// Compile one verified function for its current arena layout.
    #[cold]
    #[inline(never)]
    pub fn compile(&self, input: FunctionInput<'_>) -> Result<Arc<CompiledRegion>, Failure> {
        self.compilation_attempts.fetch_add(1, Ordering::Relaxed);
        match compile_region(input) {
            Ok(region) => {
                self.compiled_regions.fetch_add(1, Ordering::Relaxed);
                self.compiled_segments
                    .fetch_add(region.plan.segments.len() as u64, Ordering::Relaxed);
                self.compiled_call_sites
                    .fetch_add(region.plan.call_sites as u64, Ordering::Relaxed);
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
            Err(CompileError::Unsupported(_reason)) => Err(Failure::Unsupported),
            Err(CompileError::Backend) => Err(Failure::BackendUnavailable),
        }
    }

    /// Return the current clock-free compilation counters.
    pub fn metrics(&self) -> CompilerMetrics {
        CompilerMetrics {
            compilation_attempts: self.compilation_attempts.load(Ordering::Relaxed),
            compiled_regions: self.compiled_regions.load(Ordering::Relaxed),
            compiled_segments: self.compiled_segments.load(Ordering::Relaxed),
            compiled_call_sites: self.compiled_call_sites.load(Ordering::Relaxed),
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
        self.compiled_segments.store(0, Ordering::Relaxed);
        self.compiled_call_sites.store(0, Ordering::Relaxed);
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
    if function.type_params != 0
        || function.effect_params != 0
        || !function.captures.is_empty()
        || function.local_types.len() > MAX_REGION_LOCALS
    {
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

/// Return true when one instruction has a dedicated JIT treatment.
pub fn instruction_has_dedicated_treatment(instruction: &lm_bytecode::Instr) -> bool {
    use lm_bytecode::{Instr, NumericInstr};

    matches!(
        instruction,
        Instr::ConstUnit
            | Instr::ConstBool(_)
            | Instr::ConstInt(_)
            | Instr::ConstFloat(_)
            | Instr::ConstChar(_)
            | Instr::LoadLocal(_)
            | Instr::StoreLocal(_)
            | Instr::Pop
            | Instr::Add
            | Instr::Sub
            | Instr::Mul
            | Instr::Div
            | Instr::Rem
            | Instr::Neg
            | Instr::Not
            | Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt
            | Instr::EqBool
            | Instr::NeBool
            | Instr::EqRef
            | Instr::NeRef
            | Instr::Call(_)
            | Instr::New(_)
            | Instr::LoadField(_)
            | Instr::StoreField(_)
            | Instr::TupleGet(_)
            | Instr::IsType(_)
            | Instr::CastType(_)
            | Instr::ListLen
            | Instr::ListAt
            | Instr::Extended(lm_bytecode::ExtendedInstr::ListSet)
            | Instr::Extended(lm_bytecode::ExtendedInstr::ListCapacity)
            | Instr::Extended(lm_bytecode::ExtendedInstr::ListEpoch)
            | Instr::Extended(lm_bytecode::ExtendedInstr::ListIterLen)
            | Instr::Extended(lm_bytecode::ExtendedInstr::SealInstance)
            | Instr::Native(lm_bytecode::NativeInstr::BytesLen)
            | Instr::Native(lm_bytecode::NativeInstr::BytesAt)
            | Instr::Native(lm_bytecode::NativeInstr::BytesGet)
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Return
            | Instr::Perform { .. }
            | Instr::PerformValue { .. }
            | Instr::OpConst(_)
            | Instr::Native(
                lm_bytecode::NativeInstr::CharCodepoint
                    | lm_bytecode::NativeInstr::CharUtf8Len
                    | lm_bytecode::NativeInstr::EqChar
                    | lm_bytecode::NativeInstr::NeChar
                    | lm_bytecode::NativeInstr::LtChar
                    | lm_bytecode::NativeInstr::LeChar
                    | lm_bytecode::NativeInstr::GtChar
                    | lm_bytecode::NativeInstr::GeChar,
            )
            | Instr::Numeric(
                NumericInstr::IntBitAnd
                    | NumericInstr::IntBitOr
                    | NumericInstr::IntBitXor
                    | NumericInstr::IntBitNot
                    | NumericInstr::IntShl
                    | NumericInstr::IntShr
                    | NumericInstr::IntUshr
                    | NumericInstr::IntWrappingAdd
                    | NumericInstr::IntWrappingSub
                    | NumericInstr::IntWrappingMul
                    | NumericInstr::IntRotateLeft
                    | NumericInstr::IntRotateRight
                    | NumericInstr::IntToFloat
                    | NumericInstr::FloatNeg
                    | NumericInstr::FloatAdd
                    | NumericInstr::FloatSub
                    | NumericInstr::FloatMul
                    | NumericInstr::FloatDiv
                    | NumericInstr::FloatEq
                    | NumericInstr::FloatNe
                    | NumericInstr::FloatLt
                    | NumericInstr::FloatLe
                    | NumericInstr::FloatGt
                    | NumericInstr::FloatGe
                    | NumericInstr::FloatIsNan
                    | NumericInstr::FloatHash
                    | NumericInstr::FloatBits
                    | NumericInstr::FloatFromBits
                    | NumericInstr::FloatToIntStatus
                    | NumericInstr::FloatToIntValue
            )
    )
}

mod backend;

use backend::{compile_region, CompileError};
#[cfg(test)]
mod tests;
