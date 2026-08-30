//! Verified bytecode analysis and immutable native region plans.

use crate::{Failure, MAX_REGION_INSTRUCTIONS, MAX_REGION_LOCALS, MAX_REGION_STACK};
use lm_bytecode::{BcType, ExtendedInstr, Func, Instr, Module, NativeInstr, NumericInstr};
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

    pub(super) fn definition(&self, function: u32) -> Option<FunctionDefinition<'a>> {
        if function == self.root.function {
            Some(self.root)
        } else {
            self.direct_callees
                .iter()
                .find(|definition| definition.function == function)
                .copied()
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
    ListCapacity,
    ListEpoch,
    ListIterLen,
    SealInstance {
        class: u32,
    },
    BytesLen,
    BytesAt,
    BytesGet,
    TextByteLen,
    TextScalarLen,
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
pub(super) struct InlineFunctionPlan {
    pub(super) params: Vec<ScalarKind>,
    pub(super) local_kinds: Vec<ScalarKind>,
    pub(super) max_stack: usize,
    pub(super) cost: u32,
    pub(super) allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone)]
pub(super) struct CallContract {
    pub(super) params: Vec<ScalarKind>,
    pub(super) local_count: usize,
    pub(super) result: ScalarKind,
    pub(super) inline: Option<InlineFunctionPlan>,
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
    inline: Option<InlineFunctionPlan>,
}

#[derive(Debug, Clone)]
pub(super) struct RegionPlan {
    pub(super) local_kinds: Vec<ScalarKind>,
    pub(super) result_kind: ScalarKind,
    pub(super) max_stack: usize,
    pub(super) max_stack_values: usize,
    pub(super) max_roots: usize,
    pub(super) additional_frames: u32,
    pub(super) segments: Vec<Segment>,
    pub(super) entries: std::collections::HashMap<(u32, u32), usize>,
    pub(super) resume_entries: std::collections::HashMap<(u32, u32), u32>,
    pub(super) resume_targets: Vec<ResumeTarget>,
    pub(super) inline_functions: HashMap<u32, InlineFunctionPlan>,
    pub(super) call_sites: usize,
    pub(super) heap_read_sites: usize,
    pub(super) heap_write_sites: usize,
    pub(super) allocation_sites: usize,
    pub(super) effect_sites: usize,
    pub(super) interpreter_sites: usize,
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

struct SegmentAnalysisContext<'a> {
    func: &'a Func,
    source_func: &'a Func,
    module: &'a Module,
    locals: &'a [ScalarKind],
    result: ScalarKind,
    calls: &'a HashMap<u32, CallSignature>,
    class_relocation: Option<&'a [u32]>,
}

#[derive(Debug, Clone)]
pub(super) struct AllocationSite {
    pub(super) instruction: u32,
    pub(super) initialized: Vec<bool>,
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
        let segment_points: Vec<(u32, u32)> = segments
            .iter()
            .map(|segment| (segment.block, segment.start))
            .collect();
        let interpreter_points: Vec<(u32, u32)> = segments
            .iter()
            .filter(|segment| matches!(segment.exit, SegmentExit::Interpreter { .. }))
            .map(|segment| (segment.block, segment.end))
            .collect();
        let mut requested_points = segment_points.clone();
        requested_points.extend(interpreter_points.iter().copied());
        let metadata = lm_verify::verify_function_metadata_at_with_bundle(
            source,
            input.root.bundle,
            input.root.source_function,
            &requested_points,
        )
        .map_err(|_| UnsupportedReason::MissingSource)?;
        let (segment_states, interpreter_states) = metadata.points().split_at(segments.len());
        let interpreter_stacks: HashMap<(u32, u32), Vec<ScalarKind>> = interpreter_points
            .into_iter()
            .zip(interpreter_states.iter())
            .map(|(point, state)| {
                let state = state
                    .as_ref()
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                let stack = state
                    .stack()
                    .iter()
                    .map(|ty| scalar_kind_in(source, metadata.types(), *ty))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((point, stack))
            })
            .collect::<Result<_, _>>()?;
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
        let entries: std::collections::HashMap<(u32, u32), usize> = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| ((segment.block, segment.start), index))
            .collect();
        resolve_successors(&mut segments, &entries)?;
        let mut max_stack = 0;
        let mut max_stack_values = 0;
        let mut additional_frames = 0;
        let mut call_sites = 0;
        let mut heap_read_sites = 0;
        let mut heap_write_sites = 0;
        let mut allocation_sites = 0;
        let mut effect_sites = 0;
        let mut interpreter_sites = 0;
        let analysis_context = SegmentAnalysisContext {
            func: runtime,
            source_func,
            module: source,
            locals: &local_kinds,
            result: result_kind,
            calls: &call_contracts,
            class_relocation: input.root.class_relocation,
        };
        for (index, segment) in segments.iter_mut().enumerate() {
            let state = segment_states
                .get(index)
                .and_then(Option::as_ref)
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            let entry_stack = state
                .stack()
                .iter()
                .map(|ty| scalar_kind_in(source, metadata.types(), *ty))
                .collect::<Result<Vec<_>, _>>()?;
            segment.entry_stack = entry_stack.clone();
            let initialized: Vec<bool> = state.locals().iter().map(Option::is_some).collect();
            let interpreter_stack = interpreter_stacks.get(&(segment.block, segment.end));
            let analysis = analyze_segment(
                &analysis_context,
                segment,
                &initialized,
                &entry_stack,
                interpreter_stack.map(Vec::as_slice),
            )?;
            segment.uses = analysis.uses;
            segment.definitions = analysis.definitions;
            segment.call_contract = analysis.call_contract;
            segment.exit_stack = analysis.exit_stack.clone();
            segment.boundary_stack = analysis.boundary_stack;
            segment.heap_accesses = analysis.heap_accesses;
            segment.option_accesses = analysis.option_accesses;
            segment.fuel_stacks = analysis.fuel_stacks;
            for access in &segment.heap_accesses {
                match access.kind {
                    HeapAccessKind::StoreField { .. }
                    | HeapAccessKind::ListSet { .. }
                    | HeapAccessKind::ListEpoch
                    | HeapAccessKind::SealInstance { .. } => {
                        heap_write_sites += 1;
                    }
                    _ => heap_read_sites += 1,
                }
            }
            segment.replay_stacks = analysis.replay_stacks;
            segment.fault_stacks = analysis.fault_stacks;
            segment.allocations = analysis.allocations;
            allocation_sites += segment.allocations.len();
            segment.cost = segment.end - segment.start;
            if let SegmentExit::Call { target, .. } = segment.exit {
                call_sites += 1;
                if let Some(inline) = inline_functions.get(&target) {
                    segment.cost = segment
                        .cost
                        .checked_add(inline.cost)
                        .ok_or(UnsupportedReason::RegionLimit)?;
                    additional_frames = 1;
                } else {
                    segment.cost = segment.cost.saturating_sub(1);
                }
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
        let inline_root_count = inline_functions
            .values()
            .map(|inline| {
                inline
                    .local_kinds
                    .iter()
                    .filter(|kind| is_root_kind(**kind))
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
        let mut resume_entries = std::collections::HashMap::new();
        let mut resume_targets = Vec::new();
        for (segment_index, segment) in segments.iter().enumerate() {
            let has_inline_call = matches!(
                segment.exit,
                SegmentExit::Call { target, .. } if inline_functions.contains_key(&target)
            );
            if has_inline_call {
                continue;
            }
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
            additional_frames,
            segments,
            entries,
            resume_entries,
            resume_targets,
            inline_functions,
            call_sites,
            heap_read_sites,
            heap_write_sites,
            allocation_sites,
            effect_sites,
            interpreter_sites,
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
        let concrete_params = params
            .iter()
            .copied()
            .map(CallValueKind::concrete)
            .collect::<Option<Vec<_>>>();
        let concrete_result = result.concrete();
        let inline = concrete_params
            .as_deref()
            .zip(concrete_result)
            .and_then(|(params, result)| inline_function_plan(definition, params, result));
        contracts.insert(
            definition.function,
            CallSignature {
                params,
                local_count: source_func.local_types.len(),
                result,
                inline,
            },
        );
    }
    Ok(contracts)
}

impl CallValueKind {
    fn concrete(self) -> Option<ScalarKind> {
        match self {
            CallValueKind::Fixed(kind) => Some(kind),
            CallValueKind::Variable(_) => None,
        }
    }
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
        inline: signature.inline.clone(),
    })
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
                    | Instr::LoadField(_)
                    | Instr::StoreField(_)
                    | Instr::TupleGet(_)
                    | Instr::ListLen
                    | Instr::ListAt
                    | Instr::Extended(ExtendedInstr::ListSet)
                    | Instr::Extended(ExtendedInstr::ListCapacity)
                    | Instr::Extended(ExtendedInstr::ListEpoch)
                    | Instr::Extended(ExtendedInstr::ListIterLen)
                    | Instr::Extended(ExtendedInstr::SealInstance)
                    | Instr::Native(
                        NativeInstr::BytesLen
                            | NativeInstr::BytesAt
                            | NativeInstr::BytesGet
                            | NativeInstr::StrByteLen
                            | NativeInstr::StrCharCount,
                    )
                    | Instr::TupleNew { .. }
                    | Instr::ListNew { .. }
                    | Instr::ListPush
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
        call_contract: None,
        exit_stack: Vec::new(),
        boundary_stack: Vec::new(),
        heap_accesses: Vec::new(),
        option_accesses: Vec::new(),
        fuel_stacks: Vec::new(),
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
        class_relocation: definition.class_relocation,
    };
    let analysis = analyze_segment(&context, &segment, &initialized, &[], None).ok()?;
    Some(InlineFunctionPlan {
        params: params.to_vec(),
        local_kinds,
        max_stack: analysis.max_stack,
        cost: code.len() as u32,
        allocations: analysis.allocations,
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
        Some(BcType::Str | BcType::Map(_, _) | BcType::Fn(_, _, _, _)) => {
            Ok(ScalarKind::Object(ty))
        }
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
                    app: None,
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::CallG { func, app } => Some(SegmentExit::Call {
                    target: *func,
                    app: Some(*app),
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::New(_) => Some(SegmentExit::Allocation {
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::Perform { .. } | Instr::PerformValue { .. } => Some(SegmentExit::Effect {
                    fallthrough_ip: instruction_index as u32 + 1,
                }),
                Instr::TupleNew { .. } | Instr::ListNew { .. } | Instr::ListPush => {
                    Some(SegmentExit::Interpreter {
                        fallthrough_ip: Some(instruction_index as u32 + 1),
                    })
                }
                Instr::Return => Some(SegmentExit::Return),
                Instr::Unreachable => Some(SegmentExit::Unreachable),
                _ if !crate::instruction_has_dedicated_treatment(instruction) => {
                    Some(SegmentExit::Interpreter {
                        fallthrough_ip: (instruction_index + 1 < block.len())
                            .then_some(instruction_index as u32 + 1),
                    })
                }
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
    initialized: &[bool],
    entry_stack: &[ScalarKind],
    interpreter_exit_stack: Option<&[ScalarKind]>,
) -> Result<SegmentAnalysis, UnsupportedReason> {
    let mut stack = entry_stack.to_vec();
    let mut max_stack = stack.len();
    let mut max_stack_values = stack.len();
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
        fuel_stacks.push((segment.start + offset as u32, stack.clone()));
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
            Instr::ConstChar(_) => stack.push(ScalarKind::Char),
            Instr::ConstStr(_) => stack.push(ScalarKind::Object(lm_verify::TY_STR)),
            Instr::ConstBytes(_) => {
                let ty = context
                    .module
                    .types
                    .iter()
                    .position(|ty| matches!(ty, BcType::Bytes))
                    .and_then(|index| u32::try_from(index).ok())
                    .ok_or(UnsupportedReason::InvalidStack)?;
                stack.push(ScalarKind::Object(ty));
            }
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
                let (receiver_class, value) = field_contract(context, receiver, field)?;
                let kind = value.kind;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::LoadField {
                        receiver_class,
                        value,
                    },
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(kind);
            }
            Instr::StoreField(field) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let (receiver_class, contract) = field_contract(context, receiver, field)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::StoreField {
                        receiver_class,
                        value: contract,
                    },
                });
                fault_stacks.push((instruction + 1, stack.clone()));
            }
            Instr::TupleGet(index) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let value = tuple_element_contract(context, receiver, index)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::TupleGet { value },
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(value.kind);
            }
            Instr::Extended(ExtendedInstr::OptionSome { .. }) => {
                let Instr::Extended(ExtendedInstr::OptionSome { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let payload = option_argument_type(context.module, source_ty)?;
                expect(&mut stack, scalar_kind(context.module, payload)?)?;
                stack.push(ScalarKind::Tagged(source_ty));
            }
            Instr::Extended(ExtendedInstr::OptionNone { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionNone { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                option_argument_type(context.module, source_ty)?;
                option_accesses.push(OptionAccess {
                    instruction: segment.start + offset as u32,
                    family_type: ty,
                    kind: OptionAccessKind::None,
                });
                stack.push(ScalarKind::Tagged(source_ty));
            }
            Instr::Extended(ExtendedInstr::OptionPayload { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionPayload { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let instruction = segment.start + offset as u32;
                let replay_stack = stack.clone();
                expect(&mut stack, ScalarKind::Tagged(source_ty))?;
                let payload = option_argument_type(context.module, source_ty)?;
                let value = value_contract(context, payload)?;
                option_accesses.push(OptionAccess {
                    instruction,
                    family_type: ty,
                    kind: OptionAccessKind::Payload { value },
                });
                replay_stacks.push((instruction, replay_stack.clone()));
                fault_stacks.push((instruction + 1, replay_stack));
                stack.push(value.kind);
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let instruction_index = segment.start + offset as u32;
                replay_stacks.push((instruction_index, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
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
                        instruction: instruction_index,
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
                        instruction: instruction_index,
                        kind,
                    });
                }
                if matches!(instruction, Instr::IsType(_)) {
                    stack.push(ScalarKind::Bool);
                } else {
                    stack.push(scalar_kind(context.module, source_ty)?);
                }
            }
            Instr::ListLen => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListLen,
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(ScalarKind::Int);
            }
            Instr::ListAt => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                expect(&mut stack, ScalarKind::Int)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let element = list_element_type(context.module, receiver)?;
                let value = value_contract(context, element)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListAt { value },
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(value.kind);
            }
            Instr::Extended(ExtendedInstr::ListSet) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                expect(&mut stack, ScalarKind::Int)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let element = list_element_type(context.module, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListSet { value: contract },
                });
                fault_stacks.push((instruction + 1, stack.clone()));
                stack.push(ScalarKind::Unit);
            }
            Instr::Extended(ExtendedInstr::ListCapacity) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListCapacity,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Extended(ExtendedInstr::ListEpoch) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListEpoch,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Extended(ExtendedInstr::ListIterLen) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                expect(&mut stack, ScalarKind::Int)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                list_element_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::ListIterLen,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let ScalarKind::Object(ty) = receiver else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                let contract = value_contract(context, ty)?;
                let Some(ObjectContract::Instance(class)) = contract.object else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::SealInstance { class },
                });
                stack.push(receiver);
            }
            Instr::Native(NativeInstr::BytesLen) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::BytesLen,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Native(NativeInstr::BytesAt) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                expect(&mut stack, ScalarKind::Int)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::BytesAt,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Native(NativeInstr::BytesGet) => {
                let instruction = segment.start + offset as u32;
                replay_stacks.push((instruction, stack.clone()));
                expect(&mut stack, ScalarKind::Int)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                bytes_type(context.module, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction,
                    kind: HeapAccessKind::BytesGet,
                });
                stack.push(ScalarKind::Int);
            }
            Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                let position = segment.start + offset as u32;
                replay_stacks.push((position, stack.clone()));
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
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
                stack.push(ScalarKind::Int);
            }
            Instr::TupleNew { count, .. } => {
                let Instr::TupleNew {
                    ty: source_ty,
                    count: source_count,
                } = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                if count != source_count {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                let Some(BcType::Tuple(elements)) = context.module.types.get(source_ty as usize)
                else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                if elements.len() != count as usize {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                boundary_stack = stack.clone();
                for element in elements.iter().rev().copied() {
                    expect(&mut stack, scalar_kind(context.module, element)?)?;
                }
                stack.push(ScalarKind::Object(source_ty));
            }
            Instr::ListNew { count, .. } => {
                let Instr::ListNew {
                    ty: source_ty,
                    count: source_count,
                } = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                if count != source_count {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                let element = match context.module.types.get(source_ty as usize) {
                    Some(BcType::List(element)) => *element,
                    _ => return Err(UnsupportedReason::InvalidStack),
                };
                let element = scalar_kind(context.module, element)?;
                boundary_stack = stack.clone();
                for _ in 0..count {
                    expect(&mut stack, element)?;
                }
                stack.push(ScalarKind::Object(source_ty));
            }
            Instr::ListPush => {
                boundary_stack = stack.clone();
                let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let receiver = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let element = list_element_type(context.module, receiver)?;
                if value != scalar_kind(context.module, element)? {
                    return Err(UnsupportedReason::InvalidStack);
                }
                stack.push(ScalarKind::Unit);
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
            Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
                expect(&mut stack, ScalarKind::Int)?;
                expect(&mut stack, ScalarKind::Int)?;
                stack.push(ScalarKind::Int);
            }
            Instr::EqRef | Instr::NeRef => {
                let right = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                let left = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                if !matches!(left, ScalarKind::Object(_)) || !matches!(right, ScalarKind::Object(_))
                {
                    return Err(UnsupportedReason::InvalidStack);
                }
                stack.push(ScalarKind::Bool);
            }
            Instr::Native(operation) if char_operation(operation, &mut stack)? => {}
            Instr::Call(target) => {
                let signature = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                let contract = instantiate_call(signature, context.module, None)?;
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
                boundary_stack = stack.clone();
                for parameter in contract.params.iter().rev().copied() {
                    expect(&mut stack, parameter)?;
                }
                stack.push(contract.result);
                call_contract = Some(contract);
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
            Instr::Numeric(operation) if numeric_operation_replays(operation) => {
                replay_stacks.push((segment.start + offset as u32, stack.clone()));
                if !scalar_numeric_operation(operation, &mut stack)? {
                    return Err(UnsupportedReason::UnsupportedInstruction);
                }
            }
            Instr::Numeric(operation) if scalar_numeric_operation(operation, &mut stack)? => {}
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
            Instr::Unreachable => {
                fault_stacks.push((segment.start + offset as u32 + 1, stack.clone()));
            }
            _ if matches!(segment.exit, SegmentExit::Interpreter { .. })
                && offset + 1 == (segment.end - segment.start) as usize =>
            {
                boundary_stack = stack.clone();
                stack = interpreter_exit_stack
                    .ok_or(UnsupportedReason::InvalidControlFlow)?
                    .to_vec();
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
        heap_accesses,
        option_accesses,
        fuel_stacks,
        replay_stacks,
        fault_stacks,
        allocations,
        call_contract,
    })
}

fn numeric_operation_replays(operation: NumericInstr) -> bool {
    matches!(
        operation,
        NumericInstr::IntShl
            | NumericInstr::IntShr
            | NumericInstr::IntUshr
            | NumericInstr::IntRotateLeft
            | NumericInstr::IntRotateRight
            | NumericInstr::FloatToIntValue
    )
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
        Some(BcType::Fn(_, _, _, _)) => Some(ObjectContract::Closure),
        Some(BcType::Bytes) => Some(ObjectContract::Bytes),
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

fn expect(stack: &mut Vec<ScalarKind>, expected: ScalarKind) -> Result<(), UnsupportedReason> {
    match stack.pop() {
        Some(found) if uses_equal_representation(found, expected) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
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

fn scalar_numeric_operation(
    operation: NumericInstr,
    stack: &mut Vec<ScalarKind>,
) -> Result<bool, UnsupportedReason> {
    match operation {
        NumericInstr::IntBitAnd
        | NumericInstr::IntBitOr
        | NumericInstr::IntBitXor
        | NumericInstr::IntShl
        | NumericInstr::IntShr
        | NumericInstr::IntUshr
        | NumericInstr::IntWrappingAdd
        | NumericInstr::IntWrappingSub
        | NumericInstr::IntWrappingMul
        | NumericInstr::IntRotateLeft
        | NumericInstr::IntRotateRight => {
            expect(stack, ScalarKind::Int)?;
            expect(stack, ScalarKind::Int)?;
            stack.push(ScalarKind::Int);
        }
        NumericInstr::IntBitNot => {
            expect(stack, ScalarKind::Int)?;
            stack.push(ScalarKind::Int);
        }
        NumericInstr::IntToFloat => {
            expect(stack, ScalarKind::Int)?;
            stack.push(ScalarKind::Float);
        }
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
        NumericInstr::FloatIsNan => {
            expect(stack, ScalarKind::Float)?;
            stack.push(ScalarKind::Bool);
        }
        NumericInstr::FloatHash
        | NumericInstr::FloatBits
        | NumericInstr::FloatToIntStatus
        | NumericInstr::FloatToIntValue => {
            expect(stack, ScalarKind::Float)?;
            stack.push(ScalarKind::Int);
        }
        NumericInstr::FloatFromBits => {
            expect(stack, ScalarKind::Int)?;
            stack.push(ScalarKind::Float);
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn char_operation(
    operation: NativeInstr,
    stack: &mut Vec<ScalarKind>,
) -> Result<bool, UnsupportedReason> {
    match operation {
        NativeInstr::CharCodepoint | NativeInstr::CharUtf8Len => {
            expect(stack, ScalarKind::Char)?;
            stack.push(ScalarKind::Int);
        }
        NativeInstr::EqChar
        | NativeInstr::NeChar
        | NativeInstr::LtChar
        | NativeInstr::LeChar
        | NativeInstr::GtChar
        | NativeInstr::GeChar => {
            expect(stack, ScalarKind::Char)?;
            expect(stack, ScalarKind::Char)?;
            stack.push(ScalarKind::Bool);
        }
        _ => return Ok(false),
    }
    Ok(true)
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
