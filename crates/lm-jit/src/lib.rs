//! Native regions over verified scalar LMBC.

use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlags,
    UserFuncName,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module as _};
use lm_bytecode::{BcType, Func, Instr, Module, NumericInstr};
use lm_value::{canonical_float_bits, CANONICAL_NAN_BITS};
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};
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

const RUNTIME_LOAD_FIELD: u32 = 1;
const RUNTIME_ALLOC_INSTANCE: u32 = 2;
const RUNTIME_OK: u32 = 0;
const RUNTIME_TYPE_MISMATCH: u32 = 1;
const RUNTIME_UNINITIALIZED_FIELD: u32 = 2;
const RUNTIME_INTERPRETER: u32 = 3;
const RUNTIME_HEAP_LIMIT: u32 = 4;

/// The native local changed during this activation.
pub const LOCAL_DIRTY: u8 = 1;
/// The native local contains an initialized value.
pub const LOCAL_INITIALIZED: u8 = 2;

#[repr(C)]
#[derive(Debug, Default)]
struct RawExit {
    retired: u64,
    kind: u32,
    block: u32,
    instruction: u32,
    stack_len: u32,
    result: u64,
}

type RawRuntimeCall = unsafe extern "C" fn(*mut c_void, u32, u64, u64, u32, *mut u64) -> u32;

type NativeFunction = unsafe extern "C" fn(
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
);

struct RawRuntimeContext<R> {
    runtime: *mut R,
    roots: *const u64,
    root_states: *const u8,
    root_capacity: usize,
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

unsafe extern "C" fn runtime_call<R: Runtime>(
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
            runtime.allocate_instance(first as u32, root_bits, root_states, second != 0)
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
    compiled_allocation_sites: AtomicU64,
    compiled_effect_sites: AtomicU64,
}

impl fmt::Debug for JitEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("JitEngine").finish()
    }
}

/// One immutable compiled function region.
pub struct CompiledRegion {
    plan: RegionPlan,
    entry: NativeFunction,
    // The module owns the executable memory behind `entry`.
    module: Mutex<Option<JITModule>>,
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
                        SegmentExit::Call { .. } | SegmentExit::Effect { .. }
                    )
            })
            .map(|segment| segment.boundary_stack.as_slice())
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

    /// Execute native code over explicit scalar buffers.
    #[inline(always)]
    pub fn execute<R: Runtime>(
        &self,
        runtime: &mut R,
        entry: u32,
        locals: &mut [u64],
        local_states: &mut [u8],
        operands: &mut [u64],
        fuel: u64,
    ) -> Result<ExecutionExit, Failure> {
        if entry as usize >= self.plan.segments.len()
            || locals.len() != self.plan.local_kinds.len()
            || local_states.len() != self.plan.local_kinds.len()
            || operands.len() < self.plan.max_stack
        {
            return Err(Failure::BackendUnavailable);
        }
        let mut exit = RawExit::default();
        let mut runtime_result = 0u64;
        let mut roots = vec![0u64; self.plan.max_roots.max(1)];
        let mut root_states = vec![0u8; self.plan.max_roots.max(1)];
        let mut runtime_context = RawRuntimeContext {
            runtime: std::ptr::from_mut(runtime),
            roots: roots.as_ptr(),
            root_states: root_states.as_ptr(),
            root_capacity: self.plan.max_roots,
        };
        // SAFETY: The compiler bounds every access by the checked buffer lengths.
        // The generated function uses the exact `NativeFunction` C ABI.
        unsafe {
            (self.entry)(
                locals.as_mut_ptr(),
                local_states.as_mut_ptr(),
                operands.as_mut_ptr(),
                fuel,
                entry,
                std::ptr::from_mut(&mut runtime_context).cast::<c_void>(),
                runtime_call::<R>,
                std::ptr::from_mut(&mut runtime_result),
                roots.as_mut_ptr(),
                root_states.as_mut_ptr(),
                &mut exit,
            );
        }
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

/// One scalar representation used by the native ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Unit,
    Bool,
    Int,
    Float,
    /// One heap object with a source-unit type index.
    Object(u32),
    Operation,
}

impl ScalarKind {
    fn representation(self) -> ValueRepr {
        match self {
            ScalarKind::Unit => ValueRepr::Unit,
            ScalarKind::Bool => ValueRepr::Bool,
            ScalarKind::Int => ValueRepr::Int,
            ScalarKind::Float => ValueRepr::Float,
            ScalarKind::Object(_) => ValueRepr::Object,
            ScalarKind::Operation => ValueRepr::Operation,
        }
    }
}

#[derive(Clone, Copy)]
struct FunctionDefinition<'a> {
    function: u32,
    runtime: &'a Func,
    source: &'a Module,
    bundle: &'a Arc<lm_abi::AbiBundle>,
    source_function: u32,
}

/// Immutable verified input for one native compilation.
pub struct FunctionInput<'a> {
    root: FunctionDefinition<'a>,
    direct_callees: Vec<FunctionDefinition<'a>>,
}

impl<'a> FunctionInput<'a> {
    /// Create one function input from a published function and its source unit.
    #[inline]
    pub fn new(
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
    ) -> FunctionInput<'a> {
        FunctionInput {
            root: FunctionDefinition {
                function,
                runtime,
                source,
                bundle,
                source_function,
            },
            direct_callees: Vec::new(),
        }
    }

    /// Add one exact direct callee used by the root function.
    pub fn add_direct_callee(
        &mut self,
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
    ) {
        if function == self.root.function
            || self
                .direct_callees
                .iter()
                .any(|definition| definition.function == function)
        {
            return;
        }
        self.direct_callees.push(FunctionDefinition {
            function,
            runtime,
            source,
            bundle,
            source_function,
        });
    }

    fn definition(&self, function: u32) -> Option<FunctionDefinition<'a>> {
        if function == self.root.function {
            Some(self.root)
        } else {
            self.direct_callees
                .iter()
                .find(|definition| definition.function == function)
                .copied()
        }
    }
}

/// Clock-free native compilation counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompilerMetrics {
    pub compilation_attempts: u64,
    pub compiled_regions: u64,
    pub compiled_segments: u64,
    pub compiled_call_sites: u64,
    pub compiled_heap_read_sites: u64,
    pub compiled_allocation_sites: u64,
    pub compiled_effect_sites: u64,
}

/// One supported native entry and its required scalar values.
#[derive(Debug, Clone, Copy)]
pub struct EntryPlan<'a> {
    index: u32,
    live_locals: &'a [bool],
    operand_kinds: &'a [ScalarKind],
}

impl EntryPlan<'_> {
    /// Return the native entry index.
    #[inline(always)]
    pub fn index(&self) -> u32 {
        self.index
    }

    /// Return the local slots that native code can read.
    #[inline(always)]
    pub fn live_locals(&self) -> &[bool] {
        self.live_locals
    }

    /// Return the required operand representations.
    #[inline(always)]
    pub fn operand_kinds(&self) -> &[ScalarKind] {
        self.operand_kinds
    }
}

/// One native exit category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitKind {
    Fuel,
    Return,
    IntegerOverflow,
    DivideByZero,
    TypeMismatch,
    UninitializedField,
    Call,
    Allocation,
    HeapLimit,
    Effect,
    Interpreter,
}

/// One validated native exit record.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionExit {
    retired: u64,
    kind: ExitKind,
    block: u32,
    instruction: u32,
    stack_len: u32,
    result: u64,
}

impl ExecutionExit {
    #[inline(always)]
    pub fn retired(&self) -> u64 {
        self.retired
    }

    #[inline(always)]
    pub fn kind(&self) -> ExitKind {
        self.kind
    }

    #[inline(always)]
    pub fn block(&self) -> u32 {
        self.block
    }

    #[inline(always)]
    pub fn instruction(&self) -> u32 {
        self.instruction
    }

    #[inline(always)]
    pub fn stack_len(&self) -> u32 {
        self.stack_len
    }

    #[inline(always)]
    pub fn result(&self) -> u64 {
        self.result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UnsupportedReason {
    MissingSource,
    GenericFunction,
    CapturedFunction,
    NonScalarType,
    UnsupportedInstruction,
    InvalidStack,
    InvalidControlFlow,
    RegionLimit,
}

#[derive(Debug, Clone, Copy)]
enum SegmentExit {
    Jump {
        target_block: u32,
    },
    Conditional {
        target_block: u32,
        jump_on_true: bool,
        fallthrough_ip: u32,
    },
    Call {
        target: u32,
        fallthrough_ip: u32,
    },
    Allocation {
        fallthrough_ip: u32,
    },
    Effect {
        fallthrough_ip: u32,
    },
    Return,
}

#[derive(Debug, Clone)]
struct Segment {
    block: u32,
    start: u32,
    end: u32,
    cost: u32,
    exit: SegmentExit,
    uses: Vec<bool>,
    definitions: Vec<bool>,
    successors: Vec<usize>,
    live_in: Vec<bool>,
    entry_stack: Vec<ScalarKind>,
    exit_stack: Vec<ScalarKind>,
    boundary_stack: Vec<ScalarKind>,
    field_results: Vec<(u32, ScalarKind)>,
    replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone)]
struct InlineFunctionPlan {
    params: Vec<ScalarKind>,
    local_kinds: Vec<ScalarKind>,
    max_stack: usize,
    cost: u32,
    allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone)]
struct CallContract {
    params: Vec<ScalarKind>,
    result: ScalarKind,
    inline: Option<InlineFunctionPlan>,
}

#[derive(Debug, Clone)]
struct RegionPlan {
    local_kinds: Vec<ScalarKind>,
    result_kind: ScalarKind,
    max_stack: usize,
    max_stack_values: usize,
    max_roots: usize,
    additional_frames: u32,
    segments: Vec<Segment>,
    entries: std::collections::HashMap<(u32, u32), usize>,
    inline_functions: HashMap<u32, InlineFunctionPlan>,
    inline_call_sites: usize,
    heap_read_sites: usize,
    allocation_sites: usize,
    effect_sites: usize,
}

struct SegmentAnalysis {
    uses: Vec<bool>,
    definitions: Vec<bool>,
    exit_stack: Vec<ScalarKind>,
    max_stack: usize,
    max_stack_values: usize,
    boundary_stack: Vec<ScalarKind>,
    field_results: Vec<(u32, ScalarKind)>,
    replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    allocations: Vec<AllocationSite>,
}

struct SegmentAnalysisContext<'a> {
    func: &'a Func,
    source_func: &'a Func,
    module: &'a Module,
    locals: &'a [ScalarKind],
    result: ScalarKind,
    calls: &'a HashMap<u32, CallContract>,
}

#[derive(Debug, Clone)]
struct AllocationSite {
    instruction: u32,
    initialized: Vec<bool>,
    stack: Vec<ScalarKind>,
}

impl RegionPlan {
    fn for_function(input: &FunctionInput<'_>) -> Result<RegionPlan, UnsupportedReason> {
        let runtime = input.root.runtime;
        if runtime.type_params != 0 || runtime.effect_params != 0 {
            return Err(UnsupportedReason::GenericFunction);
        }
        if !runtime.captures.is_empty() {
            return Err(UnsupportedReason::CapturedFunction);
        }
        let instructions = runtime
            .blocks
            .iter()
            .try_fold(0usize, |total, block| total.checked_add(block.len()));
        if runtime.local_types.len() > MAX_REGION_LOCALS
            || instructions.is_none_or(|count| count > MAX_REGION_INSTRUCTIONS)
        {
            return Err(UnsupportedReason::RegionLimit);
        }
        let source = input.root.source;
        let source_func = source
            .funcs
            .get(input.root.source_function as usize)
            .ok_or(UnsupportedReason::MissingSource)?;
        let states = lm_verify::verify_function_states_with_bundle(
            source,
            input.root.bundle,
            input.root.source_function,
        )
        .map_err(|_| UnsupportedReason::MissingSource)?;
        if source_func.blocks.len() != runtime.blocks.len()
            || source_func.local_types.len() != runtime.local_types.len()
        {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        let local_kinds = source_func
            .local_types
            .iter()
            .map(|ty| scalar_kind(source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result_kind = scalar_kind(source, source_func.ret)?;
        let call_contracts = call_contracts(input)?;
        let inline_functions: HashMap<u32, InlineFunctionPlan> = call_contracts
            .iter()
            .filter_map(|(function, contract)| {
                contract
                    .inline
                    .as_ref()
                    .map(|plan| (*function, plan.clone()))
            })
            .collect();
        let mut segments = split_segments(runtime)?;
        let entries: std::collections::HashMap<(u32, u32), usize> = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| ((segment.block, segment.start), index))
            .collect();
        resolve_successors(&mut segments, &entries)?;
        let mut max_stack = 0;
        let mut max_stack_values = 0;
        let mut additional_frames = 0;
        let mut inline_call_sites = 0;
        let mut heap_read_sites = 0;
        let mut allocation_sites = 0;
        let mut effect_sites = 0;
        let mut active_block = u32::MAX;
        let mut block_stack = Vec::new();
        let analysis_context = SegmentAnalysisContext {
            func: runtime,
            source_func,
            module: source,
            locals: &local_kinds,
            result: result_kind,
            calls: &call_contracts,
        };
        for (index, segment) in segments.iter_mut().enumerate() {
            let state = states
                .get(segment.block as usize)
                .and_then(Option::as_ref)
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            if active_block != segment.block {
                active_block = segment.block;
                block_stack = state
                    .stack()
                    .iter()
                    .map(|ty| scalar_kind(source, *ty))
                    .collect::<Result<Vec<_>, _>>()?;
            }
            segment.entry_stack = block_stack.clone();
            let mut initialized: Vec<bool> = state.locals().iter().map(Option::is_some).collect();
            for instruction in runtime.blocks[segment.block as usize]
                .iter()
                .take(segment.start as usize)
            {
                if let Instr::StoreLocal(slot) = instruction {
                    let Some(value) = initialized.get_mut(*slot as usize) else {
                        return Err(UnsupportedReason::InvalidControlFlow);
                    };
                    *value = true;
                }
            }
            let analysis = analyze_segment(&analysis_context, segment, &initialized, &block_stack)?;
            segment.uses = analysis.uses;
            segment.definitions = analysis.definitions;
            segment.exit_stack = analysis.exit_stack.clone();
            segment.boundary_stack = analysis.boundary_stack;
            segment.field_results = analysis.field_results;
            heap_read_sites += segment.field_results.len();
            segment.replay_stacks = analysis.replay_stacks;
            segment.fault_stacks = analysis.fault_stacks;
            segment.allocations = analysis.allocations;
            allocation_sites += segment.allocations.len();
            segment.cost = segment.end - segment.start;
            if let SegmentExit::Call { target, .. } = segment.exit {
                if let Some(inline) = inline_functions.get(&target) {
                    segment.cost = segment
                        .cost
                        .checked_add(inline.cost)
                        .ok_or(UnsupportedReason::RegionLimit)?;
                    additional_frames = 1;
                    inline_call_sites += 1;
                } else {
                    segment.cost = segment.cost.saturating_sub(1);
                }
            }
            if matches!(segment.exit, SegmentExit::Effect { .. }) {
                segment.cost = segment.cost.saturating_sub(1);
                effect_sites += 1;
            }
            block_stack = analysis.exit_stack;
            max_stack = max_stack.max(analysis.max_stack);
            max_stack_values = max_stack_values.max(analysis.max_stack_values);
            debug_assert_eq!(index, entries[&(segment.block, segment.start)]);
        }
        for segment in &segments {
            for successor in &segment.successors {
                if segment.exit_stack != segments[*successor].entry_stack {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
            }
        }
        if max_stack > MAX_REGION_STACK {
            return Err(UnsupportedReason::RegionLimit);
        }
        compute_liveness(&mut segments, local_kinds.len());
        let root_local_count = local_kinds
            .iter()
            .filter(|kind| matches!(kind, ScalarKind::Object(_)))
            .count();
        let inline_root_count = inline_functions
            .values()
            .map(|inline| {
                inline
                    .local_kinds
                    .iter()
                    .filter(|kind| matches!(kind, ScalarKind::Object(_)))
                    .count()
                    + inline.max_stack
            })
            .max()
            .unwrap_or(0);
        let max_roots = root_local_count
            .checked_add(max_stack)
            .and_then(|count| count.checked_add(inline_root_count))
            .ok_or(UnsupportedReason::RegionLimit)?;
        allocation_sites = inline_functions
            .values()
            .try_fold(allocation_sites, |total, inline| {
                total.checked_add(inline.allocations.len())
            })
            .ok_or(UnsupportedReason::RegionLimit)?;
        Ok(RegionPlan {
            local_kinds,
            result_kind,
            max_stack,
            max_stack_values,
            max_roots,
            additional_frames,
            segments,
            entries,
            inline_functions,
            inline_call_sites,
            heap_read_sites,
            allocation_sites,
            effect_sites,
        })
    }

    fn distance_to_entry(&self, block: u32, instruction: u32) -> Option<u32> {
        self.segments
            .iter()
            .find(|segment| {
                segment.block == block && segment.start < instruction && instruction < segment.end
            })
            .map(|segment| segment.end - instruction)
    }
}

fn call_contracts(
    input: &FunctionInput<'_>,
) -> Result<HashMap<u32, CallContract>, UnsupportedReason> {
    let mut contracts = HashMap::new();
    let definitions = std::iter::once(input.root).chain(input.direct_callees.iter().copied());
    for definition in definitions {
        let source_func = definition
            .source
            .funcs
            .get(definition.source_function as usize)
            .ok_or(UnsupportedReason::MissingSource)?;
        if source_func.params.len() != definition.runtime.params.len()
            || source_func.local_types.len() != definition.runtime.local_types.len()
        {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        let params = source_func
            .params
            .iter()
            .map(|ty| scalar_kind(definition.source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result = scalar_kind(definition.source, source_func.ret)?;
        let inline = inline_function_plan(definition, &params, result);
        contracts.insert(
            definition.function,
            CallContract {
                params,
                result,
                inline,
            },
        );
    }
    Ok(contracts)
}

fn inline_function_plan(
    definition: FunctionDefinition<'_>,
    params: &[ScalarKind],
    result: ScalarKind,
) -> Option<InlineFunctionPlan> {
    const MAX_INLINE_INSTRUCTIONS: usize = 256;

    let runtime = definition.runtime;
    if runtime.type_params != 0
        || runtime.effect_params != 0
        || !runtime.captures.is_empty()
        || !runtime.row.is_empty()
        || runtime.blocks.len() != 1
    {
        return None;
    }
    let code = runtime.blocks.first()?;
    if code.is_empty()
        || code.len() > MAX_INLINE_INSTRUCTIONS
        || !matches!(code.last(), Some(Instr::Return))
        || code[..code.len() - 1].iter().any(|instruction| {
            matches!(
                instruction,
                Instr::Jump(_)
                    | Instr::JumpIfFalse(_)
                    | Instr::JumpIfTrue(_)
                    | Instr::Call(_)
                    | Instr::Return
            )
        })
    {
        return None;
    }
    let mut allocated = false;
    for instruction in &code[..code.len() - 1] {
        if allocated
            && matches!(
                instruction,
                Instr::New(_)
                    | Instr::LoadField(_)
                    | Instr::Add
                    | Instr::Sub
                    | Instr::Mul
                    | Instr::Div
                    | Instr::Rem
                    | Instr::Neg
            )
        {
            return None;
        }
        allocated |= matches!(instruction, Instr::New(_));
    }
    let source_func = definition
        .source
        .funcs
        .get(definition.source_function as usize)?;
    let local_kinds = source_func
        .local_types
        .iter()
        .map(|ty| scalar_kind(definition.source, *ty))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if local_kinds.len() > MAX_REGION_LOCALS || local_kinds.get(..params.len()) != Some(params) {
        return None;
    }
    let segment = Segment {
        block: 0,
        start: 0,
        end: code.len() as u32,
        cost: code.len() as u32,
        exit: SegmentExit::Return,
        uses: Vec::new(),
        definitions: Vec::new(),
        successors: Vec::new(),
        live_in: Vec::new(),
        entry_stack: Vec::new(),
        exit_stack: Vec::new(),
        boundary_stack: Vec::new(),
        field_results: Vec::new(),
        replay_stacks: Vec::new(),
        fault_stacks: Vec::new(),
        allocations: Vec::new(),
    };
    let mut initialized = vec![false; local_kinds.len()];
    initialized[..params.len()].fill(true);
    let calls = HashMap::new();
    let context = SegmentAnalysisContext {
        func: runtime,
        source_func,
        module: definition.source,
        locals: &local_kinds,
        result,
        calls: &calls,
    };
    let analysis = analyze_segment(&context, &segment, &initialized, &[]).ok()?;
    Some(InlineFunctionPlan {
        params: params.to_vec(),
        local_kinds,
        max_stack: analysis.max_stack,
        cost: code.len() as u32,
        allocations: analysis.allocations,
    })
}

fn scalar_kind(module: &lm_bytecode::Module, ty: u32) -> Result<ScalarKind, UnsupportedReason> {
    match module.types.get(ty as usize) {
        Some(BcType::Unit) => Ok(ScalarKind::Unit),
        Some(BcType::Bool) => Ok(ScalarKind::Bool),
        Some(BcType::Int) => Ok(ScalarKind::Int),
        Some(BcType::Float) => Ok(ScalarKind::Float),
        Some(BcType::Class(_)) => Ok(ScalarKind::Object(ty)),
        Some(BcType::Op(_, _)) => Ok(ScalarKind::Operation),
        _ => Err(UnsupportedReason::NonScalarType),
    }
}

fn split_segments(func: &Func) -> Result<Vec<Segment>, UnsupportedReason> {
    let mut segments = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut start = 0usize;
        for (instruction_index, instruction) in block.iter().enumerate() {
            let exit = match instruction {
                Instr::Jump(target) => Some(SegmentExit::Jump {
                    target_block: *target,
                }),
                Instr::JumpIfFalse(target) => Some(SegmentExit::Conditional {
                    target_block: *target,
                    jump_on_true: false,
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::JumpIfTrue(target) => Some(SegmentExit::Conditional {
                    target_block: *target,
                    jump_on_true: true,
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::Call(target) => Some(SegmentExit::Call {
                    target: *target,
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::New(_) => Some(SegmentExit::Allocation {
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::Perform { .. } | Instr::PerformValue { .. } => Some(SegmentExit::Effect {
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::Return => Some(SegmentExit::Return),
                _ => None,
            };
            let Some(exit) = exit else { continue };
            segments.push(Segment {
                block: block_index as u32,
                start: start as u32,
                end: instruction_index as u32 + 1,
                cost: instruction_index as u32 + 1 - start as u32,
                exit,
                uses: Vec::new(),
                definitions: Vec::new(),
                successors: Vec::new(),
                live_in: Vec::new(),
                entry_stack: Vec::new(),
                exit_stack: Vec::new(),
                boundary_stack: Vec::new(),
                field_results: Vec::new(),
                replay_stacks: Vec::new(),
                fault_stacks: Vec::new(),
                allocations: Vec::new(),
            });
            start = instruction_index + 1;
        }
        if start != block.len() {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
    }
    Ok(segments)
}

fn resolve_successors(
    segments: &mut [Segment],
    entries: &std::collections::HashMap<(u32, u32), usize>,
) -> Result<(), UnsupportedReason> {
    for segment in segments {
        segment.successors = match segment.exit {
            SegmentExit::Jump { target_block } => vec![entry(entries, target_block, 0)?],
            SegmentExit::Conditional {
                target_block,
                fallthrough_ip,
                ..
            } => vec![
                entry(entries, target_block, 0)?,
                entry(entries, segment.block, fallthrough_ip)?,
            ],
            SegmentExit::Call { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Allocation { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Effect { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Return => Vec::new(),
        };
    }
    Ok(())
}

fn entry(
    entries: &std::collections::HashMap<(u32, u32), usize>,
    block: u32,
    instruction: u32,
) -> Result<usize, UnsupportedReason> {
    entries
        .get(&(block, instruction))
        .copied()
        .ok_or(UnsupportedReason::InvalidControlFlow)
}

fn analyze_segment(
    context: &SegmentAnalysisContext<'_>,
    segment: &Segment,
    initialized: &[bool],
    entry_stack: &[ScalarKind],
) -> Result<SegmentAnalysis, UnsupportedReason> {
    let mut stack = entry_stack.to_vec();
    let mut max_stack = stack.len();
    let mut max_stack_values = stack.len();
    let mut boundary_stack = Vec::new();
    let mut field_results = Vec::new();
    let mut replay_stacks = Vec::new();
    let mut fault_stacks = Vec::new();
    let mut allocations = Vec::new();
    let mut uses = vec![false; context.locals.len()];
    let mut definitions = vec![false; context.locals.len()];
    for (offset, instruction) in context.func.blocks[segment.block as usize]
        [segment.start as usize..segment.end as usize]
        .iter()
        .enumerate()
    {
        let source_instruction = context
            .source_func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize + offset))
            .copied()
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        match *instruction {
            Instr::ConstUnit => stack.push(ScalarKind::Unit),
            Instr::ConstBool(_) => stack.push(ScalarKind::Bool),
            Instr::ConstInt(_) => stack.push(ScalarKind::Int),
            Instr::ConstFloat(_) => stack.push(ScalarKind::Float),
            Instr::OpConst(_) => stack.push(ScalarKind::Operation),
            Instr::LoadLocal(slot) => {
                let at = slot as usize;
                let Some(kind) = context.locals.get(at).copied() else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                if !initialized.get(at).copied().unwrap_or(false)
                    && !definitions.get(at).copied().unwrap_or(false)
                {
                    return Err(UnsupportedReason::InvalidStack);
                }
                if !definitions[at] {
                    uses[at] = true;
                }
                stack.push(kind);
            }
            Instr::StoreLocal(slot) => {
                let at = slot as usize;
                let kind = context
                    .locals
                    .get(at)
                    .copied()
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                expect(&mut stack, kind)?;
                definitions[at] = true;
            }
            Instr::Pop => {
                stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
            }
            Instr::LoadField(field) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let kind = field_kind(context.module, receiver, field)?;
                field_results.push((instruction, kind));
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(kind);
            }
            Instr::New(_) => {
                let Instr::New(class) = source_instruction else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let ty = context
                    .module
                    .types
                    .iter()
                    .position(|ty| matches!(ty, BcType::Class(found) if *found == class))
                    .and_then(|index| u32::try_from(index).ok())
                    .ok_or(UnsupportedReason::NonScalarType)?;
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let mut initialized_now = initialized.to_vec();
                for (slot, defined) in definitions.iter().copied().enumerate() {
                    initialized_now[slot] |= defined;
                }
                allocations.push(AllocationSite {
                    instruction,
                    initialized: initialized_now,
                    stack: stack.clone(),
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(ScalarKind::Object(ty));
            }
            Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
                let instruction = segment.start + offset as u32;
                expect(&mut stack, ScalarKind::Int)?;
                expect(&mut stack, ScalarKind::Int)?;
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(ScalarKind::Int);
            }
            Instr::Neg => {
                let instruction = segment.start + offset as u32;
                expect(&mut stack, ScalarKind::Int)?;
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(ScalarKind::Int);
            }
            Instr::Not => {
                expect(&mut stack, ScalarKind::Bool)?;
                stack.push(ScalarKind::Bool);
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                expect(&mut stack, ScalarKind::Int)?;
                expect(&mut stack, ScalarKind::Int)?;
                stack.push(ScalarKind::Bool);
            }
            Instr::EqBool | Instr::NeBool => {
                expect(&mut stack, ScalarKind::Bool)?;
                expect(&mut stack, ScalarKind::Bool)?;
                stack.push(ScalarKind::Bool);
            }
            Instr::Call(target) => {
                let contract = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                boundary_stack = stack.clone();
                for parameter in contract.params.iter().rev().copied() {
                    expect(&mut stack, parameter)?;
                }
                if let Some(inline) = &contract.inline {
                    let prefix = stack.len();
                    let push_limit = boundary_stack
                        .len()
                        .checked_add(inline.local_kinds.len())
                        .ok_or(UnsupportedReason::RegionLimit)?;
                    let body_limit = prefix
                        .checked_add(inline.local_kinds.len())
                        .and_then(|value| value.checked_add(inline.max_stack))
                        .ok_or(UnsupportedReason::RegionLimit)?;
                    max_stack_values = max_stack_values.max(push_limit).max(body_limit);
                }
                stack.push(contract.result);
            }
            Instr::Perform { argc, .. } => {
                let Instr::Perform {
                    reply_ty: source_reply,
                    ..
                } = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                boundary_stack = stack.clone();
                let arguments = usize::try_from(argc)
                    .ok()
                    .and_then(|count| stack.len().checked_sub(count))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                stack.truncate(arguments);
                stack.push(scalar_kind(context.module, source_reply)?);
            }
            Instr::PerformValue { argc, .. } => {
                let Instr::PerformValue {
                    reply_ty: source_reply,
                    ..
                } = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                boundary_stack = stack.clone();
                let operation = usize::try_from(argc)
                    .ok()
                    .and_then(|count| stack.len().checked_sub(count))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                stack.truncate(operation);
                expect(&mut stack, ScalarKind::Operation)?;
                stack.push(scalar_kind(context.module, source_reply)?);
            }
            Instr::Numeric(operation) if float_operation(operation, &mut stack)? => {}
            Instr::Jump(_) => {}
            Instr::JumpIfFalse(_) | Instr::JumpIfTrue(_) => {
                expect(&mut stack, ScalarKind::Bool)?;
            }
            Instr::Return => {
                expect(&mut stack, context.result)?;
                if !stack.is_empty() {
                    return Err(UnsupportedReason::InvalidStack);
                }
            }
            _ => return Err(UnsupportedReason::UnsupportedInstruction),
        }
        max_stack = max_stack.max(stack.len());
        max_stack_values = max_stack_values.max(stack.len());
    }
    Ok(SegmentAnalysis {
        uses,
        definitions,
        exit_stack: stack,
        max_stack,
        max_stack_values,
        boundary_stack,
        field_results,
        replay_stacks,
        fault_stacks,
        allocations,
    })
}

fn field_kind(
    module: &Module,
    receiver: ScalarKind,
    field: u32,
) -> Result<ScalarKind, UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let Some(BcType::Class(class)) = module.types.get(ty as usize) else {
        return Err(UnsupportedReason::NonScalarType);
    };
    let field_type = module
        .classes
        .get(*class as usize)
        .and_then(|class| class.fields.get(field as usize))
        .map(|(_, ty)| *ty)
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    scalar_kind(module, field_type)
}

fn expect(stack: &mut Vec<ScalarKind>, expected: ScalarKind) -> Result<(), UnsupportedReason> {
    match stack.pop() {
        Some(found) if found == expected => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn float_operation(
    operation: NumericInstr,
    stack: &mut Vec<ScalarKind>,
) -> Result<bool, UnsupportedReason> {
    match operation {
        NumericInstr::FloatNeg => {
            expect(stack, ScalarKind::Float)?;
            stack.push(ScalarKind::Float);
        }
        NumericInstr::FloatAdd
        | NumericInstr::FloatSub
        | NumericInstr::FloatMul
        | NumericInstr::FloatDiv => {
            expect(stack, ScalarKind::Float)?;
            expect(stack, ScalarKind::Float)?;
            stack.push(ScalarKind::Float);
        }
        NumericInstr::FloatEq
        | NumericInstr::FloatNe
        | NumericInstr::FloatLt
        | NumericInstr::FloatLe
        | NumericInstr::FloatGt
        | NumericInstr::FloatGe => {
            expect(stack, ScalarKind::Float)?;
            expect(stack, ScalarKind::Float)?;
            stack.push(ScalarKind::Bool);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn compute_liveness(segments: &mut [Segment], locals: usize) {
    for segment in segments.iter_mut() {
        segment.live_in = vec![false; locals];
    }
    loop {
        let previous: Vec<Vec<bool>> = segments
            .iter()
            .map(|segment| segment.live_in.clone())
            .collect();
        let mut changed = false;
        for index in (0..segments.len()).rev() {
            let mut live_out = vec![false; locals];
            for successor in segments[index].successors.iter().copied() {
                for (slot, live) in previous[successor].iter().copied().enumerate() {
                    live_out[slot] |= live;
                }
            }
            let next: Vec<bool> = (0..locals)
                .map(|slot| {
                    segments[index].uses[slot]
                        || (live_out[slot] && !segments[index].definitions[slot])
                })
                .collect();
            changed |= next != segments[index].live_in;
            segments[index].live_in = next;
        }
        if !changed {
            break;
        }
    }
}

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
                    .fetch_add(region.plan.inline_call_sites as u64, Ordering::Relaxed);
                self.compiled_heap_read_sites
                    .fetch_add(region.plan.heap_read_sites as u64, Ordering::Relaxed);
                self.compiled_allocation_sites
                    .fetch_add(region.plan.allocation_sites as u64, Ordering::Relaxed);
                self.compiled_effect_sites
                    .fetch_add(region.plan.effect_sites as u64, Ordering::Relaxed);
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
            compiled_allocation_sites: self.compiled_allocation_sites.load(Ordering::Relaxed),
            compiled_effect_sites: self.compiled_effect_sites.load(Ordering::Relaxed),
        }
    }

    /// Reset every clock-free compilation counter.
    pub fn reset_metrics(&self) {
        self.compilation_attempts.store(0, Ordering::Relaxed);
        self.compiled_regions.store(0, Ordering::Relaxed);
        self.compiled_segments.store(0, Ordering::Relaxed);
        self.compiled_call_sites.store(0, Ordering::Relaxed);
        self.compiled_heap_read_sites.store(0, Ordering::Relaxed);
        self.compiled_allocation_sites.store(0, Ordering::Relaxed);
        self.compiled_effect_sites.store(0, Ordering::Relaxed);
    }
}

enum CompileError {
    Unsupported(UnsupportedReason),
    Backend,
}

impl From<UnsupportedReason> for CompileError {
    fn from(reason: UnsupportedReason) -> CompileError {
        CompileError::Unsupported(reason)
    }
}

fn compile_region(input: FunctionInput<'_>) -> Result<CompiledRegion, CompileError> {
    let plan = RegionPlan::for_function(&input)?;
    let func = input.root.runtime;

    let mut flags = settings::builder();
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|_| CompileError::Backend)?;
    flags
        .set("is_pic", "false")
        .map_err(|_| CompileError::Backend)?;
    flags
        .set("opt_level", "speed")
        .map_err(|_| CompileError::Backend)?;
    let isa = cranelift_native::builder()
        .map_err(|_| CompileError::Backend)?
        .finish(settings::Flags::new(flags))
        .map_err(|_| CompileError::Backend)?;
    let pointer_type = isa.pointer_type();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I32));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    let id = module
        .declare_function("loom_scalar_region", Linkage::Local, &signature)
        .map_err(|_| CompileError::Backend)?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, id.as_u32());
    let mut frontend = FunctionBuilderContext::new();
    emit_region(
        &mut context.func,
        &mut frontend,
        pointer_type,
        func,
        &plan,
        &input,
    )?;
    module
        .define_function(id, &mut context)
        .map_err(|_| CompileError::Backend)?;
    module
        .finalize_definitions()
        .map_err(|_| CompileError::Backend)?;
    let code = module.get_finalized_function(id);
    // SAFETY: The generated function uses the exact `NativeFunction` C ABI.
    // `CompiledRegion` retains the module that owns the executable memory.
    let entry = unsafe { mem::transmute::<*const u8, NativeFunction>(code) };
    Ok(CompiledRegion {
        plan,
        entry,
        module: Mutex::new(Some(module)),
    })
}

#[derive(Clone, Copy)]
struct NativeValues<'a> {
    locals: &'a [Variable],
    local_states: &'a [Variable],
    stack: &'a [Variable],
    fuel: Variable,
    retired: Variable,
    local_pointer: ir::Value,
    local_state_pointer: ir::Value,
    stack_pointer: ir::Value,
    runtime_context: ir::Value,
    runtime_call: ir::Value,
    runtime_result_pointer: ir::Value,
    root_pointer: ir::Value,
    root_state_pointer: ir::Value,
    runtime_signature: ir::SigRef,
    exit_pointer: ir::Value,
}

#[derive(Clone, Copy)]
struct NativeRoot {
    bits: ir::Value,
    state: Option<ir::Value>,
}

#[derive(Clone, Copy)]
struct ExitEmission {
    retired: ir::Value,
    kind: u32,
    block: u32,
    instruction: u32,
    result: ir::Value,
}

#[derive(Clone, Copy)]
struct FaultPoint {
    block: u32,
    instruction: u32,
    prefix: u32,
}

struct InlineCallEmission<'a> {
    definition: FunctionDefinition<'a>,
    plan: &'a InlineFunctionPlan,
    root_local_kinds: &'a [ScalarKind],
    caller_stack_kinds: &'a [ScalarKind],
    deopt: FaultPoint,
    deopt_stack: &'a [ir::Value],
}

struct FieldExitEmission<'a> {
    point: FaultPoint,
    fault_stack: &'a [ir::Value],
    deopt_stack: &'a [ir::Value],
}

fn emit_region(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
    pointer_type: ir::Type,
    bytecode: &Func,
    plan: &RegionPlan,
    input: &FunctionInput<'_>,
) -> Result<(), CompileError> {
    let call_conv = function.signature.call_conv;
    let mut builder = FunctionBuilder::new(function, frontend);
    let mut runtime_signature = ir::Signature::new(call_conv);
    runtime_signature.params.push(AbiParam::new(pointer_type));
    runtime_signature.params.push(AbiParam::new(types::I32));
    runtime_signature.params.push(AbiParam::new(types::I64));
    runtime_signature.params.push(AbiParam::new(types::I64));
    runtime_signature.params.push(AbiParam::new(types::I32));
    runtime_signature.params.push(AbiParam::new(pointer_type));
    runtime_signature.returns.push(AbiParam::new(types::I32));
    let runtime_signature = builder.import_signature(runtime_signature);
    let entry_block = builder.create_block();
    let invalid_block = builder.create_block();
    let blocks: Vec<ir::Block> = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect();

    builder.switch_to_block(entry_block);
    builder.append_block_params_for_function_params(entry_block);
    let parameters = builder.block_params(entry_block);
    let local_pointer = parameters[0];
    let local_state_pointer = parameters[1];
    let stack_pointer = parameters[2];
    let initial_fuel = parameters[3];
    let entry = parameters[4];
    let runtime_context = parameters[5];
    let runtime_call = parameters[6];
    let runtime_result_pointer = parameters[7];
    let root_pointer = parameters[8];
    let root_state_pointer = parameters[9];
    let exit_pointer = parameters[10];

    let mut locals = Vec::with_capacity(plan.local_kinds.len());
    let mut local_states = Vec::with_capacity(plan.local_kinds.len());
    for slot in 0..plan.local_kinds.len() {
        let local = builder.declare_var(types::I64);
        let state = builder.declare_var(types::I8);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), local_pointer, offset);
        let local_state = builder.ins().load(
            types::I8,
            MemFlags::new(),
            local_state_pointer,
            state_offset,
        );
        builder.def_var(local, value);
        builder.def_var(state, local_state);
        locals.push(local);
        local_states.push(state);
    }
    let mut stack = Vec::with_capacity(plan.max_stack);
    for slot in 0..plan.max_stack {
        let variable = builder.declare_var(types::I64);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), stack_pointer, offset);
        builder.def_var(variable, value);
        stack.push(variable);
    }
    let fuel = builder.declare_var(types::I64);
    let retired = builder.declare_var(types::I64);
    builder.def_var(fuel, initial_fuel);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.def_var(retired, zero);
    let values = NativeValues {
        locals: &locals,
        local_states: &local_states,
        stack: &stack,
        fuel,
        retired,
        local_pointer,
        local_state_pointer,
        stack_pointer,
        runtime_context,
        runtime_call,
        runtime_result_pointer,
        root_pointer,
        root_state_pointer,
        runtime_signature,
        exit_pointer,
    };

    let mut dispatch = Switch::new();
    for (index, block) in blocks.iter().copied().enumerate() {
        dispatch.set_entry(index as u128, block);
    }
    dispatch.emit(&mut builder, entry, invalid_block);

    builder.switch_to_block(invalid_block);
    let retired_value = builder.use_var(retired);
    emit_exit(
        &mut builder,
        values,
        ExitEmission {
            retired: retired_value,
            kind: EXIT_INVALID_ENTRY,
            block: 0,
            instruction: 0,
            result: zero,
        },
        &[],
    )?;

    for (index, segment) in plan.segments.iter().enumerate() {
        builder.switch_to_block(blocks[index]);
        let body = builder.create_block();
        let fuel_exit = builder.create_block();
        let available = builder.use_var(fuel);
        let enough = builder.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            available,
            i64::from(segment.cost),
        );
        builder.ins().brif(enough, body, &[], fuel_exit, &[]);

        builder.switch_to_block(fuel_exit);
        let retired_value = builder.use_var(retired);
        let result = builder.ins().iconst(types::I64, 0);
        let entry_stack: Vec<ir::Value> = values
            .stack
            .iter()
            .take(segment.entry_stack.len())
            .map(|variable| builder.use_var(*variable))
            .collect();
        emit_exit(
            &mut builder,
            values,
            ExitEmission {
                retired: retired_value,
                kind: EXIT_FUEL,
                block: segment.block,
                instruction: segment.start,
                result,
            },
            &entry_stack,
        )?;

        builder.switch_to_block(body);
        emit_segment(
            &mut builder,
            bytecode,
            segment,
            &blocks,
            values,
            plan,
            input,
        )?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    let _ = pointer_type;
    Ok(())
}

fn emit_segment(
    builder: &mut FunctionBuilder<'_>,
    bytecode: &Func,
    segment: &Segment,
    blocks: &[ir::Block],
    values: NativeValues<'_>,
    plan: &RegionPlan,
    input: &FunctionInput<'_>,
) -> Result<(), CompileError> {
    let mut stack: Vec<ir::Value> = values
        .stack
        .iter()
        .take(segment.entry_stack.len())
        .map(|variable| builder.use_var(*variable))
        .collect();
    let code =
        &bytecode.blocks[segment.block as usize][segment.start as usize..segment.end as usize];
    for (within, instruction) in code.iter().copied().enumerate() {
        let prefix = within as u32 + 1;
        match instruction {
            Instr::ConstUnit => {
                let value = builder.ins().iconst(types::I64, 0);
                stack.push(value);
            }
            Instr::New(class) => {
                let instruction = segment.start + within as u32;
                let site = segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == instruction)
                    .ok_or(CompileError::Backend)?;
                let mut roots = Vec::new();
                for (slot, (kind, variable)) in plan
                    .local_kinds
                    .iter()
                    .copied()
                    .zip(values.locals.iter().copied())
                    .enumerate()
                {
                    if matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            state: Some(builder.use_var(values.local_states[slot])),
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
                let value = emit_allocate_instance(
                    builder,
                    values,
                    class,
                    &roots,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix,
                    },
                    &stack,
                )?;
                stack.push(value);
            }
            Instr::ConstBool(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                stack.push(value);
            }
            Instr::ConstInt(value) => {
                let value = builder.ins().iconst(types::I64, value);
                stack.push(value);
            }
            Instr::ConstFloat(bits) => {
                let value = builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64);
                stack.push(value);
            }
            Instr::OpConst(operation) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(operation)));
            }
            Instr::LoadLocal(slot) => {
                stack.push(builder.use_var(values.locals[slot as usize]));
            }
            Instr::StoreLocal(slot) => {
                let value = pop_native(&mut stack)?;
                builder.def_var(values.locals[slot as usize], value);
                let state = builder
                    .ins()
                    .iconst(types::I8, i64::from(LOCAL_DIRTY | LOCAL_INITIALIZED));
                builder.def_var(values.local_states[slot as usize], state);
            }
            Instr::Pop => {
                pop_native(&mut stack)?;
            }
            Instr::LoadField(field) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let kind = segment
                    .field_results
                    .iter()
                    .find(|(at, _)| *at == instruction)
                    .map(|(_, kind)| *kind)
                    .ok_or(CompileError::Backend)?;
                let value = emit_load_field(
                    builder,
                    values,
                    reference,
                    field,
                    kind.representation(),
                    FieldExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::Add | Instr::Sub | Instr::Mul => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let (result, overflow) = match instruction {
                    Instr::Add => builder.ins().sadd_overflow(left, right),
                    Instr::Sub => builder.ins().ssub_overflow(left, right),
                    Instr::Mul => builder.ins().smul_overflow(left, right),
                    _ => unreachable!(),
                };
                let result = emit_overflow_check(
                    builder,
                    values,
                    overflow,
                    result,
                    FaultPoint {
                        block: segment.block,
                        instruction: segment.start + prefix,
                        prefix,
                    },
                    &stack,
                )?;
                stack.push(result);
            }
            Instr::Div | Instr::Rem => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: segment.start + prefix,
                    prefix,
                };
                let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                emit_fault_check(builder, values, zero, EXIT_DIVIDE_BY_ZERO, point, &stack)?;
                let minimum = builder.ins().iconst(types::I64, i64::MIN);
                let minimum_left = builder.ins().icmp(IntCC::Equal, left, minimum);
                let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                let overflow = builder.ins().band(minimum_left, negative_one);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTEGER_OVERFLOW,
                    point,
                    &stack,
                )?;
                let result = if matches!(instruction, Instr::Div) {
                    builder.ins().sdiv(left, right)
                } else {
                    builder.ins().srem(left, right)
                };
                stack.push(result);
            }
            Instr::Neg => {
                let value = pop_native(&mut stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (result, overflow) = builder.ins().ssub_overflow(zero, value);
                let result = emit_overflow_check(
                    builder,
                    values,
                    overflow,
                    result,
                    FaultPoint {
                        block: segment.block,
                        instruction: segment.start + prefix,
                        prefix,
                    },
                    &stack,
                )?;
                stack.push(result);
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                stack.push(builder.ins().bxor_imm(value, 1));
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = match instruction {
                    Instr::LtInt => IntCC::SignedLessThan,
                    Instr::LeInt => IntCC::SignedLessThanOrEqual,
                    Instr::GtInt => IntCC::SignedGreaterThan,
                    Instr::GeInt => IntCC::SignedGreaterThanOrEqual,
                    Instr::EqInt => IntCC::Equal,
                    Instr::NeInt => IntCC::NotEqual,
                    _ => unreachable!(),
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::EqBool | Instr::NeBool => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqBool) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::Numeric(operation) => {
                emit_float_instruction(builder, &mut stack, operation)?;
            }
            Instr::Call(_)
            | Instr::Perform { .. }
            | Instr::PerformValue { .. }
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Return => {}
            _ => {
                return Err(CompileError::Unsupported(
                    UnsupportedReason::UnsupportedInstruction,
                ))
            }
        }
    }

    if let SegmentExit::Call { target, .. } = segment.exit {
        let call_instruction = segment.end - 1;
        let prefix = segment.end - segment.start - 1;
        if let Some(inline) = plan.inline_functions.get(&target) {
            let definition = input
                .definition(target)
                .ok_or(CompileError::Unsupported(UnsupportedReason::MissingSource))?;
            let deopt_stack = stack.clone();
            let caller_kind_count = segment
                .boundary_stack
                .len()
                .checked_sub(inline.params.len())
                .ok_or(CompileError::Backend)?;
            emit_inline_call(
                builder,
                values,
                &mut stack,
                InlineCallEmission {
                    definition,
                    plan: inline,
                    root_local_kinds: &plan.local_kinds,
                    caller_stack_kinds: &segment.boundary_stack[..caller_kind_count],
                    deopt: FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix,
                    },
                    deopt_stack: &deopt_stack,
                },
            )?;
            emit_charge(builder, values, segment.cost);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        } else {
            emit_charge(builder, values, segment.cost);
            let retired = builder.use_var(values.retired);
            let zero = builder.ins().iconst(types::I64, 0);
            emit_exit(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_CALL,
                    block: segment.block,
                    instruction: call_instruction,
                    result: zero,
                },
                &stack,
            )?;
        }
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Effect { .. }) {
        let effect_instruction = segment.end - 1;
        emit_charge(builder, values, segment.cost);
        let retired = builder.use_var(values.retired);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_EFFECT,
                block: segment.block,
                instruction: effect_instruction,
                result: zero,
            },
            &stack,
        )?;
        return Ok(());
    }

    emit_charge(builder, values, segment.cost);
    match segment.exit {
        SegmentExit::Jump { .. } => {
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        }
        SegmentExit::Conditional { jump_on_true, .. } => {
            let condition = pop_native(&mut stack)?;
            define_stack(builder, values, &stack)?;
            let condition = builder.ins().icmp_imm(IntCC::NotEqual, condition, 0);
            let target = blocks[segment.successors[0]];
            let fallthrough = blocks[segment.successors[1]];
            if jump_on_true {
                builder.ins().brif(condition, target, &[], fallthrough, &[]);
            } else {
                builder.ins().brif(condition, fallthrough, &[], target, &[]);
            }
        }
        SegmentExit::Call { .. } => unreachable!(),
        SegmentExit::Allocation { .. } => {
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        }
        SegmentExit::Effect { .. } => unreachable!(),
        SegmentExit::Return => {
            let result = pop_native(&mut stack)?;
            let retired = builder.use_var(values.retired);
            emit_exit(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_RETURN,
                    block: segment.block,
                    instruction: segment.end,
                    result,
                },
                &stack,
            )?;
        }
    }
    Ok(())
}

fn emit_inline_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    caller_stack: &mut Vec<ir::Value>,
    call: InlineCallEmission<'_>,
) -> Result<(), CompileError> {
    let argument_start = caller_stack
        .len()
        .checked_sub(call.plan.params.len())
        .ok_or(CompileError::Backend)?;
    let arguments = caller_stack.split_off(argument_start);
    let mut locals = vec![None; call.plan.local_kinds.len()];
    for (slot, value) in arguments.into_iter().enumerate() {
        locals[slot] = Some(value);
    }
    let mut stack = Vec::with_capacity(call.plan.max_stack);
    let code = call
        .definition
        .runtime
        .blocks
        .first()
        .ok_or(CompileError::Backend)?;
    for (instruction_index, instruction) in code.iter().copied().enumerate() {
        match instruction {
            Instr::ConstUnit => stack.push(builder.ins().iconst(types::I64, 0)),
            Instr::ConstBool(value) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(value)));
            }
            Instr::ConstInt(value) => stack.push(builder.ins().iconst(types::I64, value)),
            Instr::ConstFloat(bits) => stack.push(
                builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64),
            ),
            Instr::OpConst(operation) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(operation)));
            }
            Instr::LoadLocal(slot) => {
                let value = locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                    .ok_or(CompileError::Backend)?;
                stack.push(value);
            }
            Instr::StoreLocal(slot) => {
                let value = pop_native(&mut stack)?;
                *locals.get_mut(slot as usize).ok_or(CompileError::Backend)? = Some(value);
            }
            Instr::Pop => {
                pop_native(&mut stack)?;
            }
            Instr::New(class) => {
                let site = call
                    .plan
                    .allocations
                    .iter()
                    .find(|site| site.instruction as usize == instruction_index)
                    .ok_or(CompileError::Backend)?;
                let mut roots = Vec::new();
                for (slot, (kind, variable)) in call
                    .root_local_kinds
                    .iter()
                    .copied()
                    .zip(values.locals.iter().copied())
                    .enumerate()
                {
                    if matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            state: Some(builder.use_var(values.local_states[slot])),
                        });
                    }
                }
                extend_stack_roots(&mut roots, call.caller_stack_kinds, caller_stack)?;
                for ((kind, initialized), value) in call
                    .plan
                    .local_kinds
                    .iter()
                    .copied()
                    .zip(site.initialized.iter().copied())
                    .zip(locals.iter().copied())
                {
                    if initialized && matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: value.ok_or(CompileError::Backend)?,
                            state: None,
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
                let (status, value) = emit_allocation_call(builder, values, class, &roots, false)?;
                let replay = builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
                emit_fault_check(
                    builder,
                    values,
                    replay,
                    EXIT_ALLOCATION,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(value);
            }
            Instr::Add | Instr::Sub | Instr::Mul => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let (result, overflow) = match instruction {
                    Instr::Add => builder.ins().sadd_overflow(left, right),
                    Instr::Sub => builder.ins().ssub_overflow(left, right),
                    Instr::Mul => builder.ins().smul_overflow(left, right),
                    _ => unreachable!(),
                };
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(result);
            }
            Instr::Div | Instr::Rem => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                emit_fault_check(
                    builder,
                    values,
                    zero,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                let minimum = builder.ins().iconst(types::I64, i64::MIN);
                let minimum_left = builder.ins().icmp(IntCC::Equal, left, minimum);
                let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                let overflow = builder.ins().band(minimum_left, negative_one);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                let result = if matches!(instruction, Instr::Div) {
                    builder.ins().sdiv(left, right)
                } else {
                    builder.ins().srem(left, right)
                };
                stack.push(result);
            }
            Instr::Neg => {
                let value = pop_native(&mut stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (result, overflow) = builder.ins().ssub_overflow(zero, value);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(result);
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                stack.push(builder.ins().bxor_imm(value, 1));
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = match instruction {
                    Instr::LtInt => IntCC::SignedLessThan,
                    Instr::LeInt => IntCC::SignedLessThanOrEqual,
                    Instr::GtInt => IntCC::SignedGreaterThan,
                    Instr::GeInt => IntCC::SignedGreaterThanOrEqual,
                    Instr::EqInt => IntCC::Equal,
                    Instr::NeInt => IntCC::NotEqual,
                    _ => unreachable!(),
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::EqBool | Instr::NeBool => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqBool) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::Numeric(operation) => {
                emit_float_instruction(builder, &mut stack, operation)?;
            }
            Instr::Return => {
                let result = pop_native(&mut stack)?;
                if !stack.is_empty() {
                    return Err(CompileError::Backend);
                }
                caller_stack.push(result);
                return Ok(());
            }
            _ => {
                return Err(CompileError::Unsupported(
                    UnsupportedReason::UnsupportedInstruction,
                ));
            }
        }
    }
    Err(CompileError::Backend)
}

fn pop_native(stack: &mut Vec<ir::Value>) -> Result<ir::Value, CompileError> {
    stack.pop().ok_or(CompileError::Backend)
}

fn emit_charge(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>, cost: u32) {
    let fuel = builder.use_var(values.fuel);
    let retired = builder.use_var(values.retired);
    let fuel = builder.ins().iadd_imm(fuel, -i64::from(cost));
    let retired = builder.ins().iadd_imm(retired, i64::from(cost));
    builder.def_var(values.fuel, fuel);
    builder.def_var(values.retired, retired);
}

fn emit_overflow_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    overflow: ir::Value,
    result: ir::Value,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<ir::Value, CompileError> {
    emit_fault_check(
        builder,
        values,
        overflow,
        EXIT_INTEGER_OVERFLOW,
        point,
        stack,
    )?;
    Ok(result)
}

fn emit_fault_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    faulted: ir::Value,
    kind: u32,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    let fault = builder.create_block();
    let success = builder.create_block();
    builder.ins().brif(faulted, fault, &[], success, &[]);
    builder.switch_to_block(fault);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(point.prefix));
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind,
            block: point.block,
            instruction: point.instruction,
            result: zero,
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

fn emit_load_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    field: u32,
    representation: ValueRepr,
    exit: FieldExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let operation = builder
        .ins()
        .iconst(types::I32, i64::from(RUNTIME_LOAD_FIELD));
    let field = builder.ins().iconst(types::I64, i64::from(field));
    let representation = builder
        .ins()
        .iconst(types::I32, representation as u32 as i64);
    let call = builder.ins().call_indirect(
        values.runtime_signature,
        values.runtime_call,
        &[
            values.runtime_context,
            operation,
            reference,
            field,
            representation,
            values.runtime_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let mismatch = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_TYPE_MISMATCH));
    emit_fault_check(
        builder,
        values,
        mismatch,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let uninitialized =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_UNINITIALIZED_FIELD));
    emit_fault_check(
        builder,
        values,
        uninitialized,
        EXIT_UNINITIALIZED_FIELD,
        exit.point,
        exit.fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.runtime_result_pointer,
        0,
    ))
}

fn extend_stack_roots(
    roots: &mut Vec<NativeRoot>,
    kinds: &[ScalarKind],
    values: &[ir::Value],
) -> Result<(), CompileError> {
    if kinds.len() != values.len() {
        return Err(CompileError::Backend);
    }
    for (kind, value) in kinds.iter().copied().zip(values.iter().copied()) {
        if matches!(kind, ScalarKind::Object(_)) {
            roots.push(NativeRoot {
                bits: value,
                state: None,
            });
        }
    }
    Ok(())
}

fn emit_allocate_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    roots: &[NativeRoot],
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<ir::Value, CompileError> {
    let (status, result) = emit_allocation_call(builder, values, class, roots, true)?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(builder, values, heap_limit, EXIT_HEAP_LIMIT, point, stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, stack)?;
    Ok(result)
}

fn emit_allocation_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    roots: &[NativeRoot],
    allow_collection: bool,
) -> Result<(ir::Value, ir::Value), CompileError> {
    for (slot, root) in roots.iter().copied().enumerate() {
        let value_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder.ins().store(
            MemFlags::new(),
            root.bits,
            values.root_pointer,
            value_offset,
        );
        let state = root.state.unwrap_or_else(|| {
            builder
                .ins()
                .iconst(types::I8, i64::from(LOCAL_INITIALIZED))
        });
        builder.ins().store(
            MemFlags::new(),
            state,
            values.root_state_pointer,
            state_offset,
        );
    }
    let operation = builder
        .ins()
        .iconst(types::I32, i64::from(RUNTIME_ALLOC_INSTANCE));
    let class = builder.ins().iconst(types::I64, i64::from(class));
    let collection = builder
        .ins()
        .iconst(types::I64, i64::from(allow_collection));
    let root_count = builder.ins().iconst(
        types::I32,
        i64::try_from(roots.len()).map_err(|_| CompileError::Backend)?,
    );
    let call = builder.ins().call_indirect(
        values.runtime_signature,
        values.runtime_call,
        &[
            values.runtime_context,
            operation,
            class,
            collection,
            root_count,
            values.runtime_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.runtime_result_pointer,
        0,
    );
    Ok((status, result))
}

fn emit_interpreter_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    replay: ir::Value,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    let interpreter = builder.create_block();
    let success = builder.create_block();
    builder.ins().brif(replay, interpreter, &[], success, &[]);
    builder.switch_to_block(interpreter);
    let retired = builder.use_var(values.retired);
    let retired = builder
        .ins()
        .iadd_imm(retired, i64::from(point.prefix.saturating_sub(1)));
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_INTERPRETER,
            block: point.block,
            instruction: point.instruction.saturating_sub(1),
            result: zero,
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

fn emit_float_instruction(
    builder: &mut FunctionBuilder<'_>,
    stack: &mut Vec<ir::Value>,
    operation: NumericInstr,
) -> Result<(), CompileError> {
    match operation {
        NumericInstr::FloatNeg => {
            let value = float_value(builder, pop_native(stack)?);
            let value = builder.ins().fneg(value);
            stack.push(canonical_float(builder, value));
        }
        NumericInstr::FloatAdd
        | NumericInstr::FloatSub
        | NumericInstr::FloatMul
        | NumericInstr::FloatDiv => {
            let right_bits = pop_native(stack)?;
            let left_bits = pop_native(stack)?;
            let right = float_value(builder, right_bits);
            let left = float_value(builder, left_bits);
            let value = match operation {
                NumericInstr::FloatAdd => builder.ins().fadd(left, right),
                NumericInstr::FloatSub => builder.ins().fsub(left, right),
                NumericInstr::FloatMul => builder.ins().fmul(left, right),
                NumericInstr::FloatDiv => builder.ins().fdiv(left, right),
                _ => unreachable!(),
            };
            stack.push(canonical_float(builder, value));
        }
        NumericInstr::FloatEq
        | NumericInstr::FloatNe
        | NumericInstr::FloatLt
        | NumericInstr::FloatLe
        | NumericInstr::FloatGt
        | NumericInstr::FloatGe => {
            let right_bits = pop_native(stack)?;
            let left_bits = pop_native(stack)?;
            let right = float_value(builder, right_bits);
            let left = float_value(builder, left_bits);
            let compared = match operation {
                NumericInstr::FloatEq | NumericInstr::FloatNe => {
                    let equal = builder.ins().fcmp(FloatCC::Equal, left, right);
                    let left_nan = builder.ins().fcmp(FloatCC::Unordered, left, left);
                    let right_nan = builder.ins().fcmp(FloatCC::Unordered, right, right);
                    let both_nan = builder.ins().band(left_nan, right_nan);
                    let equal = builder.ins().bor(equal, both_nan);
                    if matches!(operation, NumericInstr::FloatNe) {
                        builder.ins().bxor_imm(equal, 1)
                    } else {
                        equal
                    }
                }
                NumericInstr::FloatLt => builder.ins().fcmp(FloatCC::LessThan, left, right),
                NumericInstr::FloatLe => builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right),
                NumericInstr::FloatGt => builder.ins().fcmp(FloatCC::GreaterThan, left, right),
                NumericInstr::FloatGe => {
                    builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right)
                }
                _ => unreachable!(),
            };
            stack.push(builder.ins().uextend(types::I64, compared));
        }
        _ => {
            return Err(CompileError::Unsupported(
                UnsupportedReason::UnsupportedInstruction,
            ))
        }
    }
    Ok(())
}

fn float_value(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    builder.ins().bitcast(types::F64, MemFlags::new(), bits)
}

fn canonical_float(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
    let canonical = builder.ins().iconst(types::I64, CANONICAL_NAN_BITS as i64);
    builder.ins().select(is_nan, canonical, bits)
}

fn emit_exit(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    for (slot, variable) in values.locals.iter().copied().enumerate() {
        let value = builder.use_var(variable);
        let state = builder.use_var(values.local_states[slot]);
        let local_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value, values.local_pointer, local_offset);
        builder.ins().store(
            MemFlags::new(),
            state,
            values.local_state_pointer,
            state_offset,
        );
    }
    for (slot, value) in stack.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value, values.stack_pointer, offset);
    }
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
        exit.retired,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, kind),
        exit.kind,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, block),
        exit.block,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, instruction),
        exit.instruction,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, stack_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, result),
        exit.result,
    )?;
    builder.ins().return_(&[]);
    Ok(())
}

fn define_stack(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    if stack.len() > values.stack.len() {
        return Err(CompileError::Backend);
    }
    for (variable, value) in values.stack.iter().copied().zip(stack.iter().copied()) {
        builder.def_var(variable, value);
    }
    Ok(())
}

fn store_i32_constant(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: u32,
) -> Result<(), CompileError> {
    let value = builder.ins().iconst(types::I32, i64::from(value));
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

fn store_i64(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, BcClassKind, BcType, Module, NO_PARENT};

    fn module(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![2, 2],
                blocks,
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        }
    }

    #[test]
    fn segments_split_conditional_fallthrough() {
        let module = module(vec![
            vec![Instr::ConstInt(0), Instr::StoreLocal(0), Instr::Jump(1)],
            vec![
                Instr::LoadLocal(0),
                Instr::ConstInt(10),
                Instr::LtInt,
                Instr::JumpIfFalse(3),
                Instr::Jump(2),
            ],
            vec![
                Instr::LoadLocal(0),
                Instr::ConstInt(1),
                Instr::Add,
                Instr::StoreLocal(0),
                Instr::Jump(1),
            ],
            vec![Instr::LoadLocal(0), Instr::Return],
        ]);
        lm_verify::verify_module(&module).expect("the loop verifies");
        let segments = split_segments(&module.funcs[0]).expect("the loop splits");
        assert_eq!(segments.len(), 5);
        assert_eq!((segments[1].block, segments[1].start), (1, 0));
        assert_eq!((segments[2].block, segments[2].start), (1, 4));
    }

    #[test]
    fn liveness_ignores_a_local_replaced_before_use() {
        let mut segments = vec![Segment {
            block: 0,
            start: 0,
            end: 3,
            cost: 3,
            exit: SegmentExit::Return,
            uses: vec![false, true],
            definitions: vec![true, false],
            successors: vec![],
            live_in: vec![],
            entry_stack: vec![],
            exit_stack: vec![],
            boundary_stack: vec![],
            field_results: vec![],
            replay_stacks: vec![],
            fault_stacks: vec![],
            allocations: vec![],
        }];
        compute_liveness(&mut segments, 2);
        assert_eq!(segments[0].live_in, vec![false, true]);
    }

    fn field_module() -> Module {
        let mut module = module(vec![vec![
            Instr::LoadLocal(0),
            Instr::LoadField(0),
            Instr::Return,
        ]]);
        module.types.push(BcType::Class(0));
        module.classes.push(BcClass {
            name: "Pair".to_string(),
            key: "Pair".to_string(),
            is_final: true,
            is_frozen: false,
            parent: NO_PARENT,
            parent_args: vec![],
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("left".to_string(), 2)],
            methods: vec![],
            field_defaults: vec![false],
            own_start: 0,
            has_init: false,
        });
        module.class_bounds.push(vec![]);
        module.funcs[0].param_names = vec!["pair".to_string()];
        module.funcs[0].params = vec![4];
        module.funcs[0].param_muts = vec![false];
        module.funcs[0].local_types = vec![4];
        module.funcs.push(Func {
            name: "main".to_string(),
            param_names: vec![],
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: 2,
            row: vec![],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![Instr::ConstInt(0), Instr::Return]],
        });
        module.func_bounds.push(vec![]);
        module.entry = 1;
        module
    }

    struct FieldRuntime {
        result: RuntimeResult,
    }

    impl Runtime for FieldRuntime {
        fn load_field(
            &mut self,
            reference: lm_value::ObjRef,
            field: u32,
            expected: ValueRepr,
        ) -> RuntimeResult {
            assert_eq!(reference.slot, 3);
            assert_eq!(reference.generation, 7);
            assert_eq!(field, 0);
            assert_eq!(expected, ValueRepr::Int);
            self.result
        }

        fn allocate_instance(
            &mut self,
            _class: u32,
            _root_bits: &[u64],
            _root_states: &[u8],
            _allow_collection: bool,
        ) -> RuntimeResult {
            RuntimeResult::Interpreter
        }
    }

    #[test]
    fn native_field_load_uses_the_checked_runtime_boundary() {
        let module = field_module();
        let bundle = lm_abi::standard_bundle();
        lm_verify::verify_module_with_bundle(&module, &bundle).expect("the field load verifies");
        let engine = JitEngine::default();
        let region = engine
            .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
            .expect("the field load compiles");
        let reference = u64::from(3u32) | (u64::from(7u32) << 32);
        let mut locals = [reference];
        let mut dirty = [0];
        let mut operands = vec![0; region.max_stack()];
        let mut runtime = FieldRuntime {
            result: RuntimeResult::Value(41),
        };
        let exit = region
            .execute(&mut runtime, 0, &mut locals, &mut dirty, &mut operands, 3)
            .expect("the field load executes");
        assert_eq!(exit.kind(), ExitKind::Return);
        assert_eq!(exit.retired(), 3);
        assert_eq!(exit.result(), 41);
    }

    #[test]
    fn native_field_fault_keeps_the_exact_program_point() {
        let module = field_module();
        let bundle = lm_abi::standard_bundle();
        let engine = JitEngine::default();
        let region = engine
            .compile(FunctionInput::new(0, &module.funcs[0], &module, &bundle, 0))
            .expect("the field load compiles");
        let reference = u64::from(3u32) | (u64::from(7u32) << 32);
        let mut locals = [reference];
        let mut dirty = [0];
        let mut operands = vec![0; region.max_stack()];
        let mut runtime = FieldRuntime {
            result: RuntimeResult::UninitializedField,
        };
        let exit = region
            .execute(&mut runtime, 0, &mut locals, &mut dirty, &mut operands, 3)
            .expect("the field fault executes");
        assert_eq!(exit.kind(), ExitKind::UninitializedField);
        assert_eq!(exit.retired(), 2);
        assert_eq!((exit.block(), exit.instruction()), (0, 2));
        assert_eq!(exit.stack_len(), 0);
    }
}
