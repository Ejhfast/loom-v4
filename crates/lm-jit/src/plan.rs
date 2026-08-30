//! Verified bytecode analysis and immutable native region plans.

use crate::{Failure, MAX_REGION_INSTRUCTIONS, MAX_REGION_LOCALS, MAX_REGION_STACK};
use lm_bytecode::{BcType, ExtendedInstr, Func, Instr, Module, NativeInstr};
use std::collections::HashMap;
use std::sync::Arc;

/// One scalar representation used by the native ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Unit,
    Bool,
    Int,
    Float,
    Char,
    /// One heap object with a source-unit type index.
    Object(u32),
    /// One canonical tagged value with a source-unit type index.
    Tagged(u32),
    Operation,
}

#[derive(Clone, Copy)]
pub(super) struct FunctionDefinition<'a> {
    pub(super) function: u32,
    pub(super) runtime: &'a Func,
    pub(super) source: &'a Module,
    pub(super) bundle: &'a Arc<lm_abi::AbiBundle>,
    pub(super) source_function: u32,
    pub(super) class_relocation: Option<&'a [u32]>,
}

/// Immutable verified input for one native compilation.
pub struct FunctionInput<'a> {
    pub(super) root: FunctionDefinition<'a>,
    direct_callees: Vec<FunctionDefinition<'a>>,
    runtime_string_count: usize,
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
                class_relocation: None,
            },
            direct_callees: Vec::new(),
            runtime_string_count: source.strings.len(),
        }
    }

    /// Supply the relocated string-table size for byte literal slots.
    pub fn set_runtime_string_count(&mut self, count: usize) {
        self.runtime_string_count = count;
    }

    /// Supply the source-to-runtime class relocation for this unit.
    pub fn set_class_relocation(&mut self, classes: &'a [u32]) {
        self.root.class_relocation = Some(classes);
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
            class_relocation: None,
        });
    }

    /// Supply one direct callee and its source-to-runtime class relocation.
    pub fn add_relocated_direct_callee(
        &mut self,
        function: u32,
        runtime: &'a Func,
        source: &'a Module,
        bundle: &'a Arc<lm_abi::AbiBundle>,
        source_function: u32,
        classes: &'a [u32],
    ) {
        self.add_direct_callee(function, runtime, source, bundle, source_function);
        if let Some(definition) = self
            .direct_callees
            .iter_mut()
            .find(|definition| definition.function == function)
        {
            definition.class_relocation = Some(classes);
        }
    }

    pub(super) fn runtime_string_count(&self) -> usize {
        self.runtime_string_count
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
    pub compiled_heap_write_sites: u64,
    pub compiled_allocation_sites: u64,
    pub compiled_effect_sites: u64,
    pub compiled_interpreter_sites: u64,
}

/// One supported native entry and its required scalar values.
#[derive(Debug, Clone, Copy)]
pub struct EntryPlan<'a> {
    pub(super) index: u32,
    pub(super) live_locals: &'a [bool],
    pub(super) operand_kinds: &'a [ScalarKind],
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
    StackLimit,
    Interpreter,
    Replay,
    Literal,
    Unreachable,
    GrowActivation,
    TypeResolution,
    TypeEnvironment,
}

/// One validated native exit record.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionExit {
    pub(super) retired: u64,
    pub(super) kind: ExitKind,
    pub(super) block: u32,
    pub(super) instruction: u32,
    pub(super) stack_len: u32,
    pub(super) result_tag: u64,
    pub(super) result: u64,
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

    /// Return the canonical result tag.
    #[inline(always)]
    pub fn result_tag(&self) -> u64 {
        self.result_tag
    }

    #[inline(always)]
    pub fn result(&self) -> u64 {
        self.result
    }

    /// Add retired instructions from an earlier detached frame.
    pub fn add_prior_retired(mut self, retired: u64) -> Result<ExecutionExit, Failure> {
        self.retired = self
            .retired
            .checked_add(retired)
            .ok_or(Failure::BackendUnavailable)?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UnsupportedReason {
    MissingSource,
    CapturedFunction,
    NonScalarType,
    UnsupportedInstruction,
    InvalidStack,
    InvalidControlFlow,
    RegionLimit,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SegmentExit {
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
        app: Option<u32>,
        fallthrough_ip: u32,
    },
    Allocation {
        fallthrough_ip: u32,
    },
    Effect {
        fallthrough_ip: u32,
    },
    Interpreter {
        fallthrough_ip: Option<u32>,
    },
    Unreachable,
    Return,
}

#[derive(Debug, Clone)]
pub(super) struct Segment {
    pub(super) block: u32,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) cost: u32,
    pub(super) exit: SegmentExit,
    pub(super) uses: Vec<bool>,
    pub(super) definitions: Vec<bool>,
    pub(super) successors: Vec<usize>,
    pub(super) live_in: Vec<bool>,
    pub(super) entry_stack: Vec<ScalarKind>,
    pub(super) call_contract: Option<CallContract>,
    pub(super) exit_stack: Vec<ScalarKind>,
    pub(super) boundary_stack: Vec<ScalarKind>,
    pub(super) heap_accesses: Vec<HeapAccess>,
    pub(super) option_accesses: Vec<OptionAccess>,
    pub(super) fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ValueContract {
    pub(super) kind: ScalarKind,
    pub(super) object: Option<ObjectContract>,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ObjectContract {
    Str,
    Text,
    Instance(u32),
    List,
    Map,
    Tuple,
    Closure,
    Bytes,
    Digest,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct HeapAccess {
    pub(super) instruction: u32,
    pub(super) kind: HeapAccessKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum HeapAccessKind {
    LoadField {
        receiver_class: u32,
        value: ValueContract,
    },
    StoreField {
        receiver_class: u32,
        value: ValueContract,
    },
    TupleGet {
        value: ValueContract,
    },
    IsType {
        target_class: u32,
    },
    CastType {
        target_class: u32,
    },
    ListLen,
    ListAt {
        value: ValueContract,
    },
    ListSet {
        value: ValueContract,
    },
    ListPush {
        value: ValueContract,
    },
    ListReserve,
    ListReorder,
    ListCapacity,
    ListEpoch,
    ListIterLen,
    MapLen,
    MapEpoch,
    MapIterLen,
    DigestCompare,
    AsCallback,
    SealInstance {
        class: u32,
    },
    BytesLen,
    BytesAt,
    BytesGet,
    TextByteLen,
    TextScalarLen,
    TextAtByte,
    TextAt,
    TextIsBoundary,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct OptionAccess {
    pub(super) instruction: u32,
    pub(super) family_type: u32,
    pub(super) kind: OptionAccessKind,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OptionAccessKind {
    None,
    Payload { value: ValueContract },
    ListGet { value: ValueContract },
    IsType { target: OptionTarget },
    CastType { target: OptionTarget },
}

#[derive(Debug, Clone, Copy)]
pub(super) enum OptionTarget {
    Family,
    Some,
    None,
}

#[derive(Debug, Clone)]
pub(super) struct CallContract {
    pub(super) params: Vec<ScalarKind>,
    pub(super) local_count: usize,
    pub(super) result: ScalarKind,
}

#[derive(Debug, Clone, Copy)]
enum CallValueKind {
    Fixed(ScalarKind),
    Variable(u32),
}

#[derive(Debug, Clone)]
struct CallSignature {
    params: Vec<CallValueKind>,
    local_count: usize,
    result: CallValueKind,
}

#[derive(Debug, Clone)]
pub(super) struct RegionPlan {
    pub(super) local_kinds: Vec<ScalarKind>,
    pub(super) result_kind: ScalarKind,
    pub(super) max_stack: usize,
    pub(super) max_stack_values: usize,
    pub(super) max_roots: usize,
    pub(super) segments: Vec<Segment>,
    pub(super) entries: std::collections::HashMap<(u32, u32), usize>,
    pub(super) resume_entries: std::collections::HashMap<(u32, u32), u32>,
    pub(super) resume_targets: Vec<ResumeTarget>,
    pub(super) call_sites: usize,
    pub(super) heap_read_sites: usize,
    pub(super) heap_write_sites: usize,
    pub(super) allocation_sites: usize,
    pub(super) collection_sites: usize,
    pub(super) effect_sites: usize,
    pub(super) interpreter_sites: usize,
    pub(super) type_resolution_sites: usize,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ResumeTarget {
    pub(super) segment: usize,
    pub(super) offset: usize,
}

struct SegmentAnalysis {
    uses: Vec<bool>,
    definitions: Vec<bool>,
    exit_stack: Vec<ScalarKind>,
    max_stack: usize,
    max_stack_values: usize,
    boundary_stack: Vec<ScalarKind>,
    heap_accesses: Vec<HeapAccess>,
    option_accesses: Vec<OptionAccess>,
    fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
    replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    allocations: Vec<AllocationSite>,
    call_contract: Option<CallContract>,
}

#[derive(Debug, Clone)]
struct VerifiedPoint {
    initialized: Vec<bool>,
    stack: Vec<ScalarKind>,
}

struct SegmentAnalysisContext<'a> {
    func: &'a Func,
    source_func: &'a Func,
    module: &'a Module,
    locals: &'a [ScalarKind],
    calls: &'a HashMap<u32, CallSignature>,
    class_relocation: Option<&'a [u32]>,
}

#[derive(Debug, Clone)]
pub(super) struct AllocationSite {
    pub(super) instruction: u32,
    pub(super) stack: Vec<ScalarKind>,
}

impl RegionPlan {
    pub(super) fn for_function(input: &FunctionInput<'_>) -> Result<RegionPlan, UnsupportedReason> {
        let runtime = input.root.runtime;
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
        if source_func.blocks.len() != runtime.blocks.len()
            || source_func.local_types.len() != runtime.local_types.len()
            || source_func
                .blocks
                .iter()
                .zip(&runtime.blocks)
                .any(|(source, linked)| source.len() != linked.len())
        {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        let mut segments = split_segments(runtime)?;
        let mut requested_points = Vec::new();
        for (block, instructions) in runtime.blocks.iter().enumerate() {
            let block = u32::try_from(block).map_err(|_| UnsupportedReason::RegionLimit)?;
            for instruction in 0..=instructions.len() {
                let instruction =
                    u32::try_from(instruction).map_err(|_| UnsupportedReason::RegionLimit)?;
                requested_points.push((block, instruction));
            }
        }
        let metadata = lm_verify::verify_function_metadata_at_with_bundle(
            source,
            input.root.bundle,
            input.root.source_function,
            &requested_points,
        )
        .map_err(|_| UnsupportedReason::MissingSource)?;
        let verified_points: HashMap<(u32, u32), VerifiedPoint> = requested_points
            .into_iter()
            .zip(metadata.points())
            .filter_map(|(point, state)| state.as_ref().map(|state| (point, state)))
            .map(|(point, state)| {
                let stack = state
                    .stack()
                    .iter()
                    .map(|ty| scalar_kind_in(source, metadata.types(), *ty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((
                    point,
                    VerifiedPoint {
                        initialized: state.locals().iter().map(Option::is_some).collect(),
                        stack,
                    },
                ))
            })
            .collect::<Result<_, UnsupportedReason>>()?;
        let local_kinds = source_func
            .local_types
            .iter()
            .map(|ty| scalar_kind(source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result_kind = scalar_kind(source, source_func.ret)?;
        let call_contracts = call_contracts(input)?;
        let entries: std::collections::HashMap<(u32, u32), usize> = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| ((segment.block, segment.start), index))
            .collect();
        resolve_successors(&mut segments, &entries)?;
        let mut max_stack = 0;
        let mut max_stack_values = 0;
        let mut call_sites = 0;
        let mut heap_read_sites = 0;
        let mut heap_write_sites = 0;
        let mut allocation_sites = 0;
        let mut collection_sites = 0;
        let mut effect_sites = 0;
        let mut interpreter_sites = 0;
        let mut type_resolution_sites = 0;
        let analysis_context = SegmentAnalysisContext {
            func: runtime,
            source_func,
            module: source,
            locals: &local_kinds,
            calls: &call_contracts,
            class_relocation: input.root.class_relocation,
        };
        for (index, segment) in segments.iter_mut().enumerate() {
            let state = verified_points
                .get(&(segment.block, segment.start))
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            let entry_stack = state.stack.clone();
            segment.entry_stack = entry_stack.clone();
            let analysis = analyze_segment(&analysis_context, segment, &verified_points)?;
            segment.uses = analysis.uses;
            segment.definitions = analysis.definitions;
            segment.call_contract = analysis.call_contract;
            segment.exit_stack = analysis.exit_stack.clone();
            segment.boundary_stack = analysis.boundary_stack;
            segment.heap_accesses = analysis.heap_accesses;
            segment.option_accesses = analysis.option_accesses;
            type_resolution_sites += segment.option_accesses.len();
            heap_read_sites += segment
                .option_accesses
                .iter()
                .filter(|access| matches!(access.kind, OptionAccessKind::ListGet { .. }))
                .count();
            segment.fuel_stacks = analysis.fuel_stacks;
            for access in &segment.heap_accesses {
                match access.kind {
                    HeapAccessKind::StoreField { .. }
                    | HeapAccessKind::ListSet { .. }
                    | HeapAccessKind::ListPush { .. }
                    | HeapAccessKind::ListReserve
                    | HeapAccessKind::ListReorder
                    | HeapAccessKind::ListEpoch
                    | HeapAccessKind::MapEpoch
                    | HeapAccessKind::SealInstance { .. } => {
                        heap_write_sites += 1;
                        if matches!(
                            access.kind,
                            HeapAccessKind::ListPush { .. } | HeapAccessKind::ListReserve
                        ) {
                            collection_sites += 1;
                        }
                    }
                    _ => heap_read_sites += 1,
                }
            }
            segment.replay_stacks = analysis.replay_stacks;
            segment.fault_stacks = analysis.fault_stacks;
            segment.allocations = analysis.allocations;
            allocation_sites += segment.allocations.len();
            segment.cost = segment.end - segment.start;
            if matches!(segment.exit, SegmentExit::Call { .. }) {
                call_sites += 1;
                segment.cost = segment.cost.saturating_sub(1);
            }
            if matches!(segment.exit, SegmentExit::Effect { .. }) {
                segment.cost = segment.cost.saturating_sub(1);
                effect_sites += 1;
            }
            if matches!(segment.exit, SegmentExit::Interpreter { .. }) {
                segment.cost = segment.cost.saturating_sub(1);
                interpreter_sites += 1;
            }
            max_stack = max_stack.max(analysis.max_stack);
            max_stack_values = max_stack_values.max(analysis.max_stack_values);
            debug_assert_eq!(index, entries[&(segment.block, segment.start)]);
        }
        for segment in &segments {
            for successor in &segment.successors {
                if !stacks_use_equal_representations(
                    &segment.exit_stack,
                    &segments[*successor].entry_stack,
                ) {
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
            .filter(|kind| is_root_kind(**kind))
            .count();
        let max_roots = root_local_count
            .checked_add(max_stack)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let mut resume_entries = std::collections::HashMap::new();
        let mut resume_targets = Vec::new();
        for (segment_index, segment) in segments.iter().enumerate() {
            for offset in 1..segment.fuel_stacks.len() {
                let (instruction, _) = &segment.fuel_stacks[offset];
                let index = segments
                    .len()
                    .checked_add(resume_targets.len())
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(UnsupportedReason::RegionLimit)?;
                if resume_entries
                    .insert((segment.block, *instruction), index)
                    .is_some()
                {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                resume_targets.push(ResumeTarget {
                    segment: segment_index,
                    offset,
                });
            }
        }
        Ok(RegionPlan {
            local_kinds,
            result_kind,
            max_stack,
            max_stack_values,
            max_roots,
            segments,
            entries,
            resume_entries,
            resume_targets,
            call_sites,
            heap_read_sites,
            heap_write_sites,
            allocation_sites,
            collection_sites,
            effect_sites,
            interpreter_sites,
            type_resolution_sites,
        })
    }

    pub(super) fn distance_to_entry(&self, block: u32, instruction: u32) -> Option<u32> {
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
) -> Result<HashMap<u32, CallSignature>, UnsupportedReason> {
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
            .map(|ty| call_value_kind(definition.source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result = call_value_kind(definition.source, source_func.ret)?;
        contracts.insert(
            definition.function,
            CallSignature {
                params,
                local_count: source_func.local_types.len(),
                result,
            },
        );
    }
    Ok(contracts)
}

fn call_value_kind(module: &Module, ty: u32) -> Result<CallValueKind, UnsupportedReason> {
    match module.types.get(ty as usize) {
        Some(BcType::Var(variable)) => Ok(CallValueKind::Variable(*variable)),
        _ => scalar_kind(module, ty).map(CallValueKind::Fixed),
    }
}

fn instantiate_call(
    signature: &CallSignature,
    caller: &Module,
    app: Option<u32>,
) -> Result<CallContract, UnsupportedReason> {
    let application = match app {
        Some(app) => Some(
            caller
                .apps
                .get(app as usize)
                .ok_or(UnsupportedReason::InvalidControlFlow)?,
        ),
        None => None,
    };
    let instantiate = |value: CallValueKind| match value {
        CallValueKind::Fixed(kind) => Ok(kind),
        CallValueKind::Variable(variable) => {
            let ty = application
                .and_then(|application| application.types.get(variable as usize))
                .copied()
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            scalar_kind(caller, ty)
        }
    };
    Ok(CallContract {
        params: signature
            .params
            .iter()
            .copied()
            .map(instantiate)
            .collect::<Result<Vec<_>, _>>()?,
        local_count: signature.local_count,
        result: instantiate(signature.result)?,
    })
}

fn scalar_kind(module: &lm_bytecode::Module, ty: u32) -> Result<ScalarKind, UnsupportedReason> {
    scalar_kind_in(module, &module.types, ty)
}

fn scalar_kind_in(
    module: &lm_bytecode::Module,
    types: &[BcType],
    ty: u32,
) -> Result<ScalarKind, UnsupportedReason> {
    match types.get(ty as usize) {
        Some(BcType::Unit) => Ok(ScalarKind::Unit),
        Some(BcType::Bool) => Ok(ScalarKind::Bool),
        Some(BcType::Int) => Ok(ScalarKind::Int),
        Some(BcType::Float) => Ok(ScalarKind::Float),
        Some(
            BcType::Str
            | BcType::Map(_, _)
            | BcType::Fn(_, _, _, _)
            | BcType::Callback(_, _, _, _)
            | BcType::Digest,
        ) => Ok(ScalarKind::Object(ty)),
        Some(BcType::Class(class)) => {
            let core = lm_bytecode::corepin::declared_layout(module);
            if core.char_value == Some(*class) {
                Ok(ScalarKind::Char)
            } else {
                Ok(ScalarKind::Object(ty))
            }
        }
        Some(BcType::Inst(class, _)) if is_option_class(module, *class) => {
            Ok(ScalarKind::Tagged(ty))
        }
        Some(BcType::Inst(_, _) | BcType::List(_) | BcType::Tuple(_) | BcType::Bytes) => {
            Ok(ScalarKind::Object(ty))
        }
        Some(BcType::Op(_, _)) => Ok(ScalarKind::Operation),
        Some(BcType::Var(_) | BcType::Projection { .. }) => Ok(ScalarKind::Tagged(ty)),
        _ => Err(UnsupportedReason::NonScalarType),
    }
}

fn is_option_class(module: &Module, class: u32) -> bool {
    let core = lm_bytecode::corepin::declared_layout(module);
    if [core.option_some, core.option_none].contains(&Some(class)) {
        return true;
    }
    [core.option_some, core.option_none]
        .into_iter()
        .flatten()
        .filter_map(|arm| module.classes.get(arm as usize))
        .filter_map(lm_bytecode::BcClass::parent)
        .any(|parent| parent == class)
}

fn is_root_kind(kind: ScalarKind) -> bool {
    matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_))
}

pub(super) fn split_segments(func: &Func) -> Result<Vec<Segment>, UnsupportedReason> {
    let mut segments = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut start = 0usize;
        for (instruction_index, instruction) in block.iter().enumerate() {
            let exit = segment_exit(instruction, instruction_index, block.len())?;
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
                call_contract: None,
                exit_stack: Vec::new(),
                boundary_stack: Vec::new(),
                heap_accesses: Vec::new(),
                option_accesses: Vec::new(),
                fuel_stacks: Vec::new(),
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

fn segment_exit(
    instruction: &Instr,
    instruction_index: usize,
    block_len: usize,
) -> Result<Option<SegmentExit>, UnsupportedReason> {
    let next = instruction_index as u32 + 1;
    let treatment = crate::instruction_treatment(instruction);
    if !treatment.is_dedicated() {
        return Ok(Some(SegmentExit::Interpreter {
            fallthrough_ip: (instruction_index + 1 < block_len).then_some(next),
        }));
    }
    Ok(match treatment.exit() {
        crate::ExitBehavior::Continue => None,
        crate::ExitBehavior::Branch => Some(match instruction {
            Instr::Jump(target) => SegmentExit::Jump {
                target_block: *target,
            },
            Instr::JumpIfFalse(target) => SegmentExit::Conditional {
                target_block: *target,
                jump_on_true: false,
                fallthrough_ip: next,
            },
            Instr::JumpIfTrue(target) => SegmentExit::Conditional {
                target_block: *target,
                jump_on_true: true,
                fallthrough_ip: next,
            },
            _ => return Err(UnsupportedReason::InvalidControlFlow),
        }),
        crate::ExitBehavior::Call => Some(match instruction {
            Instr::Call(target) => SegmentExit::Call {
                target: *target,
                app: None,
                fallthrough_ip: next,
            },
            Instr::CallG { func, app } => SegmentExit::Call {
                target: *func,
                app: Some(*app),
                fallthrough_ip: next,
            },
            _ => return Err(UnsupportedReason::InvalidControlFlow),
        }),
        crate::ExitBehavior::Allocation => Some(SegmentExit::Allocation {
            fallthrough_ip: next,
        }),
        crate::ExitBehavior::Effect => Some(SegmentExit::Effect {
            fallthrough_ip: next,
        }),
        crate::ExitBehavior::Return => Some(SegmentExit::Return),
        crate::ExitBehavior::Fault => Some(SegmentExit::Unreachable),
    })
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
            SegmentExit::Interpreter {
                fallthrough_ip: Some(fallthrough_ip),
            } => vec![entry(entries, segment.block, fallthrough_ip)?],
            SegmentExit::Interpreter {
                fallthrough_ip: None,
            } => Vec::new(),
            SegmentExit::Return | SegmentExit::Unreachable => Vec::new(),
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
    verified_points: &HashMap<(u32, u32), VerifiedPoint>,
) -> Result<SegmentAnalysis, UnsupportedReason> {
    let mut max_stack = 0;
    let mut max_stack_values = 0;
    let mut boundary_stack = Vec::new();
    let mut heap_accesses = Vec::new();
    let mut option_accesses = Vec::new();
    let mut fuel_stacks = Vec::new();
    let mut replay_stacks = Vec::new();
    let mut fault_stacks = Vec::new();
    let mut allocations = Vec::new();
    let mut call_contract = None;
    let mut uses = vec![false; context.locals.len()];
    let mut definitions = vec![false; context.locals.len()];
    for (offset, instruction) in context.func.blocks[segment.block as usize]
        [segment.start as usize..segment.end as usize]
        .iter()
        .enumerate()
    {
        let position = segment
            .start
            .checked_add(u32::try_from(offset).map_err(|_| UnsupportedReason::RegionLimit)?)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let next = position
            .checked_add(1)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let before = verified_points
            .get(&(segment.block, position))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        let after = verified_points
            .get(&(segment.block, next))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        fuel_stacks.push((position, before.stack.clone()));

        let treatment = crate::instruction_treatment(instruction);
        if treatment.replays() {
            replay_stacks.push((position, before.stack.clone()));
        }
        match treatment.fault_stack() {
            crate::FaultStack::None => {}
            crate::FaultStack::Before => {
                fault_stacks.push((next, before.stack.clone()));
            }
            crate::FaultStack::Pop(count) => {
                let length = before
                    .stack
                    .len()
                    .checked_sub(usize::from(count))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                fault_stacks.push((next, before.stack[..length].to_vec()));
            }
        }

        let source_instruction = context
            .source_func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(position as usize))
            .copied()
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        match *instruction {
            Instr::LoadLocal(slot) => {
                let at = slot as usize;
                if context.locals.get(at).is_none() {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                if !before.initialized.get(at).copied().unwrap_or(false) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                if !definitions[at] {
                    uses[at] = true;
                }
            }
            Instr::StoreLocal(slot) => {
                let at = slot as usize;
                context
                    .locals
                    .get(at)
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                definitions[at] = true;
            }
            Instr::LoadField(field) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let (receiver_class, value) = field_contract(context, receiver, field)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::LoadField {
                        receiver_class,
                        value,
                    },
                });
            }
            Instr::StoreField(field) => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 1)?;
                let (receiver_class, contract) = field_contract(context, receiver, field)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::StoreField {
                        receiver_class,
                        value: contract,
                    },
                });
            }
            Instr::TupleGet(index) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let value = tuple_element_contract(context, receiver, index)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::TupleGet { value },
                });
            }
            Instr::EqDigest | Instr::NeDigest => {
                let right = stack_from_end(&before.stack, 0)?;
                let left = stack_from_end(&before.stack, 1)?;
                digest_type(context.module, left)?;
                digest_type(context.module, right)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::DigestCompare,
                });
            }
            Instr::Extended(ExtendedInstr::AsCallback) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                function_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::AsCallback,
                });
            }
            Instr::Extended(ExtendedInstr::OptionNone { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionNone { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                option_argument_type(context.module, source_ty)?;
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::None,
                });
            }
            Instr::Extended(ExtendedInstr::OptionPayload { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionPayload { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let payload = option_argument_type(context.module, source_ty)?;
                let value = value_contract(context, payload)?;
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::Payload { value },
                });
            }
            Instr::Extended(ExtendedInstr::ListGet { ty }) => {
                let Instr::Extended(ExtendedInstr::ListGet { ty: source_ty }) = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context.module, receiver)?;
                let option_element = option_argument_type(context.module, source_ty)?;
                let value = value_contract(context, element)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_element)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::ListGet { value },
                });
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let source_ty = match source_instruction {
                    Instr::IsType(ty) | Instr::CastType(ty) => ty,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                if let Some(target) = option_test_target(context.module, source_ty)? {
                    if !matches!(receiver, ScalarKind::Tagged(_)) {
                        return Err(UnsupportedReason::InvalidStack);
                    }
                    let kind = if matches!(instruction, Instr::IsType(_)) {
                        OptionAccessKind::IsType { target }
                    } else {
                        OptionAccessKind::CastType { target }
                    };
                    let family_type = match instruction {
                        Instr::IsType(ty) | Instr::CastType(ty) => ty,
                        _ => return Err(UnsupportedReason::InvalidControlFlow),
                    };
                    option_accesses.push(OptionAccess {
                        instruction: position,
                        family_type: *family_type,
                        kind,
                    });
                } else {
                    if !matches!(receiver, ScalarKind::Object(_)) {
                        return Err(UnsupportedReason::InvalidStack);
                    }
                    let target_class = class_test_target(context, source_ty)?;
                    let kind = if matches!(instruction, Instr::IsType(_)) {
                        HeapAccessKind::IsType { target_class }
                    } else {
                        HeapAccessKind::CastType { target_class }
                    };
                    heap_accesses.push(HeapAccess {
                        instruction: position,
                        kind,
                    });
                }
            }
            Instr::ListLen => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListLen,
                });
            }
            Instr::MapLen => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapLen,
                });
            }
            Instr::ListAt => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context.module, receiver)?;
                let value = value_contract(context, element)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListAt { value },
                });
            }
            Instr::Extended(ExtendedInstr::ListSet) => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 2)?;
                let element = list_element_type(context.module, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListSet { value: contract },
                });
            }
            Instr::Extended(ExtendedInstr::ListCapacity) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListCapacity,
                });
            }
            Instr::Extended(ExtendedInstr::ListReserve) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListReserve,
                });
            }
            Instr::Extended(ExtendedInstr::ListReorder) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListReorder,
                });
            }
            Instr::Extended(ExtendedInstr::ListEpoch) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListEpoch,
                });
            }
            Instr::Extended(ExtendedInstr::ListIterLen) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListIterLen,
                });
            }
            Instr::Extended(ExtendedInstr::MapEpoch) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapEpoch,
                });
            }
            Instr::Extended(ExtendedInstr::MapIterLen) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                map_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapIterLen,
                });
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let ScalarKind::Object(ty) = receiver else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                let contract = value_contract(context, ty)?;
                let Some(ObjectContract::Instance(class)) = contract.object else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::SealInstance { class },
                });
            }
            Instr::Native(NativeInstr::BytesLen) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesLen,
                });
            }
            Instr::Native(NativeInstr::BytesAt) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesAt,
                });
            }
            Instr::Native(NativeInstr::BytesGet) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesGet,
                });
            }
            Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                text_type(context, receiver)?;
                let kind = if matches!(*instruction, Instr::Native(NativeInstr::StrByteLen)) {
                    HeapAccessKind::TextByteLen
                } else {
                    HeapAccessKind::TextScalarLen
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind,
                });
            }
            Instr::Native(
                NativeInstr::TextAtByte | NativeInstr::TextAt | NativeInstr::TextIsBoundary,
            ) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                text_type(context, receiver)?;
                let kind = match instruction {
                    Instr::Native(NativeInstr::TextAtByte) => HeapAccessKind::TextAtByte,
                    Instr::Native(NativeInstr::TextAt) => HeapAccessKind::TextAt,
                    Instr::Native(NativeInstr::TextIsBoundary) => HeapAccessKind::TextIsBoundary,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind,
                });
            }
            Instr::ListPush => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context.module, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListPush { value: contract },
                });
            }
            Instr::New(_) | Instr::NewG { .. } => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Call(target) => {
                let signature = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                let contract = instantiate_call(signature, context.module, None)?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                if !uses_equal_representation(result, contract.result) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                boundary_stack = before.stack.clone();
                call_contract = Some(contract);
            }
            Instr::CallG { func: target, .. } => {
                let Instr::CallG { app, .. } = source_instruction else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let signature = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                let contract = instantiate_call(signature, context.module, Some(app))?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                if !uses_equal_representation(result, contract.result) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                boundary_stack = before.stack.clone();
                call_contract = Some(contract);
            }
            Instr::Perform { .. } | Instr::PerformValue { .. } => {
                boundary_stack = before.stack.clone();
            }
            _ if matches!(segment.exit, SegmentExit::Interpreter { .. })
                && offset + 1 == (segment.end - segment.start) as usize =>
            {
                boundary_stack = before.stack.clone();
            }
            _ => {}
        }
        max_stack = max_stack.max(before.stack.len()).max(after.stack.len());
        max_stack_values = max_stack_values
            .max(before.stack.len())
            .max(after.stack.len());
    }
    let exit_stack = verified_points
        .get(&(segment.block, segment.end))
        .ok_or(UnsupportedReason::InvalidControlFlow)?
        .stack
        .clone();
    Ok(SegmentAnalysis {
        uses,
        definitions,
        exit_stack,
        max_stack,
        max_stack_values,
        boundary_stack,
        heap_accesses,
        option_accesses,
        fuel_stacks,
        replay_stacks,
        fault_stacks,
        allocations,
        call_contract,
    })
}

fn stack_from_end(stack: &[ScalarKind], offset: usize) -> Result<ScalarKind, UnsupportedReason> {
    let index = offset
        .checked_add(1)
        .and_then(|count| stack.len().checked_sub(count))
        .ok_or(UnsupportedReason::InvalidStack)?;
    stack
        .get(index)
        .copied()
        .ok_or(UnsupportedReason::InvalidStack)
}

fn field_contract(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
    field: u32,
) -> Result<(u32, ValueContract), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let Some(BcType::Class(class)) = context.module.types.get(ty as usize) else {
        return Err(UnsupportedReason::NonScalarType);
    };
    let field_type = context
        .module
        .classes
        .get(*class as usize)
        .and_then(|class| class.fields.get(field as usize))
        .map(|(_, ty)| *ty)
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    Ok((
        relocate_class(*class, context.class_relocation)?,
        value_contract(context, field_type)?,
    ))
}

fn tuple_element_contract(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
    index: u32,
) -> Result<ValueContract, UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let Some(BcType::Tuple(elements)) = context.module.types.get(ty as usize) else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let element = elements
        .get(index as usize)
        .copied()
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    value_contract(context, element)
}

fn list_element_type(module: &Module, receiver: ScalarKind) -> Result<u32, UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match module.types.get(ty as usize) {
        Some(BcType::List(element)) => Ok(*element),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn map_type(module: &Module, receiver: ScalarKind) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match module.types.get(ty as usize) {
        Some(BcType::Map(_, _)) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn digest_type(module: &Module, receiver: ScalarKind) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match module.types.get(ty as usize) {
        Some(BcType::Digest) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn function_type(module: &Module, receiver: ScalarKind) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match module.types.get(ty as usize) {
        Some(BcType::Fn(_, _, _, _)) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn option_argument_type(module: &Module, ty: u32) -> Result<u32, UnsupportedReason> {
    let Some(BcType::Inst(class, arguments)) = module.types.get(ty as usize) else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let core = lm_bytecode::corepin::declared_layout(module);
    if ![core.option, core.option_some, core.option_none].contains(&Some(*class))
        || arguments.len() != 1
    {
        return Err(UnsupportedReason::InvalidStack);
    }
    Ok(arguments[0])
}

fn bytes_type(module: &Module, receiver: ScalarKind) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match module.types.get(ty as usize) {
        Some(BcType::Bytes) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn text_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let contract = value_contract(context, ty)?;
    match contract.object {
        Some(ObjectContract::Str | ObjectContract::Text) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

fn class_test_target(
    context: &SegmentAnalysisContext<'_>,
    ty: u32,
) -> Result<u32, UnsupportedReason> {
    let class = match context.module.types.get(ty as usize) {
        Some(BcType::Class(class) | BcType::Inst(class, _)) => *class,
        _ => return Err(UnsupportedReason::InvalidStack),
    };
    relocate_class(class, context.class_relocation)
}

fn option_test_target(module: &Module, ty: u32) -> Result<Option<OptionTarget>, UnsupportedReason> {
    let class = match module.types.get(ty as usize) {
        Some(BcType::Class(class) | BcType::Inst(class, _)) => *class,
        _ => return Err(UnsupportedReason::InvalidStack),
    };
    let core = lm_bytecode::corepin::declared_layout(module);
    Ok(if core.option == Some(class) {
        Some(OptionTarget::Family)
    } else if core.option_some == Some(class) {
        Some(OptionTarget::Some)
    } else if core.option_none == Some(class) {
        Some(OptionTarget::None)
    } else {
        None
    })
}

fn value_contract(
    context: &SegmentAnalysisContext<'_>,
    ty: u32,
) -> Result<ValueContract, UnsupportedReason> {
    let kind = scalar_kind(context.module, ty)?;
    let core = lm_bytecode::corepin::declared_layout(context.module);
    let object = match context.module.types.get(ty as usize) {
        Some(BcType::Str) => Some(ObjectContract::Str),
        Some(BcType::Class(class) | BcType::Inst(class, _))
            if [core.text, core.substring].contains(&Some(*class)) =>
        {
            Some(ObjectContract::Text)
        }
        Some(BcType::Class(class) | BcType::Inst(class, _))
            if matches!(kind, ScalarKind::Object(_)) =>
        {
            Some(ObjectContract::Instance(relocate_class(
                *class,
                context.class_relocation,
            )?))
        }
        Some(BcType::List(_)) => Some(ObjectContract::List),
        Some(BcType::Map(_, _)) => Some(ObjectContract::Map),
        Some(BcType::Tuple(_)) => Some(ObjectContract::Tuple),
        Some(BcType::Fn(_, _, _, _) | BcType::Callback(_, _, _, _)) => {
            Some(ObjectContract::Closure)
        }
        Some(BcType::Bytes) => Some(ObjectContract::Bytes),
        Some(BcType::Digest) => Some(ObjectContract::Digest),
        _ => None,
    };
    Ok(ValueContract { kind, object })
}

fn relocate_class(class: u32, relocation: Option<&[u32]>) -> Result<u32, UnsupportedReason> {
    match relocation {
        Some(classes) => classes
            .get(class as usize)
            .copied()
            .ok_or(UnsupportedReason::MissingSource),
        None => Ok(class),
    }
}

fn stacks_use_equal_representations(left: &[ScalarKind], right: &[ScalarKind]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .copied()
            .zip(right.iter().copied())
            .all(|(left, right)| uses_equal_representation(left, right))
}

fn uses_equal_representation(left: ScalarKind, right: ScalarKind) -> bool {
    left == right
        || matches!(
            (left, right),
            (ScalarKind::Object(_), ScalarKind::Object(_))
                | (ScalarKind::Tagged(_), ScalarKind::Tagged(_))
        )
}

pub(super) fn compute_liveness(segments: &mut [Segment], locals: usize) {
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
