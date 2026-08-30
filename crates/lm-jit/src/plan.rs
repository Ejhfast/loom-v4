//! Verified bytecode analysis and immutable native region plans.

use crate::{Failure, MAX_REGION_INSTRUCTIONS, MAX_REGION_LOCALS, MAX_REGION_STACK};
use lm_bytecode::{BcType, Func, Instr, Module, NumericInstr};
use std::collections::HashMap;
use std::sync::Arc;

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
        }
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
}

/// One validated native exit record.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionExit {
    pub(super) retired: u64,
    pub(super) kind: ExitKind,
    pub(super) block: u32,
    pub(super) instruction: u32,
    pub(super) stack_len: u32,
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
    GenericFunction,
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
    pub(super) exit_stack: Vec<ScalarKind>,
    pub(super) boundary_stack: Vec<ScalarKind>,
    pub(super) field_results: Vec<FieldResult>,
    pub(super) fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) replay_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) fault_stacks: Vec<(u32, Vec<ScalarKind>)>,
    pub(super) allocations: Vec<AllocationSite>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct FieldResult {
    pub(super) instruction: u32,
    pub(super) receiver_class: u32,
    pub(super) kind: ScalarKind,
    pub(super) result_class: Option<u32>,
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
    pub(super) call_contracts: HashMap<u32, CallContract>,
    pub(super) inline_functions: HashMap<u32, InlineFunctionPlan>,
    pub(super) call_sites: usize,
    pub(super) heap_read_sites: usize,
    pub(super) allocation_sites: usize,
    pub(super) effect_sites: usize,
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
    field_results: Vec<FieldResult>,
    fuel_stacks: Vec<(u32, Vec<ScalarKind>)>,
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
        let mut call_sites = 0;
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
            class_relocation: input.root.class_relocation,
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
            segment.fuel_stacks = analysis.fuel_stacks;
            heap_read_sites += segment.field_results.len();
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
            call_contracts,
            inline_functions,
            call_sites,
            heap_read_sites,
            allocation_sites,
            effect_sites,
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
                local_count: source_func.local_types.len(),
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
    let mut fuel_stacks = Vec::new();
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
                let field = field_result(context, receiver, field, instruction)?;
                let kind = field.kind;
                field_results.push(field);
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
        fuel_stacks,
        replay_stacks,
        fault_stacks,
        allocations,
    })
}

fn field_result(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
    field: u32,
    instruction: u32,
) -> Result<FieldResult, UnsupportedReason> {
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
    let kind = scalar_kind(context.module, field_type)?;
    let result_class = match kind {
        ScalarKind::Object(result_ty) => {
            let Some(BcType::Class(result_class)) = context.module.types.get(result_ty as usize)
            else {
                return Err(UnsupportedReason::NonScalarType);
            };
            Some(relocate_class(*result_class, context.class_relocation)?)
        }
        _ => None,
    };
    Ok(FieldResult {
        instruction,
        receiver_class: relocate_class(*class, context.class_relocation)?,
        kind,
        result_class,
    })
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
