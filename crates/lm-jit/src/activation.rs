//! Native activation storage and the temporary runtime boundary.

use crate::Failure;
use std::ffi::c_void;

pub(super) const RUNTIME_LOAD_FIELD: u32 = 1;
pub(super) const RUNTIME_ALLOC_INSTANCE: u32 = 2;
pub(super) const RUNTIME_OK: u32 = 0;
pub(super) const RUNTIME_TYPE_MISMATCH: u32 = 1;
pub(super) const RUNTIME_UNINITIALIZED_FIELD: u32 = 2;
const RUNTIME_INTERPRETER: u32 = 3;
pub(super) const RUNTIME_HEAP_LIMIT: u32 = 4;

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
    pub(super) entries: *const usize,
    pub(super) entry_count: u32,
    pub(super) base_stack_values: u32,
    pub(super) stack_values: u32,
    pub(super) max_stack_values: u32,
    pub(super) base_frames: u32,
    pub(super) max_frames: u32,
}

pub(super) type RawRuntimeCall =
    unsafe extern "C" fn(*mut c_void, u32, u64, u64, u32, *mut u64) -> u32;

pub(super) type NativeFunction = unsafe extern "C" fn(
    *mut u64,
    *mut u8,
    *mut u64,
    u64,
    u32,
    *mut c_void,
    RawRuntimeCall,
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
            scalar_base: 0,
            local_count: u32::try_from(local_count).map_err(|_| Failure::BackendUnavailable)?,
            max_stack: u32::try_from(max_stack).map_err(|_| Failure::BackendUnavailable)?,
            operand_len: u32::try_from(operand_len).map_err(|_| Failure::BackendUnavailable)?,
            native_created: 0,
            caller_stack_values: 0,
        };
        self.scalar_len = window;
        self.frame_len = 1;
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
}

pub(super) struct RawRuntimeContext<R> {
    pub(super) runtime: *mut R,
    pub(super) activation: *const RawNativeActivation,
    pub(super) roots: *const u64,
    pub(super) root_states: *const u8,
    pub(super) root_capacity: usize,
}

/// One stable value representation at the native runtime boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ValueRepr {
    Unit = 0,
    Bool = 1,
    Int = 2,
    Float = 3,
    Object = 4,
    Operation = 5,
}

impl ValueRepr {
    fn from_raw(value: u32) -> Option<ValueRepr> {
        match value {
            0 => Some(ValueRepr::Unit),
            1 => Some(ValueRepr::Bool),
            2 => Some(ValueRepr::Int),
            3 => Some(ValueRepr::Float),
            4 => Some(ValueRepr::Object),
            5 => Some(ValueRepr::Operation),
            _ => None,
        }
    }
}

/// One checked runtime operation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeResult {
    Value(u64),
    TypeMismatch,
    UninitializedField,
    HeapLimit,
    Interpreter,
}

/// Safe runtime services available during one native activation.
pub trait Runtime {
    /// Copy one field value into its stable native representation.
    fn load_field(
        &mut self,
        reference: lm_value::ObjRef,
        field: u32,
        expected: ValueRepr,
    ) -> RuntimeResult;

    /// Allocate one plain instance with the supplied active roots.
    fn allocate_instance(
        &mut self,
        class: u32,
        root_bits: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> RuntimeResult;
}

pub(super) unsafe extern "C" fn runtime_call<R: Runtime>(
    context: *mut c_void,
    operation: u32,
    first: u64,
    second: u64,
    representation: u32,
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
    let response = match operation {
        RUNTIME_LOAD_FIELD => {
            let Some(expected) = ValueRepr::from_raw(representation) else {
                return RUNTIME_INTERPRETER;
            };
            let reference = lm_value::ObjRef {
                slot: first as u32,
                generation: (first >> 32) as u32,
            };
            runtime.load_field(reference, second as u32, expected)
        }
        RUNTIME_ALLOC_INSTANCE => {
            if second > 1 {
                return RUNTIME_INTERPRETER;
            }
            let root_count = representation as usize;
            if root_count > context.root_capacity {
                return RUNTIME_INTERPRETER;
            }
            // SAFETY: The checked count stays inside the activation root buffer.
            let root_bits = unsafe { std::slice::from_raw_parts(context.roots, root_count) };
            // SAFETY: Both root buffers have the same checked capacity.
            let root_states =
                unsafe { std::slice::from_raw_parts(context.root_states, root_count) };
            if context.activation.is_null() {
                return RUNTIME_INTERPRETER;
            }
            // SAFETY: The activation remains live during this call.
            let nested = unsafe { (*context.activation).frame_len > 1 };
            runtime.allocate_instance(first as u32, root_bits, root_states, second != 0 && !nested)
        }
        _ => RuntimeResult::Interpreter,
    };
    match response {
        RuntimeResult::Value(value) => {
            // SAFETY: The caller provides one writable result slot.
            unsafe { result.write(value) };
            RUNTIME_OK
        }
        RuntimeResult::TypeMismatch => RUNTIME_TYPE_MISMATCH,
        RuntimeResult::UninitializedField => RUNTIME_UNINITIALIZED_FIELD,
        RuntimeResult::HeapLimit => RUNTIME_HEAP_LIMIT,
        RuntimeResult::Interpreter => RUNTIME_INTERPRETER,
    }
}
