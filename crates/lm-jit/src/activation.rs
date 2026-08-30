//! Native activation storage and typed slow-path boundaries.

use crate::Failure;
use lm_heap::JitHeapView;
use std::ffi::c_void;

pub(super) const ALLOCATION_OK: u32 = 0;
const ALLOCATION_INTERPRETER: u32 = 1;
pub(super) const ALLOCATION_HEAP_LIMIT: u32 = 2;

const INITIAL_NATIVE_SCALARS: usize = 4_096;
const INITIAL_NATIVE_FRAMES: usize = 256;

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
    pub(super) result: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RawNativeFrame {
    pub(super) function: u32,
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
    pub(super) class_parents: *const u32,
    pub(super) class_count: usize,
}

pub(super) type RawAllocateInstance =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut u64) -> u32;

pub(super) type NativeFunction = unsafe extern "C" fn(
    *mut u64,
    *mut u8,
    *mut u64,
    u64,
    u32,
    *mut c_void,
    RawAllocateInstance,
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
    pub(super) states: Vec<u8>,
    pub(super) frames: Vec<RawNativeFrame>,
    pub(super) scalar_len: usize,
    pub(super) frame_len: usize,
    pub(super) changed_from: usize,
}

/// Root frame data and native scratch limits.
pub struct NativePreparation {
    pub function: u32,
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
    pub root_states: &'a mut [u8],
    pub fuel: u64,
    pub heap: JitHeapView,
    pub class_parents: &'a [u32],
}

/// One materialized view of a live native frame.
pub struct NativeFrameView<'a> {
    frame: RawNativeFrame,
    locals: &'a [u64],
    states: &'a [u8],
    operands: &'a [u64],
}

impl NativeFrameView<'_> {
    /// Return the namespace function slot.
    pub fn function(&self) -> u32 {
        self.frame.function
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

    /// Return the local initialization and mutation states.
    pub fn states(&self) -> &[u8] {
        self.states
    }

    /// Return the operand scalar bits.
    pub fn operands(&self) -> &[u64] {
        self.operands
    }
}

impl NativeActivation {
    /// Prepare one root frame without changing guest state.
    pub fn prepare_root(&mut self, input: NativePreparation) -> Result<(), Failure> {
        let NativePreparation {
            function,
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
        self.states.resize(scalar_capacity, 0);
        self.frames
            .resize(frame_capacity, RawNativeFrame::default());
        self.scalars[..window].fill(0);
        self.states[..window].fill(0);
        self.frames[0] = RawNativeFrame {
            function,
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
    pub fn root_buffers_mut(&mut self) -> (&mut [u64], &mut [u8], &mut [u64]) {
        let locals = self.frames[0].local_count as usize;
        let stack = self.frames[0].max_stack as usize;
        let (local_bits, rest) = self.scalars.split_at_mut(locals);
        let operand_bits = &mut rest[..stack];
        (local_bits, &mut self.states[..locals], operand_bits)
    }

    /// Return all immutable root buffers.
    pub fn root_buffers(&self) -> (&[u64], &[u8], &[u64]) {
        let locals = self.frames[0].local_count as usize;
        let stack = self.frames[0].max_stack as usize;
        (
            &self.scalars[..locals],
            &self.states[..locals],
            &self.scalars[locals..locals + stack],
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
                states: &self.states[base..operand_base],
                operands: &self.scalars[operand_base..operand_base + operands],
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
        self.states
            .try_reserve(scalar_target.saturating_sub(self.states.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.frames
            .try_reserve(frame_target.saturating_sub(self.frames.len()))
            .map_err(|_| Failure::BackendUnavailable)?;
        self.scalars.resize(scalar_target, 0);
        self.states.resize(scalar_target, 0);
        self.frames.resize(frame_target, RawNativeFrame::default());
        Ok(true)
    }

    /// Finish one detached native return inside this activation.
    pub fn finish_detached_return(&mut self, result: u64) -> Result<(), Failure> {
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
        parent.operand_len += 1;
        self.frame_len -= 1;
        self.scalar_len = child.scalar_base as usize;
        self.changed_from = self.changed_from.min(self.frame_len);
        Ok(())
    }
}

fn growth_target(current: usize, required: usize, limit: usize) -> Result<usize, Failure> {
    if required > limit {
        return Err(Failure::BackendUnavailable);
    }
    let doubled = current.saturating_mul(2).min(limit);
    Ok(current.max(required).max(doubled))
}

pub(super) struct RawAllocationContext<R> {
    pub(super) runtime: *mut R,
    pub(super) activation: *mut RawNativeActivation,
    pub(super) roots: *const u64,
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

/// The typed allocation slow path of one native activation.
pub trait AllocationRuntime {
    /// Allocate one plain instance with the supplied active roots.
    fn allocate_instance(
        &mut self,
        class: u32,
        root_bits: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> AllocationResult;
}

pub(super) unsafe extern "C" fn allocate_instance<R: AllocationRuntime>(
    context: *mut c_void,
    class: u32,
    allow_collection: u32,
    root_count: u32,
    result: *mut u64,
) -> u32 {
    if context.is_null() || result.is_null() {
        return ALLOCATION_INTERPRETER;
    }
    // SAFETY: `CompiledRegion::execute` passes one live context for this call.
    let context = unsafe { &mut *context.cast::<RawAllocationContext<R>>() };
    if context.runtime.is_null() {
        return ALLOCATION_INTERPRETER;
    }
    // SAFETY: The context retains one live runtime during this call.
    let runtime = unsafe { &mut *context.runtime };
    if allow_collection > 1 || context.activation.is_null() {
        return ALLOCATION_INTERPRETER;
    }
    let root_count = root_count as usize;
    if root_count > context.root_capacity {
        return ALLOCATION_INTERPRETER;
    }
    // SAFETY: The checked count stays inside the activation root buffer.
    let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
    // SAFETY: Both root buffers have the same checked capacity.
    let root_states = unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
    // SAFETY: The activation remains live during this call.
    let nested = unsafe { (*context.activation).frame_len > 1 };
    let response = runtime.allocate_instance(
        class,
        root_bits,
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
                }
            }
            // SAFETY: The caller provides one writable result slot.
            unsafe { result.write(bits) };
            ALLOCATION_OK
        }
        AllocationResult::HeapLimit => ALLOCATION_HEAP_LIMIT,
        AllocationResult::Interpreter => ALLOCATION_INTERPRETER,
    }
}
