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
    root_states: Vec<u8>,
    continuation_regions: Vec<Arc<lm_jit::CompiledRegion>>,
    continuation_roots: Vec<ObjRef>,
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
        roots.extend_from_slice(&self.scratch.continuation_roots);
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

    pub(crate) fn execute(
        &self,
        machine: &mut Machine,
        module: &NamespaceRuntime,
        native: &NativeCodeState,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> NativeAttempt {
        if machine.has_native_continuation() {
            return Self::execute_region(
                machine,
                module,
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
            let runtime = module
                .funcs
                .get(function as usize)
                .ok_or(Failure::Unsupported)?;
            let (unit, local) = module
                .code_namespace()
                .function_unit(function)
                .map_err(|_| Failure::Unsupported)?;
            let mut input =
                FunctionInput::new(function, runtime, unit.module(), module.bundle(), local);
            let relocation = module
                .code_namespace()
                .relocation(unit.id())
                .ok_or(Failure::Unsupported)?;
            input.set_class_relocation(relocation.classes());
            let mut callees = Vec::new();
            for instruction in runtime.blocks.iter().flatten() {
                if let lm_bytecode::Instr::Call(callee) = instruction {
                    if !callees.contains(callee) {
                        callees.push(*callee);
                    }
                }
            }
            for callee in callees {
                let callee_runtime = module
                    .funcs
                    .get(callee as usize)
                    .ok_or(Failure::Unsupported)?;
                let (callee_unit, callee_local) = module
                    .code_namespace()
                    .function_unit(callee)
                    .map_err(|_| Failure::Unsupported)?;
                let callee_relocation = module
                    .code_namespace()
                    .relocation(callee_unit.id())
                    .ok_or(Failure::Unsupported)?;
                input.add_relocated_direct_callee(
                    callee,
                    callee_runtime,
                    callee_unit.module(),
                    module.bundle(),
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
            module,
            native,
            Some(region),
            scratch,
            metrics,
            instruction_limit,
        )
    }

    fn execute_region(
        machine: &mut Machine,
        module: &NamespaceRuntime,
        native: &NativeCodeState,
        region: Option<Arc<lm_jit::CompiledRegion>>,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> NativeAttempt {
        let continuation = machine.take_native_continuation();
        let resumed = continuation.is_some();
        let (root_region, active_region, entry_index, root_frame, base, operand_base) =
            if let Some(continuation) = continuation {
                let continuation = *continuation;
                let Some(top) = continuation.scratch.activation.frames().last() else {
                    return reject_native_continuation(machine, native, continuation, metrics);
                };
                let top_block = top.block();
                let top_instruction = top.instruction();
                let Some(active_region) = continuation.scratch.continuation_regions.last().cloned()
                else {
                    return reject_native_continuation(machine, native, continuation, metrics);
                };
                let Some(root_region) = continuation.scratch.continuation_regions.first().cloned()
                else {
                    return reject_native_continuation(machine, native, continuation, metrics);
                };
                if continuation.scratch.continuation_regions.len()
                    != continuation.scratch.activation.frame_count()
                {
                    return reject_native_continuation(machine, native, continuation, metrics);
                }
                let Some(entry) = active_region.resume_plan(top_block, top_instruction) else {
                    return reject_native_continuation(machine, native, continuation, metrics);
                };
                let entry_index = entry.index();
                let NativeContinuation {
                    scratch: continuation_scratch,
                    canonical,
                    root_frame,
                    root_local,
                    root_operand,
                    ..
                } = continuation;
                canonical.restore(machine);
                *scratch = continuation_scratch;
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
                if frame.closure.is_some() || frame.env != TypeEnvId::EMPTY {
                    metrics.note_missing_entry_fallback();
                    return NativeAttempt::Fallback;
                }
                let Some(required_frames) =
                    (machine.vm.frames.len() as u32).checked_add(region.additional_frames())
                else {
                    metrics.note_guard_failure(0);
                    return NativeAttempt::Fallback;
                };
                if required_frames > machine.config.max_frames {
                    metrics.note_guard_failure(0);
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
                if scratch
                    .activation
                    .prepare_root(NativePreparation {
                        function: frame.func,
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
                let (bits, local_states, stack_bits) = scratch.activation.root_buffers_mut();
                for (slot, (kind, live)) in region
                    .local_kinds()
                    .iter()
                    .copied()
                    .zip(entry.live_locals().iter().copied())
                    .enumerate()
                {
                    let complete_root =
                        region.requires_complete_roots() && matches!(kind, ScalarKind::Object(_));
                    if !live && !complete_root {
                        continue;
                    }
                    if !live && locals[slot] == Value::Uninit {
                        continue;
                    }
                    guarded += 1;
                    let Some(value) = scalar_bits(kind, locals[slot]) else {
                        metrics.note_guard_failure(guarded);
                        return NativeAttempt::Fallback;
                    };
                    bits[slot] = value;
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
                    let Some(value) = scalar_bits(kind, value) else {
                        metrics.note_guard_failure(guarded);
                        return NativeAttempt::Fallback;
                    };
                    stack_bits[slot] = value;
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
        let (exit, exit_region, allocations) = {
            let mut runtime = MachineRuntime {
                machine,
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
                scratch.root_states.resize(root_capacity, 0);
                scratch.root_states.fill(0);
                let Some(remaining) = batch_fuel.checked_sub(prior_retired) else {
                    break Err(Failure::BackendUnavailable);
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
                        root_states: &mut scratch.root_states,
                        fuel: remaining,
                        heap,
                    },
                ) {
                    Ok(exit) => exit,
                    Err(error) => break Err(error),
                };
                if exit.kind() != ExitKind::Return || scratch.activation.frame_count() <= 1 {
                    break exit
                        .add_prior_retired(prior_retired)
                        .map(|exit| (exit, active_region));
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
                    .finish_detached_return(exit.result())
                    .is_err()
                {
                    break Err(Failure::BackendUnavailable);
                }
                prior_retired = next_retired;
                active_region = parent_region;
                active_entry = parent_entry;
            };
            match result {
                Ok((exit, region)) => (Ok(exit), Some(region), runtime.allocations),
                Err(error) => (Err(error), None, runtime.allocations),
            }
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
        machine.vm.fuel -= exit.retired();
        let retain_continuation = exit.kind() == ExitKind::Fuel
            && exit.retired() == batch_fuel
            && batch_fuel == u64::from(instruction_limit)
            && u64::from(instruction_limit) <= original_fuel;
        if retain_continuation {
            let Ok(()) = continuation_metadata(
                native,
                Arc::clone(&root_region),
                &scratch.activation,
                exit,
                &mut scratch.continuation_regions,
                &mut scratch.continuation_roots,
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
            let canonical = CanonicalStack::take(machine);
            machine.set_native_continuation(NativeContinuation {
                scratch: std::mem::take(scratch),
                canonical,
                root_frame,
                root_local: base,
                root_operand: operand_base,
                exit,
            });
            metrics.note_native_retired(retired as u64);
            metrics.note_native_continuation_suspend();
            return NativeAttempt::Complete {
                outcome: Ok(None),
                retired,
            };
        }
        if continuation_regions(
            native,
            Arc::clone(&root_region),
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
        let top_region = top_child
            .as_deref()
            .or(exit_region.as_deref())
            .unwrap_or(root_region.as_ref());
        metrics.note_native_retired(retired as u64);
        metrics.note_materialization();
        if resumed {
            metrics.note_native_continuation_materialization();
        }

        match exit.kind() {
            ExitKind::Fuel
            | ExitKind::Interpreter
            | ExitKind::Call
            | ExitKind::Allocation
            | ExitKind::Effect => {
                let interpreter = matches!(exit.kind(), ExitKind::Interpreter);
                if matches!(exit.kind(), ExitKind::Call) {
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
                    let Ok(target) = u32::try_from(exit.result()) else {
                        return malformed_native_exit(retired);
                    };
                    machine.vm.fuel -= 1;
                    let retired = retired + 1;
                    metrics.note_native_retired(1);
                    return match machine.start_native_direct_call(module, target) {
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
                if matches!(exit.kind(), ExitKind::Allocation | ExitKind::Effect) {
                    if matches!(exit.kind(), ExitKind::Allocation) {
                        metrics.note_native_allocation_exit();
                    }
                    if matches!(exit.kind(), ExitKind::Effect) {
                        metrics.note_native_effect_exit();
                    }
                    NativeAttempt::InterpretOne { retired }
                } else if interpreter {
                    NativeAttempt::Continue { retired }
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
                let value = bits_value(top_region.result_kind(), exit.result());
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
            | ExitKind::StackLimit => {
                metrics.note_native_fault_exit();
                let fault = match exit.kind() {
                    ExitKind::IntegerOverflow => crate::FaultCode::IntegerOverflow,
                    ExitKind::DivideByZero => crate::FaultCode::DivideByZero,
                    ExitKind::TypeMismatch => crate::FaultCode::TypeMismatch,
                    ExitKind::UninitializedField => crate::FaultCode::UninitializedField,
                    ExitKind::HeapLimit => crate::FaultCode::HeapLimit,
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
    root: Arc<lm_jit::CompiledRegion>,
    activation: &lm_jit::NativeActivation,
    regions: &mut Vec<Arc<lm_jit::CompiledRegion>>,
) -> Result<(), ()> {
    let frame_count = activation.frame_count();
    if frame_count == 0 {
        return Err(());
    }
    regions.clear();
    regions.try_reserve(frame_count).map_err(|_| ())?;
    regions.push(root);
    for frame in activation.frames().skip(1) {
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

fn continuation_metadata(
    native: &NativeCodeState,
    root: Arc<lm_jit::CompiledRegion>,
    activation: &lm_jit::NativeActivation,
    exit: lm_jit::ExecutionExit,
    regions: &mut Vec<Arc<lm_jit::CompiledRegion>>,
    roots: &mut Vec<ObjRef>,
) -> Result<(), ()> {
    continuation_regions(native, root, activation, regions)?;
    let frame_count = activation.frame_count();
    roots.clear();
    for (index, (frame, region)) in activation.frames().zip(regions.iter()).enumerate() {
        if frame.locals().len() != region.local_kinds().len()
            || frame.states().len() != region.local_kinds().len()
        {
            return Err(());
        }
        for ((kind, bits), state) in region
            .local_kinds()
            .iter()
            .copied()
            .zip(frame.locals().iter().copied())
            .zip(frame.states().iter().copied())
        {
            if state & LOCAL_INITIALIZED != 0 {
                if let Value::Obj(reference) = bits_value(kind, bits) {
                    roots.push(reference);
                }
            }
        }
        let top = index + 1 == frame_count;
        let kinds = frame_operand_kinds(region, &frame, top.then_some(exit))?;
        if kinds.len() != frame.operands().len() {
            return Err(());
        }
        for (kind, bits) in kinds.iter().copied().zip(frame.operands().iter().copied()) {
            if let Value::Obj(reference) = bits_value(kind, bits) {
                roots.push(reference);
            }
        }
    }
    Ok(())
}

fn frame_operand_kinds<'a>(
    region: &'a lm_jit::CompiledRegion,
    frame: &lm_jit::NativeFrameView<'_>,
    exit: Option<lm_jit::ExecutionExit>,
) -> Result<&'a [ScalarKind], ()> {
    let kinds = match exit.map(|exit| exit.kind()) {
        None => region.suspended_operand_kinds(frame.block(), frame.instruction()),
        Some(ExitKind::Return) => Some(&[][..]),
        Some(
            ExitKind::IntegerOverflow
            | ExitKind::DivideByZero
            | ExitKind::TypeMismatch
            | ExitKind::UninitializedField
            | ExitKind::HeapLimit,
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
        .is_none_or(|frame| frame.func != root.function())
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
                    machine.vm.locals[root_base + slot] =
                        bits_value(region.local_kinds()[slot], frame.locals()[slot]);
                }
            }
            let canonical = machine.vm.frames.get_mut(root_index).ok_or(())?;
            canonical.block = frame.block();
            canonical.ip = frame.instruction();
        } else {
            let base_local = u32::try_from(machine.vm.locals.len()).map_err(|_| ())?;
            let base_operand = u32::try_from(machine.vm.operands.len()).map_err(|_| ())?;
            machine.vm.locals.extend(
                region
                    .local_kinds()
                    .iter()
                    .copied()
                    .zip(frame.locals().iter().copied())
                    .zip(frame.states().iter().copied())
                    .map(|((kind, bits), state)| {
                        if state & LOCAL_INITIALIZED == 0 {
                            Value::Uninit
                        } else {
                            bits_value(kind, bits)
                        }
                    }),
            );
            machine.vm.frames.push(Frame {
                func: frame.function(),
                block: frame.block(),
                ip: frame.instruction(),
                base_local,
                base_operand,
                closure: None,
                env: TypeEnvId::EMPTY,
            });
        }
        machine.vm.operands.extend(
            operand_kinds
                .iter()
                .copied()
                .zip(frame.operands().iter().copied())
                .map(|(kind, bits)| bits_value(kind, bits)),
        );
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

use runtime::{bits_value, scalar_bits, MachineRuntime};
