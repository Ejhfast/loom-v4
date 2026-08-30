//! Canonical machine-state adapter for native execution.

use crate::engine::EngineTurnMetrics;
use crate::machine::{ExecError, ExecOutcome, Frame, Machine};
use crate::NamespaceRuntime;
use lm_jit::{
    ExitKind, Failure, FunctionInput, NativeExecution, NativePreparation, ScalarKind, LOCAL_DIRTY,
    LOCAL_INITIALIZED,
};
use lm_value::{ObjRef, TypeEnvId, Value};
use std::sync::{Arc, Mutex, Weak};

/// Reusable scalar buffers for one engine turn.
#[derive(Default)]
pub(crate) struct NativeScratch {
    activation: lm_jit::NativeActivation,
    roots: Vec<u64>,
    root_tags: Vec<u64>,
    root_states: Vec<u8>,
    continuation_regions: Vec<Arc<lm_jit::CompiledRegion>>,
}

/// Mutable runtime data used during one native entry attempt.
pub(crate) struct NativeExecutionContext<'a> {
    pub(crate) module: &'a NamespaceRuntime,
    pub(crate) envs: &'a mut lm_bytecode::closed::TypeEnvs,
}

/// One native activation retained at an ordinary scheduler quantum.
pub(crate) struct NativeContinuation {
    scratch: NativeScratch,
    canonical: CanonicalStack,
    root_frame: usize,
    root_local: usize,
    root_operand: usize,
    exit: lm_jit::ExecutionExit,
}

#[derive(Default)]
struct CanonicalStack {
    frames: Vec<Frame>,
    locals: Vec<Value>,
    operands: Vec<Value>,
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
                instruction: if next_top && depth == 0 {
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
    Reenter {
        retired: u32,
    },
    Complete {
        outcome: Result<Option<ExecOutcome>, ExecError>,
        retired: u32,
    },
}

/// One host-owned native compiler and immutable region cache.
#[derive(Default)]
pub(crate) struct JitEngine {
    compiler: lm_jit::JitEngine,
    layouts:
        Mutex<std::collections::HashMap<usize, (Weak<lm_bytecode::CodeTables>, NativeCodeState)>>,
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
        if let Some((known_tables, known_state)) = layouts.get(&key) {
            if known_tables
                .upgrade()
                .is_some_and(|known| Arc::ptr_eq(&known, &tables))
            {
                return known_state.clone();
            }
        }
        if layouts.len() >= 256 {
            layouts.retain(|_, (tables, _)| tables.strong_count() != 0);
        }
        let state = NativeCodeState::new(module);
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
    }

    pub(crate) fn execute(
        &self,
        machine: &mut Machine,
        context: &mut NativeExecutionContext<'_>,
        native: &NativeCodeState,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> NativeAttempt {
        if machine.has_native_continuation() {
            return Self::execute_region(
                machine,
                context,
                native,
                None,
                scratch,
                metrics,
                instruction_limit,
            );
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
        let region = match slot.region(&self.compiler, native.compiled_count(), || {
            let runtime = context
                .module
                .funcs
                .get(function as usize)
                .ok_or(Failure::Unsupported)?;
            let (unit, local) = context
                .module
                .code_namespace()
                .function_unit(function)
                .map_err(|_| Failure::Unsupported)?;
            let mut input = FunctionInput::new(
                function,
                runtime,
                unit.module(),
                context.module.bundle(),
                local,
            );
            input.set_runtime_string_count(context.module.strings.len());
            let relocation = context
                .module
                .code_namespace()
                .relocation(unit.id())
                .ok_or(Failure::Unsupported)?;
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
            for callee in callees {
                let callee_runtime = context
                    .module
                    .funcs
                    .get(callee as usize)
                    .ok_or(Failure::Unsupported)?;
                let (callee_unit, callee_local) = context
                    .module
                    .code_namespace()
                    .function_unit(callee)
                    .map_err(|_| Failure::Unsupported)?;
                let callee_relocation = context
                    .module
                    .code_namespace()
                    .relocation(callee_unit.id())
                    .ok_or(Failure::Unsupported)?;
                input.add_relocated_direct_callee(
                    callee,
                    callee_runtime,
                    callee_unit.module(),
                    context.module.bundle(),
                    callee_local,
                    callee_relocation.classes(),
                );
            }
            Ok(input)
        }) {
            Ok(region) => region,
            Err(Failure::Unsupported) => {
                metrics.note_unsupported_region_fallback();
                return NativeAttempt::Fallback;
            }
            Err(Failure::BackendUnavailable) => {
                metrics.note_backend_unavailable();
                return NativeAttempt::Fallback;
            }
        };
        Self::execute_region(
            machine,
            context,
            native,
            Some(region),
            scratch,
            metrics,
            instruction_limit,
        )
    }

    fn execute_region(
        machine: &mut Machine,
        context: &mut NativeExecutionContext<'_>,
        native: &NativeCodeState,
        region: Option<Arc<lm_jit::CompiledRegion>>,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> NativeAttempt {
        let module = context.module;
        let mut continuation = machine.take_native_continuation();
        let resumed = continuation.is_some();
        let (root_region, active_region, entry_index, root_frame, base, operand_base) =
            if let Some(mut held) = continuation.take() {
                let Some(top) = held.scratch.activation.frames().last() else {
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
                if frame.closure.is_some() {
                    metrics.note_missing_entry_fallback();
                    return NativeAttempt::Fallback;
                }
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
        let (exit, allocations) = {
            let type_environments = std::mem::take(&mut machine.native_type_environments);
            let mut runtime = MachineRuntime {
                machine,
                type_environments,
                module,
                base_local: base,
                base_operand: operand_base,
                allocations: 0,
            };
            let mut active_region = active_region;
            let mut active_entry = entry_index;
            let mut prior_retired = 0u64;
            let result = loop {
                let root_capacity = active_region.max_roots().max(1);
                scratch.roots.resize(root_capacity, 0);
                scratch.roots.fill(0);
                scratch.root_tags.resize(root_capacity, 0);
                scratch.root_tags.fill(0);
                scratch.root_states.resize(root_capacity, 0);
                scratch.root_states.fill(0);
                let Some(remaining) = batch_fuel.checked_sub(prior_retired) else {
                    break Err(Failure::BackendUnavailable);
                };
                let type_environments = match runtime.type_environments.view() {
                    Ok(view) => view,
                    Err(error) => break Err(error),
                };
                let heap = runtime.machine.vm.heap.jit_view();
                let exit = match active_region.execute(
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
                        heap,
                        class_parents: native.class_parents(),
                        literals,
                        type_store_id: context.envs.canonical_store_id(),
                        type_environments,
                    },
                ) {
                    Ok(exit) => exit,
                    Err(error) => break Err(error),
                };
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
                        .frames()
                        .last()
                        .and_then(|frame| native.slot(frame.function()))
                        .and_then(|slot| slot.compiled());
                    let resume = grow_region
                        .as_deref()
                        .and_then(|region| region.resume_plan(exit.block(), exit.instruction()))
                        .map(|entry| entry.index());
                    if let (Some(grow_region), Some(resume)) = (grow_region, resume) {
                        let grew = scratch.activation.grow(
                            required_scalars,
                            required_frames,
                            max_stack_values,
                            max_frames,
                        );
                        if matches!(grew, Ok(true)) {
                            prior_retired = next_retired;
                            active_region = grow_region;
                            active_entry = resume;
                            metrics.note_native_activation_grow();
                            continue;
                        }
                    }
                }
                if exit.kind() == ExitKind::TypeResolution {
                    let type_index = exit.result() & u64::from(u32::MAX);
                    let Ok(type_index) = u32::try_from(type_index) else {
                        break Err(Failure::BackendUnavailable);
                    };
                    let Some((function, environment)) = scratch
                        .activation
                        .frames()
                        .last()
                        .map(|frame| (frame.function(), TypeEnvId(frame.environment())))
                    else {
                        break Err(Failure::BackendUnavailable);
                    };
                    if exit.result_tag() != u64::from(environment.0) {
                        break Err(Failure::BackendUnavailable);
                    }
                    let family = runtime.machine.close_option_family_at(
                        module,
                        context.envs,
                        type_index,
                        environment,
                    );
                    if let Ok(family) = family {
                        let Some(next_retired) = prior_retired.checked_add(exit.retired()) else {
                            break Err(Failure::BackendUnavailable);
                        };
                        let resolve_region = scratch
                            .activation
                            .frames()
                            .last()
                            .and_then(|frame| native.slot(frame.function()))
                            .and_then(|slot| slot.compiled());
                        let resume = resolve_region
                            .as_deref()
                            .and_then(|region| region.resume_plan(exit.block(), exit.instruction()))
                            .map(|entry| entry.index());
                        if let (Some(resolve_region), Some(resume)) = (resolve_region, resume) {
                            let cached = runtime.type_environments.cache_type_site(
                                context.envs.canonical_store_id(),
                                function,
                                exit.block(),
                                exit.instruction(),
                                environment.0,
                                family,
                            );
                            if cached {
                                prior_retired = next_retired;
                                active_region = resolve_region;
                                active_entry = resume;
                                continue;
                            }
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
                    if let Ok(child) = context.envs.derive(module, parent, application) {
                        let cached = runtime.type_environments.cache_type_site(
                            context.envs.canonical_store_id(),
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
                            let Some(resume) = resolve_region
                                .resume_plan(exit.block(), exit.instruction())
                                .map(|entry| entry.index())
                            else {
                                break Err(Failure::BackendUnavailable);
                            };
                            prior_retired = next_retired;
                            active_region = resolve_region;
                            active_entry = resume;
                            continue;
                        }
                        metrics.note_native_type_environment_fallback();
                    }
                }
                if exit.kind() != ExitKind::Return || scratch.activation.frame_count() <= 1 {
                    break exit.add_prior_retired(prior_retired);
                }
                let parent = {
                    let parent_index = scratch.activation.frame_count().saturating_sub(2);
                    let Some(parent) = scratch.activation.frames().nth(parent_index) else {
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
            (result, runtime.allocations)
        };
        metrics.note_native_allocations(allocations);
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
        if metrics.sample_productivity() {
            let sample = match exit.kind() {
                ExitKind::Fuel
                | ExitKind::Return
                | ExitKind::Interpreter
                | ExitKind::Replay
                | ExitKind::Allocation
                | ExitKind::Effect => true,
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
            | ExitKind::Interpreter
            | ExitKind::Replay
            | ExitKind::Literal
            | ExitKind::Call
            | ExitKind::GrowActivation
            | ExitKind::TypeResolution
            | ExitKind::TypeEnvironment
            | ExitKind::Allocation
            | ExitKind::Effect => {
                let interpreter = matches!(
                    exit.kind(),
                    ExitKind::Interpreter | ExitKind::Replay | ExitKind::Literal
                );
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
                    ExitKind::Allocation
                        | ExitKind::Effect
                        | ExitKind::TypeResolution
                        | ExitKind::TypeEnvironment
                ) {
                    if matches!(exit.kind(), ExitKind::Allocation) {
                        metrics.note_native_allocation_exit();
                    }
                    if matches!(exit.kind(), ExitKind::Effect) {
                        metrics.note_native_effect_exit();
                    }
                    NativeAttempt::InterpretOne { retired }
                } else if interpreter {
                    metrics.note_native_interpreter_exit();
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
            | ExitKind::StackLimit => {
                metrics.note_native_fault_exit();
                let fault = match exit.kind() {
                    ExitKind::IntegerOverflow => crate::FaultCode::IntegerOverflow,
                    ExitKind::DivideByZero => crate::FaultCode::DivideByZero,
                    ExitKind::TypeMismatch => crate::FaultCode::TypeMismatch,
                    ExitKind::UninitializedField => crate::FaultCode::UninitializedField,
                    ExitKind::HeapLimit => crate::FaultCode::HeapLimit,
                    ExitKind::Unreachable => crate::FaultCode::UnreachableCode,
                    ExitKind::StackLimit => crate::FaultCode::StackLimit,
                    _ => unreachable!(),
                };
                NativeAttempt::Complete {
                    outcome: Err(ExecError::Fault(fault)),
                    retired,
                }
            }
        }
    }
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
        for (((kind, bits), tag), state) in region
            .local_kinds()
            .iter()
            .copied()
            .zip(frame.locals().iter().copied())
            .zip(frame.local_tags().iter().copied())
            .zip(frame.states().iter().copied())
        {
            if state & LOCAL_INITIALIZED != 0 {
                if let Some(Value::Obj(reference)) = parts_value(kind, tag, bits) {
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
            if let Some(Value::Obj(reference)) = parts_value(kind, tag, bits) {
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
    let kinds = match exit.map(|exit| exit.kind()) {
        None => region.suspended_operand_kinds(frame.block(), frame.instruction()),
        Some(ExitKind::Return) => Some(&[][..]),
        Some(ExitKind::Replay) => region.replay_operand_kinds(frame.block(), frame.instruction()),
        Some(
            ExitKind::IntegerOverflow
            | ExitKind::DivideByZero
            | ExitKind::TypeMismatch
            | ExitKind::UninitializedField
            | ExitKind::HeapLimit
            | ExitKind::Unreachable,
        ) => region.fault_operand_kinds(frame.block(), frame.instruction()),
        Some(ExitKind::StackLimit) => frame
            .instruction()
            .checked_sub(1)
            .and_then(|instruction| region.operand_kinds(frame.block(), instruction)),
        Some(_) => region.operand_kinds(frame.block(), frame.instruction()),
    };
    kinds.ok_or(())
}

pub(crate) fn materialize_native_continuation(machine: &mut Machine) -> Result<bool, ()> {
    let Some(continuation) = machine.take_native_continuation() else {
        return Ok(false);
    };
    let continuation = *continuation;
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
    Ok(true)
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
    if machine
        .vm
        .frames
        .get(root_index)
        .is_none_or(|frame| frame.func != root.function() || frame.env.0 != root.environment())
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
                if state & LOCAL_DIRTY != 0 {
                    machine.vm.locals[root_base + slot] = parts_value(
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
                    parts_value(kind, tag, bits).ok_or(())?
                };
                machine.vm.locals.push(value);
            }
            machine.vm.frames.push(Frame {
                func: frame.function(),
                block: frame.block(),
                ip: frame.instruction(),
                base_local,
                base_operand,
                closure: None,
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
                .push(parts_value(kind, tag, bits).ok_or(())?);
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
