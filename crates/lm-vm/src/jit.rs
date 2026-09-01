//! Canonical machine-state adapter for native execution.

use crate::engine::EngineTurnMetrics;
use crate::machine::{ExecError, ExecOutcome, Frame, FrameCapture, ImageSlotTarget, Machine};
use crate::NamespaceRuntime;
use lm_heap::{Object, ValueArray};
use lm_jit::{
    ExitKind, Failure, FunctionInput, NativeExecution, NativePreparation, ScalarKind,
    LOCAL_INITIALIZED,
};
use lm_value::{ObjRef, TypeEnvId, Value, ValueTag};
use std::sync::{Arc, Mutex, Weak};

/// Reusable scalar buffers for one engine turn.
#[derive(Default)]
pub(crate) struct NativeScratch {
    activation: lm_jit::NativeActivation,
    roots: Vec<u64>,
    root_tags: Vec<u64>,
    root_states: Vec<u8>,
    continuation_regions: Vec<Arc<lm_jit::CompiledRegion>>,
    image_slots: Vec<lm_jit::NativeImageSlot>,
}

/// Mutable runtime data used during one native entry attempt.
pub(crate) struct NativeExecutionContext<'a> {
    pub(crate) module: &'a NamespaceRuntime,
    pub(crate) envs: &'a mut lm_bytecode::closed::TypeEnvs,
    pub(crate) slots: Option<&'a [ImageSlotTarget]>,
    pub(crate) profile: bool,
    pub(crate) instruction_limit: u32,
    pub(crate) poll: lm_jit::NativePoll<'a>,
}

/// One native activation retained at an ordinary scheduler quantum.
pub(crate) struct NativeContinuation {
    scratch: NativeScratch,
    canonical: CanonicalStack,
    root_frame: usize,
    root_local: usize,
    root_operand: usize,
    exit: lm_jit::ExecutionExit,
    effect: Option<NativeEffectContinuation>,
}

#[derive(Debug, Clone, Copy)]
struct NativeEffectContinuation {
    reply_ty: u32,
    environment: TypeEnvId,
    consumed: usize,
    block: u32,
    instruction: u32,
    reply_kind: ScalarKind,
}

#[derive(Default)]
struct CanonicalStack {
    frames: Vec<Frame>,
    locals: Vec<Value>,
    operands: Vec<Value>,
}

fn frame_capture_parts(
    machine: &Machine,
    capture: Option<FrameCapture>,
) -> Option<(u64, u64, usize, usize)> {
    let Some(capture) = capture else {
        return Some((ValueTag::Uninit as u64, 0, 0, 0));
    };
    let value = capture.value();
    let (data, len) = match capture {
        FrameCapture::Closure(reference) => {
            let Object::Closure { captures, .. } = machine.vm.heap.get(reference) else {
                return None;
            };
            (captures.as_ptr() as usize, captures.len())
        }
        FrameCapture::Callback(reference) => {
            let descriptor = machine.callback(reference).ok()?;
            (
                descriptor.captures.as_ptr() as usize,
                descriptor.captures.len(),
            )
        }
    };
    Some((value.tag() as u64, runtime::value_bits(value)?, data, len))
}

fn parts_frame_capture(tag: u64, bits: u64) -> Option<Option<FrameCapture>> {
    if tag == ValueTag::Uninit as u64 {
        return (bits == 0).then_some(None);
    }
    runtime::tagged_value(tag, bits)
        .and_then(FrameCapture::from_value)
        .map(Some)
}

impl CanonicalStack {
    fn take(machine: &mut Machine) -> CanonicalStack {
        CanonicalStack {
            frames: std::mem::take(&mut machine.vm.frames),
            locals: std::mem::take(&mut machine.vm.locals),
            operands: std::mem::take(&mut machine.vm.operands),
        }
    }

    fn restore(self, machine: &mut Machine) {
        debug_assert!(machine.vm.frames.is_empty());
        debug_assert!(machine.vm.locals.is_empty());
        debug_assert!(machine.vm.operands.is_empty());
        machine.vm.frames = self.frames;
        machine.vm.locals = self.locals;
        machine.vm.operands = self.operands;
    }
}

impl NativeContinuation {
    pub(crate) fn effect_reply_type(&self) -> Option<(u32, TypeEnvId)> {
        self.effect
            .map(|effect| (effect.reply_ty, effect.environment))
    }

    pub(crate) fn install_effect_reply(&mut self, value: Value) -> Result<bool, ()> {
        let Some(effect) = self.effect else {
            return Ok(false);
        };
        let (tag, bits) = scalar_parts(effect.reply_kind, value).ok_or(())?;
        let stack_len = self
            .scratch
            .activation
            .finish_effect(effect.consumed, effect.block, effect.instruction, tag, bits)
            .map_err(|_| ())?;
        self.exit
            .resume_at(effect.block, effect.instruction, stack_len);
        self.effect = None;
        Ok(true)
    }

    pub(crate) fn extend_gc_roots(&self, roots: &mut Vec<ObjRef>) {
        for frame in &self.canonical.frames {
            if let Some(crate::machine::FrameCapture::Closure(reference)) = frame.closure {
                roots.push(reference);
            }
        }
        for value in self
            .canonical
            .locals
            .iter()
            .chain(self.canonical.operands.iter())
        {
            if let Value::Obj(reference) = value {
                roots.push(*reference);
            }
        }
        extend_native_roots(
            &self.scratch.activation,
            &self.scratch.continuation_regions,
            self.exit,
            roots,
        );
    }

    pub(crate) fn execution_trace(&self, next_top: bool) -> Vec<lm_heap::FaultSite> {
        let frames: Vec<_> = self.scratch.activation.frames().collect();
        frames
            .into_iter()
            .rev()
            .take(64)
            .enumerate()
            .map(|(depth, frame)| lm_heap::FaultSite {
                function: frame.function(),
                block: frame.block(),
                instruction: if depth == 0 && (self.effect.is_some() || next_top) {
                    frame.instruction()
                } else {
                    frame.instruction().saturating_sub(1)
                },
            })
            .collect()
    }
}

mod cache;

pub(crate) use cache::NativeCodeState;
pub(crate) use cache::DEFAULT_CODE_BUDGET;

pub(crate) enum NativeAttempt {
    Fallback,
    AdvanceToEntry {
        instructions: u32,
    },
    Continue {
        retired: u32,
    },
    InterpretOne {
        retired: u32,
    },
    InterpretInlineCall {
        retired: u32,
    },
    Reenter {
        retired: u32,
    },
    RequestedYield {
        retired: u32,
    },
    Complete {
        outcome: Result<Option<ExecOutcome>, ExecError>,
        retired: u32,
    },
}

/// One host-owned native compiler and immutable region cache.
pub(crate) struct JitEngine {
    compiler: lm_jit::JitEngine,
    code_budget: Arc<cache::CodeBudget>,
    layouts:
        Mutex<std::collections::HashMap<usize, (Weak<lm_bytecode::CodeTables>, NativeCodeState)>>,
    runtime_exits: Mutex<std::collections::BTreeMap<(String, String, String), u64>>,
}

impl Default for JitEngine {
    fn default() -> JitEngine {
        JitEngine::with_code_budget(cache::DEFAULT_CODE_BUDGET)
    }
}

impl JitEngine {
    pub(crate) fn with_code_budget(limit: usize) -> JitEngine {
        JitEngine {
            compiler: lm_jit::JitEngine::default(),
            code_budget: Arc::new(cache::CodeBudget::new(limit)),
            layouts: Mutex::new(std::collections::HashMap::new()),
            runtime_exits: Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

impl std::fmt::Debug for JitEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let layouts = self.layouts.lock().map(|items| items.len()).unwrap_or(0);
        formatter
            .debug_struct("JitEngine")
            .field("layouts", &layouts)
            .finish()
    }
}

impl JitEngine {
    pub(crate) fn native_code(&self, module: &NamespaceRuntime) -> NativeCodeState {
        let tables = module.table_store();
        let key = Arc::as_ptr(&tables) as usize;
        let mut layouts = self
            .layouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        layouts.retain(|_, (tables, _)| tables.strong_count() != 0);
        if let Some((known_tables, known_state)) = layouts.get(&key) {
            if known_tables
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, &tables))
            {
                return known_state.clone();
            }
        }
        let state = NativeCodeState::with_budget(module, Arc::clone(&self.code_budget));
        layouts.insert(key, (Arc::downgrade(&tables), state.clone()));
        state
    }

    pub(crate) fn metrics(&self) -> lm_jit::CompilerMetrics {
        self.compiler.metrics()
    }

    pub(crate) fn reset_metrics(&self) {
        self.compiler.reset_metrics();
    }

    pub(crate) fn profile(&self) -> crate::JitProfile {
        let layouts = self
            .layouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut profile = crate::JitProfile::default();
        let mut rejection_totals = std::collections::BTreeMap::new();
        let mut treatment_totals = std::collections::BTreeMap::new();
        for (tables, state) in layouts.values() {
            let Some(tables) = tables.upgrade() else {
                continue;
            };
            state.append_profile(
                &tables,
                &mut profile,
                &mut rejection_totals,
                &mut treatment_totals,
            );
        }
        profile.hot_functions.sort_by(|left, right| {
            right
                .estimated_instructions
                .cmp(&left.estimated_instructions)
                .then_with(|| left.name.cmp(&right.name))
        });
        profile.rejections = rejection_totals
            .into_iter()
            .map(
                |(reason, estimated_instructions)| crate::JitProfileRejection {
                    reason,
                    estimated_instructions,
                },
            )
            .collect();
        profile.rejections.sort_by(|left, right| {
            right
                .estimated_instructions
                .cmp(&left.estimated_instructions)
                .then_with(|| left.reason.cmp(&right.reason))
        });
        profile.treatment_gaps = treatment_totals
            .into_iter()
            .map(
                |(instruction, estimated_instructions)| crate::JitProfileTreatmentGap {
                    instruction,
                    estimated_instructions,
                },
            )
            .collect();
        profile.treatment_gaps.sort_by(|left, right| {
            right
                .estimated_instructions
                .cmp(&left.estimated_instructions)
                .then_with(|| left.instruction.cmp(&right.instruction))
        });
        let runtime_exits = self
            .runtime_exits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        profile.runtime_exits = runtime_exits
            .iter()
            .map(
                |((function, instruction, exit), count)| crate::JitProfileRuntimeExit {
                    function: function.clone(),
                    instruction: instruction.clone(),
                    exit: exit.clone(),
                    count: *count,
                },
            )
            .collect();
        profile.runtime_exits.sort_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.function.cmp(&right.function))
                .then_with(|| left.instruction.cmp(&right.instruction))
        });
        profile
    }

    pub(crate) fn reset_profile(&self) {
        let layouts = self
            .layouts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, state) in layouts.values() {
            state.reset_profile();
        }
        self.runtime_exits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub(crate) fn execute(
        &self,
        machine: &mut Machine,
        context: &mut NativeExecutionContext<'_>,
        native: &NativeCodeState,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
    ) -> NativeAttempt {
        if machine.has_native_continuation() {
            return self.execute_region(machine, context, native, None, scratch, metrics);
        }
        let Some(frame) = machine.vm.frames.last() else {
            metrics.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        let function = frame.func;
        let Some(slot) = native.slot(function) else {
            metrics.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        metrics.note_native_entry_attempt();
        let region = match slot.region(&self.compiler, || {
            let runtime =
                context
                    .module
                    .funcs
                    .get(function as usize)
                    .ok_or(Failure::Unsupported(
                        lm_jit::UnsupportedReason::MissingSource,
                    ))?;
            let (unit, local) = context
                .module
                .code_namespace()
                .function_unit(function)
                .map_err(|_| Failure::Unsupported(lm_jit::UnsupportedReason::MissingSource))?;
            let mut input = FunctionInput::new(
                function,
                runtime,
                unit.module(),
                context.module.bundle(),
                local,
            );
            input.set_runtime_string_count(context.module.strings.len());
            input.set_runtime_core_roles(&context.module.core_roles);
            input.set_function_behaviors(native.behaviors());
            let relocation = context
                .module
                .code_namespace()
                .relocation(unit.id())
                .ok_or(Failure::Unsupported(
                    lm_jit::UnsupportedReason::MissingSource,
                ))?;
            input.set_class_relocation(relocation.classes());
            let mut callees = Vec::new();
            for instruction in runtime.blocks.iter().flatten() {
                if let lm_bytecode::Instr::Call(callee)
                | lm_bytecode::Instr::CallG { func: callee, .. } = instruction
                {
                    if !callees.contains(callee) {
                        callees.push(*callee);
                    }
                }
            }
            let mut callee_index = 0usize;
            while let Some(callee) = callees.get(callee_index).copied() {
                let callee_runtime =
                    context
                        .module
                        .funcs
                        .get(callee as usize)
                        .ok_or(Failure::Unsupported(
                            lm_jit::UnsupportedReason::MissingSource,
                        ))?;
                let (callee_unit, callee_local) = context
                    .module
                    .code_namespace()
                    .function_unit(callee)
                    .map_err(|_| Failure::Unsupported(lm_jit::UnsupportedReason::MissingSource))?;
                let callee_relocation = context
                    .module
                    .code_namespace()
                    .relocation(callee_unit.id())
                    .ok_or(Failure::Unsupported(
                        lm_jit::UnsupportedReason::MissingSource,
                    ))?;
                input.add_relocated_direct_callee(
                    callee,
                    callee_runtime,
                    callee_unit.module(),
                    context.module.bundle(),
                    callee_local,
                    callee_relocation.classes(),
                );
                let is_constructor = callee_unit.module().bindings.iter().any(|binding| {
                    binding.func == callee_local && binding.class != lm_bytecode::NO_CLASS
                });
                if is_constructor {
                    for instruction in callee_runtime.blocks.iter().flatten() {
                        if let lm_bytecode::Instr::Call(nested)
                        | lm_bytecode::Instr::CallG { func: nested, .. } = instruction
                        {
                            if !callees.contains(nested) {
                                callees.push(*nested);
                            }
                        }
                    }
                }
                callee_index += 1;
            }
            Ok(input)
        }) {
            Ok(region) => region,
            Err(cache::RegionFailure::Compile(Failure::Unsupported(reason))) => {
                metrics.note_unsupported_region_fallback(reason);
                return NativeAttempt::Fallback;
            }
            Err(
                cache::RegionFailure::Compile(Failure::BackendUnavailable)
                | cache::RegionFailure::Busy,
            ) => {
                metrics.note_backend_unavailable();
                return NativeAttempt::Fallback;
            }
            Err(cache::RegionFailure::Capacity) => {
                metrics.note_code_cache_capacity();
                return NativeAttempt::Fallback;
            }
        };
        self.execute_region(machine, context, native, Some(region), scratch, metrics)
    }

    fn execute_region(
        &self,
        machine: &mut Machine,
        context: &mut NativeExecutionContext<'_>,
        native: &NativeCodeState,
        region: Option<Arc<lm_jit::CompiledRegion>>,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
    ) -> NativeAttempt {
        let module = context.module;
        let instruction_limit = context.instruction_limit;
        let mut continuation = machine.take_native_continuation();
        let resumed = continuation.is_some();
        let (root_region, active_region, entry_index, root_frame, base, operand_base) =
            if let Some(mut held) = continuation.take() {
                let Some(top) = held.scratch.activation.top_frame() else {
                    return reject_native_continuation(machine, native, *held, metrics);
                };
                let top_block = top.block();
                let top_instruction = top.instruction();
                let Some(active_region) = held.scratch.continuation_regions.last().cloned() else {
                    return reject_native_continuation(machine, native, *held, metrics);
                };
                let Some(root_region) = held.scratch.continuation_regions.first().cloned() else {
                    return reject_native_continuation(machine, native, *held, metrics);
                };
                if held.scratch.continuation_regions.len() != held.scratch.activation.frame_count()
                {
                    return reject_native_continuation(machine, native, *held, metrics);
                }
                let Some(entry) = active_region.resume_plan(top_block, top_instruction) else {
                    return reject_native_continuation(machine, native, *held, metrics);
                };
                let entry_index = entry.index();
                let continuation_scratch = std::mem::take(&mut held.scratch);
                let canonical = std::mem::take(&mut held.canonical);
                let root_frame = held.root_frame;
                let root_local = held.root_local;
                let root_operand = held.root_operand;
                canonical.restore(machine);
                *scratch = continuation_scratch;
                continuation = Some(held);
                metrics.note_native_continuation_resume();
                (
                    root_region,
                    active_region,
                    entry_index,
                    root_frame,
                    root_local,
                    root_operand,
                )
            } else {
                let Some(region) = region else {
                    metrics.note_backend_unavailable();
                    return NativeAttempt::Fallback;
                };
                let Some(frame_index) = machine.vm.frames.len().checked_sub(1) else {
                    metrics.note_missing_entry_fallback();
                    return NativeAttempt::Fallback;
                };
                let frame = &machine.vm.frames[frame_index];
                let Some((capture_tag, capture_bits, capture_data, capture_len)) =
                    frame_capture_parts(machine, frame.closure)
                else {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                };
                let Some(entry) = region.entry_plan(frame.block, frame.ip) else {
                    metrics.note_missing_entry_fallback();
                    return region
                        .distance_to_entry(frame.block, frame.ip)
                        .map_or(NativeAttempt::Fallback, |instructions| {
                            NativeAttempt::AdvanceToEntry { instructions }
                        });
                };
                let entry_index = entry.index();
                let base = frame.base_local as usize;
                let operand_base = frame.base_operand as usize;
                if operand_base > machine.vm.operands.len() {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                }
                let Some(end) = base.checked_add(region.local_kinds().len()) else {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                };
                let Some(locals) = machine.vm.locals.get(base..end) else {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                };
                let Some(stack_bound) = machine
                    .vm
                    .locals
                    .len()
                    .checked_add(operand_base)
                    .and_then(|used| used.checked_add(region.max_stack_values()))
                else {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                };
                if stack_bound > machine.config.max_stack_values as usize {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                }
                let operands = &machine.vm.operands[operand_base..];
                if operands.len() != entry.operand_kinds().len() {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                }
                scratch.continuation_regions.clear();
                if scratch
                    .activation
                    .prepare_root(NativePreparation {
                        function: frame.func,
                        environment: frame.env.0,
                        capture_tag,
                        capture_bits,
                        capture_data,
                        capture_len,
                        block: frame.block,
                        instruction: frame.ip,
                        local_count: region.local_kinds().len(),
                        max_stack: region.max_stack(),
                        operand_len: operands.len(),
                        scalar_limit: machine.config.max_stack_values as usize,
                        frame_limit: machine.config.max_frames as usize,
                    })
                    .is_err()
                {
                    metrics.note_backend_unavailable();
                    return NativeAttempt::Fallback;
                }
                let mut guarded = 0u64;
                let (bits, tags, local_states, stack_bits, stack_tags) =
                    scratch.activation.root_buffers_mut();
                for (slot, (kind, live)) in region
                    .local_kinds()
                    .iter()
                    .copied()
                    .zip(entry.live_locals().iter().copied())
                    .enumerate()
                {
                    let complete_root = region.requires_complete_roots()
                        && matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_));
                    if !live && !complete_root {
                        continue;
                    }
                    if !live && locals[slot] == Value::Uninit {
                        continue;
                    }
                    guarded += 1;
                    let Some((tag, value)) = scalar_parts(kind, locals[slot]) else {
                        metrics.note_guard_failure(guarded);
                        return NativeAttempt::Fallback;
                    };
                    bits[slot] = value;
                    tags[slot] = tag;
                    local_states[slot] = LOCAL_INITIALIZED;
                }
                for (slot, (kind, value)) in entry
                    .operand_kinds()
                    .iter()
                    .copied()
                    .zip(operands.iter().copied())
                    .enumerate()
                {
                    guarded += 1;
                    let Some((tag, value)) = scalar_parts(kind, value) else {
                        metrics.note_guard_failure(guarded);
                        return NativeAttempt::Fallback;
                    };
                    stack_bits[slot] = value;
                    stack_tags[slot] = tag;
                }
                metrics.note_guarded_values(guarded);
                (
                    Arc::clone(&region),
                    region,
                    entry_index,
                    frame_index,
                    base,
                    operand_base,
                )
            };
        scratch.image_slots.clear();
        if let Some(slots) = context.slots {
            if scratch.image_slots.try_reserve_exact(slots.len()).is_err() {
                metrics.note_backend_unavailable();
                return NativeAttempt::Fallback;
            }
            scratch
                .image_slots
                .extend(slots.iter().map(|slot| match slot {
                    ImageSlotTarget::Empty => lm_jit::NativeImageSlot::empty(),
                    ImageSlotTarget::Function(function) => {
                        lm_jit::NativeImageSlot::function(*function)
                    }
                    ImageSlotTarget::Class { class, constructor } => {
                        lm_jit::NativeImageSlot::class(*class, *constructor)
                    }
                    ImageSlotTarget::Value(_) => lm_jit::NativeImageSlot::value(),
                    ImageSlotTarget::Process { .. } => lm_jit::NativeImageSlot::process(),
                }));
        }
        let original_fuel = machine.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(instruction_limit));
        let max_stack_values = machine.config.max_stack_values as usize;
        let max_frames = machine.config.max_frames as usize;
        let base_frames = root_frame;
        metrics.note_native_entry();
        scratch.activation.begin_execution();
        // SAFETY: Native execution cannot change the literal table.
        let literals = unsafe {
            lm_jit::NativeLiteralView::from_raw_parts(
                machine.vm.literals.as_ptr(),
                machine.vm.literals.len(),
            )
        };
        // SAFETY: The scratch slot array stays fixed during this native turn.
        let image_slots = unsafe {
            lm_jit::NativeImageSlotView::from_raw_parts(
                scratch.image_slots.as_ptr(),
                scratch.image_slots.len(),
            )
        };
        let (
            exit,
            allocations,
            inline_allocations,
            pending_instance_allocations,
            pending_instance_releases,
            pending_instance_materializations,
            scalar_replaced_allocations,
            collection_slow_paths,
        ) = {
            let type_environments = std::mem::take(&mut machine.native_type_environments);
            let resolved_calls = std::mem::take(&mut machine.native_resolved_calls);
            let mut runtime = MachineRuntime {
                machine,
                envs: context.envs,
                type_environments,
                resolved_calls,
                module,
                base_local: base,
                base_operand: operand_base,
                allocations: 0,
                inline_allocations: 0,
                pending_instance_allocations: 0,
                pending_instance_releases: 0,
                pending_instance_materializations: 0,
                scalar_replaced_allocations: 0,
                collection_slow_paths: 0,
            };
            let mut active_region = active_region;
            let mut active_entry = entry_index;
            let mut prior_retired = 0u64;
            let result = loop {
                let root_capacity = active_region
                    .max_roots()
                    .max(scratch.roots.len())
                    .max(scratch.root_tags.len())
                    .max(scratch.root_states.len())
                    .max(1);
                scratch.roots.resize(root_capacity, 0);
                scratch.root_tags.resize(root_capacity, 0);
                scratch.root_states.resize(root_capacity, 0);
                let Some(remaining) = batch_fuel.checked_sub(prior_retired) else {
                    break Err(Failure::BackendUnavailable);
                };
                let type_environments = match runtime.type_environments.view() {
                    Ok(view) => view,
                    Err(error) => break Err(error),
                };
                let resolved_calls = match runtime.resolved_calls.view() {
                    Ok(view) => view,
                    Err(error) => break Err(error),
                };
                let type_store_id = runtime.envs.canonical_store_id();
                let heap = runtime.machine.vm.heap.jit_view();
                let mut exit = match active_region.execute(
                    &mut runtime,
                    &mut scratch.activation,
                    NativeExecution {
                        entry: active_entry,
                        entries: native.entries(),
                        base_stack_values: base.saturating_add(operand_base),
                        max_stack_values,
                        base_frames,
                        max_frames,
                        roots: &mut scratch.roots,
                        root_tags: &mut scratch.root_tags,
                        root_states: &mut scratch.root_states,
                        fuel: remaining,
                        poll: context.poll.after_retirement(prior_retired),
                        heap,
                        class_parents: native.class_parents(),
                        dispatch_rows: native.dispatch_rows(),
                        dispatch_methods: native.dispatch_methods(),
                        literals,
                        type_store_id,
                        type_environments,
                        resolved_calls,
                        image_slots,
                    },
                ) {
                    Ok(exit) => exit,
                    Err(error) => break Err(error),
                };
                if scratch.activation.pending_instances().next().is_some()
                    && continuation_regions(
                        native,
                        &root_region,
                        &scratch.activation,
                        &mut scratch.continuation_regions,
                    )
                    .is_err()
                {
                    break Err(Failure::BackendUnavailable);
                }
                let (materializations, releases) = match materialize_pending_instances(
                    runtime.machine,
                    &mut scratch.activation,
                    &scratch.continuation_regions,
                    &mut exit,
                ) {
                    Ok(activity) => activity,
                    Err(()) => break Err(Failure::BackendUnavailable),
                };
                runtime.pending_instance_materializations = runtime
                    .pending_instance_materializations
                    .saturating_add(materializations);
                runtime.pending_instance_releases =
                    runtime.pending_instance_releases.saturating_add(releases);
                if exit.kind() == ExitKind::StackRollover {
                    let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(rollover_region) = scratch
                        .activation
                        .top_frame()
                        .and_then(|frame| native.slot(frame.function()))
                        .and_then(|slot| slot.compiled())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(resume) = rollover_region
                        .resume_plan(exit.block(), exit.instruction())
                        .map(|entry| entry.index())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    prior_retired = next_retired;
                    active_region = rollover_region;
                    active_entry = resume;
                    continue;
                }
                if exit.kind() == ExitKind::GrowRoots {
                    let Ok(required) = usize::try_from(exit.result()) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let grow_region = scratch
                        .activation
                        .top_frame()
                        .and_then(|frame| native.slot(frame.function()))
                        .and_then(|slot| slot.compiled());
                    let resume = grow_region
                        .as_deref()
                        .and_then(|region| region.resume_plan(exit.block(), exit.instruction()))
                        .map(|entry| entry.index());
                    let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let growth = required.saturating_sub(scratch.roots.len());
                    let grew = required <= max_stack_values
                        && scratch.roots.try_reserve(growth).is_ok()
                        && scratch.root_tags.try_reserve(growth).is_ok()
                        && scratch.root_states.try_reserve(growth).is_ok();
                    if grew {
                        scratch.roots.resize(required.max(1), 0);
                        scratch.root_tags.resize(required.max(1), 0);
                        scratch.root_states.resize(required.max(1), 0);
                        metrics.note_native_activation_grow();
                        if let (Some(grow_region), Some(resume)) = (grow_region, resume) {
                            prior_retired = next_retired;
                            active_region = grow_region;
                            active_entry = resume;
                            continue;
                        }
                        break exit.add_prior_retired(prior_retired);
                    }
                    break Err(Failure::BackendUnavailable);
                }
                if exit.kind() == ExitKind::GrowActivation {
                    let required_scalars = (exit.result() >> 32) as usize;
                    let Some(required_frames) = scratch.activation.frame_count().checked_add(1)
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let grow_region = scratch
                        .activation
                        .top_frame()
                        .and_then(|frame| native.slot(frame.function()))
                        .and_then(|slot| slot.compiled());
                    let resume = grow_region
                        .as_deref()
                        .and_then(|region| region.resume_plan(exit.block(), exit.instruction()))
                        .map(|entry| entry.index());
                    let grew = scratch.activation.grow(
                        required_scalars,
                        required_frames,
                        max_stack_values,
                        max_frames,
                    );
                    if matches!(grew, Ok(true)) {
                        metrics.note_native_activation_grow();
                        if let (Some(grow_region), Some(resume)) = (grow_region, resume) {
                            prior_retired = next_retired;
                            active_region = grow_region;
                            active_entry = resume;
                            continue;
                        }
                        break exit.add_prior_retired(prior_retired);
                    }
                }
                if exit.kind() == ExitKind::TypeResolution {
                    let type_index = exit.result() & u64::from(u32::MAX);
                    let Ok(type_index) = u32::try_from(type_index) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some((function, environment)) = scratch
                        .activation
                        .top_frame()
                        .map(|frame| (frame.function(), TypeEnvId(frame.environment())))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if exit.result_tag() != u64::from(environment.0) {
                        break Err(Failure::BackendUnavailable);
                    }
                    let family = runtime.machine.close_option_family_at(
                        module,
                        runtime.envs,
                        type_index,
                        environment,
                    );
                    if let Ok(family) = family {
                        let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                            break Err(Failure::BackendUnavailable);
                        };
                        let resolve_region = scratch
                            .activation
                            .top_frame()
                            .and_then(|frame| native.slot(frame.function()))
                            .and_then(|slot| slot.compiled());
                        let resume = resolve_region
                            .as_deref()
                            .and_then(|region| region.resume_plan(exit.block(), exit.instruction()))
                            .map(|entry| entry.index());
                        let cached = runtime.type_environments.cache_type_site(
                            runtime.envs.canonical_store_id(),
                            function,
                            exit.block(),
                            exit.instruction(),
                            environment.0,
                            family,
                        );
                        if cached {
                            if let (Some(resolve_region), Some(resume)) = (resolve_region, resume) {
                                prior_retired = next_retired;
                                active_region = resolve_region;
                                active_entry = resume;
                                continue;
                            }
                            break exit.add_prior_retired(prior_retired);
                        }
                    }
                }
                if exit.kind() == ExitKind::TypeEnvironment {
                    metrics.note_native_type_environment_exit();
                    let Some((function, parent)) = scratch
                        .activation
                        .frames()
                        .last()
                        .map(|frame| (frame.function(), TypeEnvId(frame.environment())))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if exit.result() != u64::from(parent.0) {
                        break Err(Failure::BackendUnavailable);
                    }
                    let Some(resolve_region) =
                        native.slot(function).and_then(|slot| slot.compiled())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(application) = resolve_region
                        .type_environment_application(exit.block(), exit.instruction())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if let Ok(child) = runtime.envs.derive(module, parent, application) {
                        let cached = runtime.type_environments.cache_type_site(
                            runtime.envs.canonical_store_id(),
                            function,
                            exit.block(),
                            exit.instruction(),
                            parent.0,
                            child.0,
                        );
                        if cached {
                            let Some(next_retired) = prior_retired.checked_add(exit.retired())
                            else {
                                break Err(Failure::BackendUnavailable);
                            };
                            if let Some(resume) = resolve_region
                                .resume_plan(exit.block(), exit.instruction())
                                .map(|entry| entry.index())
                            {
                                prior_retired = next_retired;
                                active_region = resolve_region;
                                active_entry = resume;
                                continue;
                            }
                        }
                        metrics.note_native_type_environment_fallback();
                    }
                }
                if matches!(
                    exit.kind(),
                    ExitKind::InterfaceCall | ExitKind::GenericVirtualCall
                ) {
                    enum ResolvedCallSite {
                        Interface(lm_jit::InterfaceCallSite),
                        GenericVirtual(lm_jit::GenericVirtualCallSite),
                    }
                    let Some(frame) = scratch.activation.top_frame() else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(resolve_region) = native
                        .slot(frame.function())
                        .and_then(|slot| slot.compiled())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let site = match exit.kind() {
                        ExitKind::InterfaceCall => resolve_region
                            .interface_call_site(exit.block(), exit.instruction())
                            .map(ResolvedCallSite::Interface),
                        ExitKind::GenericVirtualCall => resolve_region
                            .generic_virtual_call_site(exit.block(), exit.instruction())
                            .map(ResolvedCallSite::GenericVirtual),
                        _ => None,
                    };
                    let Some(site) = site else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let (function, receiver_kind, parameter_count) = match site {
                        ResolvedCallSite::Interface(site) => (
                            site.function(),
                            site.receiver_kind(),
                            site.parameter_count(),
                        ),
                        ResolvedCallSite::GenericVirtual(site) => (
                            site.function(),
                            site.receiver_kind(),
                            site.parameter_count(),
                        ),
                    };
                    if function != frame.function() || parameter_count == 0 {
                        break Err(Failure::BackendUnavailable);
                    }
                    let Some(receiver_index) = frame.operands().len().checked_sub(parameter_count)
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some((&receiver_bits, &receiver_tag)) = frame
                        .operands()
                        .get(receiver_index)
                        .zip(frame.operand_tags().get(receiver_index))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(receiver) =
                        materialized_value(receiver_kind, receiver_tag, receiver_bits)
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let parent = TypeEnvId(frame.environment());
                    let dispatch = module.dispatch_store();
                    let resolved = match site {
                        ResolvedCallSite::Interface(site) => {
                            runtime.machine.resolve_interface_target(
                                module,
                                dispatch.as_ref(),
                                runtime.envs,
                                parent,
                                site.interface(),
                                site.method(),
                                site.receiver_type(),
                                site.application(),
                                receiver,
                            )
                        }
                        ResolvedCallSite::GenericVirtual(site) => {
                            runtime.machine.resolve_virtual_generic_target(
                                module,
                                dispatch.as_ref(),
                                runtime.envs,
                                parent,
                                site.selector(),
                                site.application(),
                                receiver,
                            )
                        }
                    };
                    if let Ok((target, environment)) = resolved {
                        let cached = runtime.resolved_calls.cache_call_site(
                            runtime.envs.canonical_store_id(),
                            function,
                            exit.block(),
                            exit.instruction(),
                            parent.0,
                            exit.result(),
                            target,
                            environment.0,
                            0,
                            0,
                        );
                        let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                            break Err(Failure::BackendUnavailable);
                        };
                        let resume = resolve_region
                            .resume_plan(exit.block(), exit.instruction())
                            .map(|entry| entry.index());
                        if cached {
                            if let Some(resume) = resume {
                                prior_retired = next_retired;
                                active_region = resolve_region;
                                active_entry = resume;
                                continue;
                            }
                        }
                    }
                }
                if exit.kind() == ExitKind::CallbackCall {
                    let Some(frame) = scratch.activation.top_frame() else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(resolve_region) = native
                        .slot(frame.function())
                        .and_then(|slot| slot.compiled())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some(site) = resolve_region
                        .call_value_site(exit.block(), exit.instruction())
                        .filter(|site| site.is_callback())
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if site.function() != frame.function() {
                        break Err(Failure::BackendUnavailable);
                    }
                    let Some(callable_index) = frame
                        .operands()
                        .len()
                        .checked_sub(site.parameter_count().saturating_add(1))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some((&callable_bits, &callable_tag)) = frame
                        .operands()
                        .get(callable_index)
                        .zip(frame.operand_tags().get(callable_index))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if callable_bits != exit.result()
                        || callable_tag != exit.result_tag()
                        || callable_tag != ValueTag::Callback as u64
                    {
                        break Err(Failure::BackendUnavailable);
                    }
                    let Some(Value::Callback(reference)) =
                        runtime::tagged_value(callable_tag, callable_bits)
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Ok(descriptor) = runtime.machine.callback(reference) else {
                        break exit.add_prior_retired(prior_retired);
                    };
                    let target = descriptor.func;
                    let environment = descriptor.env;
                    let capture_data = descriptor.captures.as_ptr() as usize;
                    let capture_len = descriptor.captures.len();
                    let parent = frame.environment();
                    let cached = runtime.resolved_calls.cache_call_site(
                        runtime.envs.canonical_store_id(),
                        frame.function(),
                        exit.block(),
                        exit.instruction(),
                        parent,
                        callable_bits,
                        target,
                        environment.0,
                        capture_data,
                        capture_len,
                    );
                    let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let resume = resolve_region
                        .resume_plan(exit.block(), exit.instruction())
                        .map(|entry| entry.index());
                    if cached {
                        if let Some(resume) = resume {
                            prior_retired = next_retired;
                            active_region = resolve_region;
                            active_entry = resume;
                            continue;
                        }
                    }
                }
                if exit.kind() != ExitKind::Return || scratch.activation.frame_count() <= 1 {
                    break exit.add_prior_retired(prior_retired);
                }
                let parent = {
                    let parent_index = scratch.activation.frame_count().saturating_sub(2);
                    let Some(parent) = scratch.activation.frame(parent_index) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    (parent.function(), parent.block(), parent.instruction())
                };
                let Some(parent_region) = native.slot(parent.0).and_then(|slot| slot.compiled())
                else {
                    break Err(Failure::BackendUnavailable);
                };
                let Some(parent_entry) = parent_region.resume_plan(parent.1, parent.2) else {
                    break Err(Failure::BackendUnavailable);
                };
                let parent_entry = parent_entry.index();
                let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                    break Err(Failure::BackendUnavailable);
                };
                if scratch
                    .activation
                    .finish_detached_return(exit.result_tag(), exit.result())
                    .is_err()
                {
                    break Err(Failure::BackendUnavailable);
                }
                prior_retired = next_retired;
                active_region = parent_region;
                active_entry = parent_entry;
            };
            (
                result,
                runtime.allocations,
                runtime.inline_allocations,
                runtime.pending_instance_allocations,
                runtime.pending_instance_releases,
                runtime.pending_instance_materializations,
                runtime.scalar_replaced_allocations,
                runtime.collection_slow_paths,
            )
        };
        metrics.note_native_allocations(allocations);
        metrics.note_native_inline_allocations(inline_allocations);
        metrics.note_pending_instance_activity(
            pending_instance_allocations,
            pending_instance_releases,
            pending_instance_materializations,
        );
        metrics.note_scalar_replacements(scalar_replaced_allocations);
        metrics.note_native_collection_slow_paths(collection_slow_paths);
        let exit = match exit {
            Ok(exit) => exit,
            Err(_) => {
                metrics.note_backend_unavailable();
                metrics.note_native_fault_exit();
                return malformed_native_execution(machine, original_fuel, 0, instruction_limit);
            }
        };
        let Ok(retired) = u32::try_from(exit.retired()) else {
            metrics.note_backend_unavailable();
            metrics.note_native_fault_exit();
            return malformed_native_execution(
                machine,
                original_fuel,
                exit.retired(),
                instruction_limit,
            );
        };
        if retired > instruction_limit || exit.retired() > original_fuel {
            metrics.note_backend_unavailable();
            metrics.note_native_fault_exit();
            return malformed_native_execution(
                machine,
                original_fuel,
                exit.retired(),
                instruction_limit,
            );
        }
        if context.profile
            && matches!(
                exit.kind(),
                ExitKind::Replay | ExitKind::Literal | ExitKind::Boundary
            )
        {
            self.record_runtime_exit(module, scratch, exit);
        }
        if metrics.sample_productivity() {
            let sample = match exit.kind() {
                ExitKind::Fuel
                | ExitKind::Return
                | ExitKind::Replay
                | ExitKind::Effect
                | ExitKind::Boundary => true,
                ExitKind::Call => u32::try_from(exit.result())
                    .ok()
                    .is_some_and(|target| native.call_target_is_denied(target)),
                _ => false,
            };
            if sample {
                let demoted =
                    scratch.activation.frames().next().is_some_and(|frame| {
                        native.note_native_exit(frame.function(), exit.retired())
                    });
                if demoted {
                    metrics.note_unproductive_native_demotion();
                }
            }
        }
        machine.vm.fuel -= exit.retired();
        let retain_effect = exit.kind() == ExitKind::Effect
            && retired < instruction_limit
            && exit.retired() < original_fuel;
        if retain_effect {
            let retained = continuation_regions(
                native,
                &root_region,
                &scratch.activation,
                &mut scratch.continuation_regions,
            )
            .ok()
            .and_then(|()| scratch.continuation_regions.last())
            .and_then(|region| {
                native_effect_request(module, region.as_ref(), &scratch.activation, exit)
            });
            if let Some(request) = retained {
                let Some(retired) = retired.checked_add(1) else {
                    return malformed_native_exit(retired);
                };
                machine.vm.fuel -= 1;
                let state = NativeContinuation {
                    scratch: std::mem::take(scratch),
                    canonical: CanonicalStack::take(machine),
                    root_frame,
                    root_local: base,
                    root_operand: operand_base,
                    exit,
                    effect: Some(request.continuation),
                };
                let held = if let Some(mut held) = continuation {
                    *held = state;
                    held
                } else {
                    Box::new(state)
                };
                machine.set_native_continuation(held);
                metrics.note_native_retired(u64::from(retired));
                metrics.note_native_effect_exit();
                metrics.note_native_continuation_suspend();
                return NativeAttempt::Complete {
                    outcome: Ok(Some(ExecOutcome::Perform {
                        op: request.op,
                        args: request.args,
                    })),
                    retired,
                };
            }
        }
        let retain_continuation = exit.kind() == ExitKind::Fuel
            && exit.retired() == batch_fuel
            && batch_fuel == u64::from(instruction_limit)
            && u64::from(instruction_limit) <= original_fuel;
        if retain_continuation {
            let Ok(()) = continuation_regions(
                native,
                &root_region,
                &scratch.activation,
                &mut scratch.continuation_regions,
            ) else {
                metrics.note_backend_unavailable();
                metrics.note_native_fault_exit();
                return malformed_native_execution(
                    machine,
                    original_fuel,
                    exit.retired(),
                    instruction_limit,
                );
            };
            let state = NativeContinuation {
                scratch: std::mem::take(scratch),
                canonical: CanonicalStack::take(machine),
                root_frame,
                root_local: base,
                root_operand: operand_base,
                exit,
                effect: None,
            };
            let held = if let Some(mut held) = continuation {
                *held = state;
                held
            } else {
                Box::new(state)
            };
            machine.set_native_continuation(held);
            metrics.note_native_retired(retired as u64);
            metrics.note_native_continuation_suspend();
            return NativeAttempt::Complete {
                outcome: Ok(None),
                retired,
            };
        }
        if continuation_regions(
            native,
            &root_region,
            &scratch.activation,
            &mut scratch.continuation_regions,
        )
        .is_err()
        {
            return malformed_native_exit(retired);
        }
        let top_child = match materialize_native_frames(
            machine,
            &scratch.continuation_regions,
            &scratch.activation,
            exit,
            root_frame,
            base,
            operand_base,
        ) {
            Ok(region) => region,
            Err(()) => return malformed_native_exit(retired),
        };
        let Some(top_region) = scratch.continuation_regions.last() else {
            return malformed_native_exit(retired);
        };
        metrics.note_native_retired(retired as u64);
        metrics.note_materialization();
        if resumed {
            metrics.note_native_continuation_materialization();
        }
        match exit.kind() {
            ExitKind::Fuel
            | ExitKind::Poll
            | ExitKind::Replay
            | ExitKind::Literal
            | ExitKind::Call
            | ExitKind::GrowRoots
            | ExitKind::GrowActivation
            | ExitKind::TypeResolution
            | ExitKind::TypeEnvironment
            | ExitKind::InterfaceCall
            | ExitKind::GenericVirtualCall
            | ExitKind::CallbackCall
            | ExitKind::Effect
            | ExitKind::Boundary => {
                if exit.kind() == ExitKind::Poll {
                    return NativeAttempt::RequestedYield { retired };
                }
                let interpreter = matches!(exit.kind(), ExitKind::Replay | ExitKind::Literal);
                let grow_value_call = exit.kind() == ExitKind::GrowActivation
                    && top_region
                        .call_value_site(exit.block(), exit.instruction())
                        .is_some();
                if grow_value_call {
                    metrics.note_native_call_value_exit();
                    return NativeAttempt::InterpretOne { retired };
                }
                if matches!(exit.kind(), ExitKind::Call | ExitKind::GrowActivation) {
                    if retired == instruction_limit {
                        return NativeAttempt::Complete {
                            outcome: Ok(None),
                            retired,
                        };
                    }
                    if exit.retired() == original_fuel {
                        return NativeAttempt::Complete {
                            outcome: Err(ExecError::Fault(crate::FaultCode::OutOfFuel)),
                            retired,
                        };
                    }
                    let target = exit.result() & u64::from(u32::MAX);
                    let Ok(target) = u32::try_from(target) else {
                        return malformed_native_exit(retired);
                    };
                    let Ok(environment) = u32::try_from(exit.result_tag()) else {
                        return malformed_native_exit(retired);
                    };
                    if metrics.sample_productivity() {
                        native.promote_call_target(target);
                    }
                    machine.vm.fuel -= 1;
                    let retired = retired + 1;
                    metrics.note_native_retired(1);
                    return match machine.start_native_call(module, target, TypeEnvId(environment)) {
                        Ok(()) => NativeAttempt::Reenter { retired },
                        Err(fault) => {
                            metrics.note_native_fault_exit();
                            NativeAttempt::Complete {
                                outcome: Err(ExecError::Fault(fault)),
                                retired,
                            }
                        }
                    };
                }
                if matches!(
                    exit.kind(),
                    ExitKind::GrowRoots
                        | ExitKind::GrowActivation
                        | ExitKind::Effect
                        | ExitKind::TypeResolution
                        | ExitKind::TypeEnvironment
                        | ExitKind::InterfaceCall
                        | ExitKind::GenericVirtualCall
                        | ExitKind::CallbackCall
                        | ExitKind::Boundary
                ) {
                    if matches!(exit.kind(), ExitKind::Effect) {
                        metrics.note_native_effect_exit();
                    }
                    NativeAttempt::InterpretOne { retired }
                } else if interpreter {
                    match exit.kind() {
                        ExitKind::Replay => metrics.note_native_replay_exit(),
                        ExitKind::Literal => metrics.note_native_literal_exit(),
                        _ => unreachable!(),
                    }
                    NativeAttempt::InterpretOne { retired }
                } else if exit.retired() == batch_fuel {
                    let outcome = if u64::from(instruction_limit) <= original_fuel {
                        Ok(None)
                    } else {
                        Err(ExecError::Fault(crate::FaultCode::OutOfFuel))
                    };
                    NativeAttempt::Complete { outcome, retired }
                } else {
                    NativeAttempt::Continue { retired }
                }
            }
            ExitKind::Return => {
                if exit.stack_len() != 0 || top_child.is_some() {
                    return malformed_native_exit(retired);
                }
                let Some(value) =
                    parts_value(top_region.result_kind(), exit.result_tag(), exit.result())
                else {
                    return malformed_native_exit(retired);
                };
                match machine.finish_native_return(value) {
                    Ok(ExecOutcome::Continue) => NativeAttempt::Reenter { retired },
                    Ok(outcome) => NativeAttempt::Complete {
                        outcome: Ok(Some(outcome)),
                        retired,
                    },
                    Err(fault) => NativeAttempt::Complete {
                        outcome: Err(ExecError::Fault(fault)),
                        retired,
                    },
                }
            }
            ExitKind::IntegerOverflow
            | ExitKind::DivideByZero
            | ExitKind::TypeMismatch
            | ExitKind::UninitializedField
            | ExitKind::HeapLimit
            | ExitKind::Unreachable
            | ExitKind::StackLimit
            | ExitKind::GuestFault => {
                metrics.note_native_fault_exit();
                let fault = match exit.kind() {
                    ExitKind::IntegerOverflow => crate::FaultCode::IntegerOverflow,
                    ExitKind::DivideByZero => crate::FaultCode::DivideByZero,
                    ExitKind::TypeMismatch => crate::FaultCode::TypeMismatch,
                    ExitKind::UninitializedField => crate::FaultCode::UninitializedField,
                    ExitKind::HeapLimit => crate::FaultCode::HeapLimit,
                    ExitKind::Unreachable => crate::FaultCode::UnreachableCode,
                    ExitKind::StackLimit => crate::FaultCode::StackLimit,
                    ExitKind::GuestFault => {
                        let Ok(index) = usize::try_from(exit.result()) else {
                            return malformed_native_exit(retired);
                        };
                        let Some(fault) = lm_abi::FAULT_CODES.get(index).copied() else {
                            return malformed_native_exit(retired);
                        };
                        fault
                    }
                    _ => unreachable!(),
                };
                NativeAttempt::Complete {
                    outcome: Err(ExecError::Fault(fault)),
                    retired,
                }
            }
            ExitKind::StackRollover => malformed_native_exit(retired),
            ExitKind::InlineCall => NativeAttempt::InterpretInlineCall { retired },
        }
    }

    fn record_runtime_exit(
        &self,
        module: &NamespaceRuntime,
        scratch: &NativeScratch,
        exit: lm_jit::ExecutionExit,
    ) {
        let Some(frame) = scratch.activation.top_frame() else {
            return;
        };
        let Some(function) = module.funcs.get(frame.function() as usize) else {
            return;
        };
        let instruction = function
            .blocks
            .get(exit.block() as usize)
            .and_then(|block| block.get(exit.instruction() as usize))
            .map_or_else(|| "<missing>".to_string(), |value| format!("{value:?}"));
        let key = (
            function.name.clone(),
            instruction,
            format!("{:?}", exit.kind()),
        );
        let mut exits = self
            .runtime_exits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        exits
            .entry(key)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
    }
}

struct PendingInstanceMaterialization {
    token: u64,
    class: u32,
    environment: u32,
    frozen: bool,
    fields: ValueArray,
}

fn materialize_pending_instances(
    machine: &mut Machine,
    activation: &mut lm_jit::NativeActivation,
    regions: &[Arc<lm_jit::CompiledRegion>],
    exit: &mut lm_jit::ExecutionExit,
) -> Result<(u64, u64), ()> {
    let count = activation.pending_instances().count();
    if count == 0 {
        return Ok((0, 0));
    }
    let object_slots = pending_object_slots(activation, regions, *exit)?;
    let mut pending_materializations = Vec::new();
    pending_materializations
        .try_reserve_exact(count)
        .map_err(|_| ())?;
    let mut released_bytes = 0usize;
    let mut releases = 0usize;
    for pending in activation.pending_instances() {
        if pending.references() == 0 {
            released_bytes = released_bytes.checked_add(pending.byte_cost()).ok_or(())?;
            releases = releases.checked_add(1).ok_or(())?;
            continue;
        }
        let mut fields =
            ValueArray::try_repeated(Value::Uninit, pending.fields().len()).map_err(|_| ())?;
        fields.as_mut_slice().copy_from_slice(pending.fields());
        pending_materializations.push(PendingInstanceMaterialization {
            token: pending.object_bits(),
            class: pending.class(),
            environment: pending.environment(),
            frozen: pending.frozen(),
            fields,
        });
    }
    if machine.vm.heap.used_bytes() < released_bytes || machine.vm.heap.live_count() < releases {
        return Err(());
    }

    let mut replacements = Vec::new();
    replacements
        .try_reserve_exact(pending_materializations.len())
        .map_err(|_| ())?;
    if !machine
        .vm
        .heap
        .reserve_precharged_slots(pending_materializations.len())
        || (releases != 0
            && !machine
                .vm
                .heap
                .release_precharged_instances(released_bytes, releases))
    {
        return Err(());
    }
    for pending in pending_materializations {
        let reference = machine.vm.heap.materialize_precharged_instance(
            pending.class,
            pending.fields,
            pending.environment,
            pending.frozen,
        );
        replacements.push((pending.token, reference));
    }
    for (token, reference) in replacements.iter().copied() {
        activation.replace_pending_reference(token, reference);
        for slot in object_slots.iter().copied() {
            activation.replace_pending_object_slot(slot, token, reference);
        }
        exit.replace_object_result(token, reference);
    }
    for (_, reference) in replacements.iter().copied() {
        let Object::Instance { fields, .. } = machine.vm.heap.get_mut(reference) else {
            unreachable!("precharged materialization creates an instance");
        };
        for value in fields.as_mut_slice() {
            let Value::Obj(held) = value else {
                continue;
            };
            let held_bits = u64::from(held.slot) | (u64::from(held.generation) << 32);
            if let Some((_, replacement)) =
                replacements.iter().find(|(token, _)| *token == held_bits)
            {
                *held = *replacement;
            }
        }
    }
    activation.clear_pending_instances();
    Ok((replacements.len() as u64, releases as u64))
}

fn pending_object_slots(
    activation: &lm_jit::NativeActivation,
    regions: &[Arc<lm_jit::CompiledRegion>],
    exit: lm_jit::ExecutionExit,
) -> Result<Vec<usize>, ()> {
    let frames: Vec<_> = activation.frames().collect();
    if frames.len() != regions.len() {
        return Err(());
    }
    let mut slots = Vec::new();
    for (index, (frame, region)) in frames.iter().zip(regions).enumerate() {
        if frame.locals().len() != region.local_kinds().len() {
            return Err(());
        }
        let local_base = frame.scalar_base();
        for (slot, (kind, tag)) in region
            .local_kinds()
            .iter()
            .copied()
            .zip(frame.local_tags().iter().copied())
            .enumerate()
        {
            if scalar_slot_holds_object(kind, tag) {
                slots.push(local_base.checked_add(slot).ok_or(())?);
            }
        }
        let top = index + 1 == frames.len();
        let operand_kinds = frame_operand_kinds(region, frame, top.then_some(exit))?;
        if operand_kinds.len() != frame.operands().len()
            || operand_kinds.len() != frame.operand_tags().len()
        {
            return Err(());
        }
        let operand_base = local_base.checked_add(frame.locals().len()).ok_or(())?;
        for (slot, (kind, tag)) in operand_kinds
            .iter()
            .copied()
            .zip(frame.operand_tags().iter().copied())
            .enumerate()
        {
            if scalar_slot_holds_object(kind, tag) {
                slots.push(operand_base.checked_add(slot).ok_or(())?);
            }
        }
    }
    Ok(slots)
}

fn scalar_slot_holds_object(kind: ScalarKind, tag: u64) -> bool {
    matches!(kind, ScalarKind::Object(_))
        || matches!(kind, ScalarKind::Tagged(_)) && tag == ValueTag::Obj as u64
}

fn continuation_regions(
    native: &NativeCodeState,
    root: &Arc<lm_jit::CompiledRegion>,
    activation: &lm_jit::NativeActivation,
    regions: &mut Vec<Arc<lm_jit::CompiledRegion>>,
) -> Result<(), ()> {
    let frame_count = activation.frame_count();
    if frame_count == 0 {
        return Err(());
    }
    let retained = activation
        .changed_from()
        .min(frame_count)
        .min(regions.len());
    regions.truncate(retained);
    regions
        .try_reserve(frame_count.saturating_sub(regions.len()))
        .map_err(|_| ())?;
    if regions.is_empty() {
        if activation
            .frames()
            .next()
            .is_none_or(|frame| frame.function() != root.function())
        {
            return Err(());
        }
        regions.push(Arc::clone(root));
    }
    for frame in activation.frames().skip(regions.len()) {
        if !frame.native_created() {
            return Err(());
        }
        let region = native
            .slot(frame.function())
            .and_then(|slot| slot.compiled())
            .ok_or(())?;
        regions.push(region);
    }
    Ok(())
}

fn extend_native_roots(
    activation: &lm_jit::NativeActivation,
    regions: &[Arc<lm_jit::CompiledRegion>],
    exit: lm_jit::ExecutionExit,
    roots: &mut Vec<ObjRef>,
) {
    let frame_count = activation.frame_count();
    for (index, (frame, region)) in activation.frames().zip(regions.iter()).enumerate() {
        if frame.locals().len() != region.local_kinds().len()
            || frame.local_tags().len() != region.local_kinds().len()
            || frame.states().len() != region.local_kinds().len()
        {
            return;
        }
        if let Some(Some(FrameCapture::Closure(reference))) =
            parts_frame_capture(frame.capture_tag(), frame.capture_bits())
        {
            roots.push(reference);
        }
        for (((kind, bits), tag), state) in region
            .local_kinds()
            .iter()
            .copied()
            .zip(frame.locals().iter().copied())
            .zip(frame.local_tags().iter().copied())
            .zip(frame.states().iter().copied())
        {
            if state & LOCAL_INITIALIZED != 0 {
                if let Some(Value::Obj(reference)) = materialized_value(kind, tag, bits) {
                    roots.push(reference);
                }
            }
        }
        let top = index + 1 == frame_count;
        let Some(kinds) = frame_operand_kinds(region, &frame, top.then_some(exit)).ok() else {
            return;
        };
        if kinds.len() != frame.operands().len() {
            return;
        }
        if frame.operand_tags().len() != kinds.len() {
            return;
        }
        for ((kind, bits), tag) in kinds
            .iter()
            .copied()
            .zip(frame.operands().iter().copied())
            .zip(frame.operand_tags().iter().copied())
        {
            if let Some(Value::Obj(reference)) = materialized_value(kind, tag, bits) {
                roots.push(reference);
            }
        }
    }
}

fn frame_operand_kinds<'a>(
    region: &'a lm_jit::CompiledRegion,
    frame: &lm_jit::NativeFrameView<'_>,
    exit: Option<lm_jit::ExecutionExit>,
) -> Result<&'a [ScalarKind], ()> {
    let kinds = match exit {
        None => region.suspended_operand_kinds(frame.block(), frame.instruction()),
        Some(exit) => {
            region.materialization_operand_kinds(exit.kind(), frame.block(), frame.instruction())
        }
    };
    kinds.ok_or(())
}

pub(crate) fn materialize_native_continuation(machine: &mut Machine) -> Result<bool, ()> {
    let Some(continuation) = machine.take_native_continuation() else {
        return Ok(false);
    };
    let continuation = *continuation;
    let effect = continuation.effect;
    continuation.canonical.restore(machine);
    materialize_native_frames(
        machine,
        &continuation.scratch.continuation_regions,
        &continuation.scratch.activation,
        continuation.exit,
        continuation.root_frame,
        continuation.root_local,
        continuation.root_operand,
    )?;
    if let Some(effect) = effect {
        finish_materialized_effect(machine, effect)?;
    }
    Ok(true)
}

fn finish_materialized_effect(
    machine: &mut Machine,
    effect: NativeEffectContinuation,
) -> Result<(), ()> {
    let frame = machine.vm.frames.last().ok_or(())?;
    if frame.block != effect.block
        || frame.ip.checked_add(1) != Some(effect.instruction)
        || machine.vm.operands.len() < effect.consumed
    {
        return Err(());
    }
    let operand_len = machine.vm.operands.len() - effect.consumed;
    if operand_len < frame.base_operand as usize {
        return Err(());
    }
    machine.vm.operands.truncate(operand_len);
    let frame = machine.vm.frames.last_mut().ok_or(())?;
    frame.ip = effect.instruction;
    Ok(())
}

fn reject_native_continuation(
    machine: &mut Machine,
    _native: &NativeCodeState,
    continuation: NativeContinuation,
    metrics: &mut EngineTurnMetrics<'_>,
) -> NativeAttempt {
    metrics.note_backend_unavailable();
    metrics.note_native_fault_exit();
    continuation.canonical.restore(machine);
    if materialize_native_frames(
        machine,
        &continuation.scratch.continuation_regions,
        &continuation.scratch.activation,
        continuation.exit,
        continuation.root_frame,
        continuation.root_local,
        continuation.root_operand,
    )
    .is_ok()
    {
        metrics.note_materialization();
        metrics.note_native_continuation_materialization();
    }
    malformed_native_exit(0)
}

fn materialize_native_frames(
    machine: &mut Machine,
    regions: &[Arc<lm_jit::CompiledRegion>],
    activation: &lm_jit::NativeActivation,
    exit: lm_jit::ExecutionExit,
    root_index: usize,
    root_base: usize,
    root_operand_base: usize,
) -> Result<Option<Arc<lm_jit::CompiledRegion>>, ()> {
    let frames: Vec<_> = activation.frames().collect();
    let root = frames.first().ok_or(())?;
    let root_region = regions.first().ok_or(())?;
    if regions.len() != frames.len() {
        return Err(());
    }
    if root.native_created() || root.locals().len() != root_region.local_kinds().len() {
        return Err(());
    }
    let Some(canonical_root) = machine.vm.frames.get(root_index) else {
        return Err(());
    };
    let root_capture = parts_frame_capture(root.capture_tag(), root.capture_bits()).ok_or(())?;
    if canonical_root.func != root.function()
        || canonical_root.env.0 != root.environment()
        || canonical_root.closure != root_capture
    {
        return Err(());
    }
    if frames.iter().skip(1).any(|frame| !frame.native_created()) {
        return Err(());
    }
    machine.vm.frames.truncate(root_index + 1);
    machine
        .vm
        .locals
        .truncate(root_base + root_region.local_kinds().len());
    machine.vm.operands.truncate(root_operand_base);

    for (index, frame) in frames.iter().enumerate() {
        let region = if index == 0 {
            root_region
        } else {
            regions[index].as_ref()
        };
        if frame.locals().len() != region.local_kinds().len()
            || frame.local_tags().len() != region.local_kinds().len()
            || frame.states().len() != region.local_kinds().len()
        {
            return Err(());
        }
        let top = index + 1 == frames.len();
        let operand_kinds = frame_operand_kinds(region, frame, top.then_some(exit))?;
        if operand_kinds.len() != frame.operands().len() {
            return Err(());
        }

        if index == 0 {
            for (slot, state) in frame.states().iter().copied().enumerate() {
                if state & LOCAL_INITIALIZED != 0 {
                    machine.vm.locals[root_base + slot] = materialized_value(
                        region.local_kinds()[slot],
                        frame.local_tags()[slot],
                        frame.locals()[slot],
                    )
                    .ok_or(())?;
                }
            }
            let canonical = machine.vm.frames.get_mut(root_index).ok_or(())?;
            canonical.block = frame.block();
            canonical.ip = frame.instruction();
        } else {
            let base_local = u32::try_from(machine.vm.locals.len()).map_err(|_| ())?;
            let base_operand = u32::try_from(machine.vm.operands.len()).map_err(|_| ())?;
            for (((kind, bits), tag), state) in region
                .local_kinds()
                .iter()
                .copied()
                .zip(frame.locals().iter().copied())
                .zip(frame.local_tags().iter().copied())
                .zip(frame.states().iter().copied())
            {
                let value = if state & LOCAL_INITIALIZED == 0 {
                    Value::Uninit
                } else {
                    materialized_value(kind, tag, bits).ok_or(())?
                };
                machine.vm.locals.push(value);
            }
            machine.vm.frames.push(Frame {
                func: frame.function(),
                block: frame.block(),
                ip: frame.instruction(),
                base_local,
                base_operand,
                closure: parts_frame_capture(frame.capture_tag(), frame.capture_bits()).ok_or(())?,
                env: TypeEnvId(frame.environment()),
            });
        }
        if frame.operand_tags().len() != operand_kinds.len() {
            return Err(());
        }
        for ((kind, bits), tag) in operand_kinds
            .iter()
            .copied()
            .zip(frame.operands().iter().copied())
            .zip(frame.operand_tags().iter().copied())
        {
            machine
                .vm
                .operands
                .push(materialized_value(kind, tag, bits).ok_or(())?);
        }
    }
    let top = frames.last().ok_or(())?;
    if top.block() != exit.block()
        || top.instruction() != exit.instruction()
        || top.operands().len() != exit.stack_len() as usize
    {
        return Err(());
    }
    Ok(regions
        .get(1..)
        .and_then(|children| children.last())
        .cloned())
}

struct NativeEffectRequest {
    op: u32,
    args: Vec<Value>,
    continuation: NativeEffectContinuation,
}

fn native_effect_request(
    module: &NamespaceRuntime,
    region: &lm_jit::CompiledRegion,
    activation: &lm_jit::NativeActivation,
    exit: lm_jit::ExecutionExit,
) -> Option<NativeEffectRequest> {
    let frame = activation.top_frame()?;
    if frame.block() != exit.block()
        || frame.instruction() != exit.instruction()
        || frame.operands().len() != exit.stack_len() as usize
    {
        return None;
    }
    let instruction = module
        .funcs
        .get(frame.function() as usize)?
        .blocks
        .get(exit.block() as usize)?
        .get(exit.instruction() as usize)?;
    let (fixed_op, argc, reply_ty, dynamic) = match instruction {
        lm_bytecode::Instr::Perform { op, argc, reply_ty } => {
            (Some(*op), *argc as usize, *reply_ty, false)
        }
        lm_bytecode::Instr::PerformValue { argc, reply_ty } => {
            (None, *argc as usize, *reply_ty, true)
        }
        _ => return None,
    };
    let kinds =
        region.materialization_operand_kinds(ExitKind::Effect, exit.block(), exit.instruction())?;
    if kinds.len() != frame.operands().len() || kinds.len() != frame.operand_tags().len() {
        return None;
    }
    let consumed = argc.checked_add(usize::from(dynamic))?;
    let prefix = kinds.len().checked_sub(consumed)?;
    let args_at = kinds.len().checked_sub(argc)?;
    let mut args = Vec::new();
    args.try_reserve_exact(argc).ok()?;
    for ((kind, bits), tag) in kinds[args_at..]
        .iter()
        .copied()
        .zip(frame.operands()[args_at..].iter().copied())
        .zip(frame.operand_tags()[args_at..].iter().copied())
    {
        args.push(materialized_value(kind, tag, bits)?);
    }
    let op = match fixed_op {
        Some(op) => op,
        None => match materialized_value(
            *kinds.get(prefix)?,
            *frame.operand_tags().get(prefix)?,
            *frame.operands().get(prefix)?,
        )? {
            Value::Op(op) => op,
            _ => return None,
        },
    };
    let instruction = exit.instruction().checked_add(1)?;
    let entry = region.resume_plan(exit.block(), instruction)?;
    if entry.operand_kinds().len() != prefix.checked_add(1)? {
        return None;
    }
    let reply_kind = *entry.operand_kinds().last()?;
    Some(NativeEffectRequest {
        op,
        args,
        continuation: NativeEffectContinuation {
            reply_ty,
            environment: TypeEnvId(frame.environment()),
            consumed,
            block: exit.block(),
            instruction,
            reply_kind,
        },
    })
}

fn materialized_value(kind: ScalarKind, stored_tag: u64, bits: u64) -> Option<Value> {
    let tag = match kind {
        ScalarKind::Unit => ValueTag::Unit,
        ScalarKind::Bool => ValueTag::Bool,
        ScalarKind::Int => ValueTag::Int,
        ScalarKind::Float => ValueTag::Float,
        ScalarKind::Char => ValueTag::Char,
        ScalarKind::Object(_) => ValueTag::Obj,
        ScalarKind::Tagged(_) | ScalarKind::Callback(_) => {
            return parts_value(kind, stored_tag, bits);
        }
        ScalarKind::Operation => ValueTag::Op,
    };
    parts_value(kind, tag as u64, bits)
}

fn malformed_native_exit(retired: u32) -> NativeAttempt {
    NativeAttempt::Complete {
        outcome: Err(ExecError::Fault(crate::FaultCode::MalformedState)),
        retired,
    }
}

fn malformed_native_execution(
    machine: &mut Machine,
    original_fuel: u64,
    reported: u64,
    instruction_limit: u32,
) -> NativeAttempt {
    let retired = reported
        .min(original_fuel)
        .min(u64::from(instruction_limit)) as u32;
    machine.vm.fuel = original_fuel - u64::from(retired);
    malformed_native_exit(retired)
}

mod runtime;

use runtime::{parts_value, scalar_parts, MachineRuntime};
