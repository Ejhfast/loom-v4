//! Canonical machine-state adapter for native execution.

use crate::engine::EngineTurnMetrics;
use crate::machine::{ExecError, ExecOutcome, Frame, Machine};
use crate::NamespaceRuntime;
use lm_jit::{
    ExitKind, Failure, FunctionInput, NativeExecution, NativePreparation, Runtime, RuntimeResult,
    ScalarKind, ValueRepr, LOCAL_DIRTY, LOCAL_INITIALIZED,
};
use lm_value::{canonical_float_bits, ObjRef, TypeEnvId, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

const MAX_COMPILED_REGIONS: usize = 256;

/// Reusable scalar buffers for one engine turn.
#[derive(Default)]
pub(crate) struct NativeScratch {
    activation: lm_jit::NativeActivation,
    roots: Vec<u64>,
    root_states: Vec<u8>,
}

#[derive(Clone)]
pub(crate) struct NativeCodeState(Arc<NativeCodeRevision>);

#[derive(Clone)]
struct NativeCodeRevision {
    slots: lm_bytecode::CodeTable<Arc<NativeSlot>>,
    entries: Arc<Vec<usize>>,
    compiled: Arc<AtomicUsize>,
}

impl NativeCodeState {
    pub(crate) fn new(functions: usize) -> NativeCodeState {
        let mut revision = NativeCodeRevision {
            slots: lm_bytecode::CodeTable::default(),
            entries: Arc::new(Vec::new()),
            compiled: Arc::new(AtomicUsize::new(0)),
        };
        while revision.slots.len() < functions {
            revision.push_slot();
        }
        NativeCodeState(Arc::new(revision))
    }

    pub(crate) fn extend(&mut self, functions: usize) {
        if self.0.slots.len() >= functions {
            return;
        }
        let mut revision = self.0.as_ref().clone();
        while revision.slots.len() < functions {
            revision.push_slot();
        }
        self.0 = Arc::new(revision);
    }

    fn slot(&self, function: u32) -> Option<&Arc<NativeSlot>> {
        self.0.slots.get(function as usize)
    }

    fn entries(&self) -> &[usize] {
        self.0.entries.as_slice()
    }
}

impl NativeCodeRevision {
    fn push_slot(&mut self) {
        let slot = Arc::new(NativeSlot::default());
        Arc::make_mut(&mut self.entries).push(Arc::as_ptr(&slot.entry) as usize);
        self.slots.push(slot);
    }
}

#[derive(Default)]
struct NativeSlot {
    verdict: OnceLock<Result<Arc<lm_jit::CompiledRegion>, Failure>>,
    entry: Arc<lm_jit::NativeEntryCell>,
}

impl NativeSlot {
    fn region<'a, F>(
        &self,
        compiler: &lm_jit::JitEngine,
        compiled: &AtomicUsize,
        input: F,
    ) -> Result<Arc<lm_jit::CompiledRegion>, Failure>
    where
        F: FnOnce() -> Result<FunctionInput<'a>, Failure>,
    {
        self.verdict
            .get_or_init(|| {
                compiled
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                        (count < MAX_COMPILED_REGIONS).then_some(count + 1)
                    })
                    .map_err(|_| Failure::BackendUnavailable)?;
                let result = input()
                    .and_then(|input| compiler.compile(input))
                    .and_then(|region| self.entry.publish(&region).map(|()| region));
                if result.is_err() {
                    compiled.fetch_sub(1, Ordering::Relaxed);
                }
                result
            })
            .clone()
    }

    fn compiled(&self) -> Option<Arc<lm_jit::CompiledRegion>> {
        self.verdict.get()?.as_ref().ok().cloned()
    }
}

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
        let state = NativeCodeState::new(module.funcs.len());
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
        metrics.note_native_entry_attempt();
        let Some(frame) = machine.vm.frames.last() else {
            metrics.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        let function = frame.func;
        let Some(slot) = native.slot(function) else {
            metrics.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        let region = match slot.region(&self.compiler, &native.0.compiled, || {
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
                input.add_direct_callee(
                    callee,
                    callee_runtime,
                    callee_unit.module(),
                    module.bundle(),
                    callee_local,
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
            &region,
            scratch,
            metrics,
            instruction_limit,
        )
    }

    fn execute_region(
        machine: &mut Machine,
        module: &NamespaceRuntime,
        native: &NativeCodeState,
        region: &lm_jit::CompiledRegion,
        scratch: &mut NativeScratch,
        metrics: &mut EngineTurnMetrics<'_>,
        instruction_limit: u32,
    ) -> NativeAttempt {
        let Some(frame) = machine.vm.frames.last() else {
            metrics.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
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
        let root_capacity = region.max_roots().max(1);
        scratch.roots.resize(root_capacity, 0);
        scratch.roots.fill(0);
        scratch.root_states.resize(root_capacity, 0);
        scratch.root_states.fill(0);
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

        let original_fuel = machine.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(instruction_limit));
        let max_stack_values = machine.config.max_stack_values as usize;
        let max_frames = machine.config.max_frames as usize;
        let base_frames = machine.vm.frames.len().saturating_sub(1);
        metrics.note_native_entry();
        let (exit, heap_reads, allocations) = {
            let mut runtime = MachineRuntime {
                machine,
                module,
                base_local: base,
                base_operand: operand_base,
                heap_reads: 0,
                allocations: 0,
            };
            let exit = region.execute(
                &mut runtime,
                &mut scratch.activation,
                NativeExecution {
                    entry: entry.index(),
                    entries: native.entries(),
                    base_stack_values: base.saturating_add(operand_base),
                    max_stack_values,
                    base_frames,
                    max_frames,
                    roots: &mut scratch.roots,
                    root_states: &mut scratch.root_states,
                    fuel: batch_fuel,
                },
            );
            (exit, runtime.heap_reads, runtime.allocations)
        };
        metrics.note_native_heap_reads(heap_reads);
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
        let top_child = match materialize_native_frames(
            machine,
            native,
            region,
            &scratch.activation,
            exit,
            base,
            operand_base,
        ) {
            Ok(region) => region,
            Err(()) => return malformed_native_exit(retired),
        };
        let top_region = top_child.as_deref().unwrap_or(region);
        metrics.note_native_retired(retired as u64);
        metrics.note_materialization();

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

fn materialize_native_frames(
    machine: &mut Machine,
    native: &NativeCodeState,
    root_region: &lm_jit::CompiledRegion,
    activation: &lm_jit::NativeActivation,
    exit: lm_jit::ExecutionExit,
    root_base: usize,
    root_operand_base: usize,
) -> Result<Option<Arc<lm_jit::CompiledRegion>>, ()> {
    let frames: Vec<_> = activation.frames().collect();
    let root = frames.first().ok_or(())?;
    if root.native_created() || root.locals().len() != root_region.local_kinds().len() {
        return Err(());
    }
    let root_index = machine.vm.frames.len().checked_sub(1).ok_or(())?;
    if machine.vm.frames[root_index].func != root.function() {
        return Err(());
    }
    let child_regions = frames
        .iter()
        .skip(1)
        .map(|frame| {
            if !frame.native_created() {
                return Err(());
            }
            native
                .slot(frame.function())
                .and_then(|slot| slot.compiled())
                .ok_or(())
        })
        .collect::<Result<Vec<_>, _>>()?;
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
            child_regions[index - 1].as_ref()
        };
        if frame.locals().len() != region.local_kinds().len()
            || frame.states().len() != region.local_kinds().len()
        {
            return Err(());
        }
        let top = index + 1 == frames.len();
        let operand_kinds = if !top {
            region.suspended_operand_kinds(frame.block(), frame.instruction())
        } else {
            match exit.kind() {
                ExitKind::Return => Some(&[][..]),
                ExitKind::IntegerOverflow
                | ExitKind::DivideByZero
                | ExitKind::TypeMismatch
                | ExitKind::UninitializedField
                | ExitKind::HeapLimit => {
                    region.fault_operand_kinds(frame.block(), frame.instruction())
                }
                ExitKind::StackLimit => frame
                    .instruction()
                    .checked_sub(1)
                    .and_then(|instruction| region.operand_kinds(frame.block(), instruction)),
                _ => region.operand_kinds(frame.block(), frame.instruction()),
            }
        }
        .ok_or(())?;
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
    Ok(child_regions.last().cloned())
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

fn scalar_bits(kind: ScalarKind, value: Value) -> Option<u64> {
    match (kind, value) {
        (ScalarKind::Unit, Value::Unit) => Some(0),
        (ScalarKind::Bool, Value::Bool(value)) => Some(u64::from(value)),
        (ScalarKind::Int, Value::Int(value)) => Some(value as u64),
        (ScalarKind::Float, Value::Float(bits)) if canonical_float_bits(bits) == bits => Some(bits),
        (ScalarKind::Object(_), Value::Obj(reference)) => Some(object_bits(reference)),
        (ScalarKind::Operation, Value::Op(operation)) => Some(u64::from(operation)),
        _ => None,
    }
}

fn bits_value(kind: ScalarKind, bits: u64) -> Value {
    match kind {
        ScalarKind::Unit => Value::Unit,
        ScalarKind::Bool => Value::Bool(bits != 0),
        ScalarKind::Int => Value::Int(bits as i64),
        ScalarKind::Float => Value::Float(canonical_float_bits(bits)),
        ScalarKind::Object(_) => Value::Obj(object_reference(bits)),
        ScalarKind::Operation => Value::Op(bits as u32),
    }
}

fn object_bits(reference: ObjRef) -> u64 {
    u64::from(reference.slot) | (u64::from(reference.generation) << 32)
}

fn object_reference(bits: u64) -> ObjRef {
    ObjRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

struct MachineRuntime<'a> {
    machine: &'a mut Machine,
    module: &'a NamespaceRuntime,
    base_local: usize,
    base_operand: usize,
    heap_reads: u64,
    allocations: u64,
}

impl Runtime for MachineRuntime<'_> {
    fn load_field(&mut self, reference: ObjRef, field: u32, expected: ValueRepr) -> RuntimeResult {
        self.heap_reads = self.heap_reads.saturating_add(1);
        let Some(crate::Object::Instance { fields, .. }) = self.machine.vm.heap.try_get(reference)
        else {
            return RuntimeResult::TypeMismatch;
        };
        let Some(value) = fields.get(field as usize).copied() else {
            return RuntimeResult::TypeMismatch;
        };
        if value == Value::Uninit {
            return RuntimeResult::UninitializedField;
        }
        match representation_bits(expected, value) {
            Some(bits) => RuntimeResult::Value(bits),
            None => RuntimeResult::Interpreter,
        }
    }

    fn allocate_instance(
        &mut self,
        class: u32,
        root_bits: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> RuntimeResult {
        if root_bits.len() != root_states.len() {
            return RuntimeResult::Interpreter;
        }
        let Some(class_entry) = self.module.classes.get(class as usize) else {
            return RuntimeResult::Interpreter;
        };
        let object = crate::Object::Instance {
            class,
            fields: vec![Value::Uninit; class_entry.fields.len()],
            env: lm_value::Witness::EMPTY,
        };
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            return RuntimeResult::Value(object_bits(reference));
        }
        if !allow_collection {
            return RuntimeResult::Interpreter;
        }
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return RuntimeResult::HeapLimit;
        }
        roots.extend(
            root_bits
                .iter()
                .copied()
                .zip(root_states.iter().copied())
                .filter(|(_, state)| state & LOCAL_INITIALIZED != 0)
                .map(|(bits, _)| object_reference(bits)),
        );
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                RuntimeResult::Value(object_bits(reference))
            }
            Ok(_) => RuntimeResult::Interpreter,
            Err(crate::FaultCode::HeapLimit) => RuntimeResult::HeapLimit,
            Err(_) => RuntimeResult::Interpreter,
        }
    }
}

fn representation_bits(expected: ValueRepr, value: Value) -> Option<u64> {
    match (expected, value) {
        (ValueRepr::Unit, Value::Unit) => Some(0),
        (ValueRepr::Bool, Value::Bool(value)) => Some(u64::from(value)),
        (ValueRepr::Int, Value::Int(value)) => Some(value as u64),
        (ValueRepr::Float, Value::Float(bits)) if canonical_float_bits(bits) == bits => Some(bits),
        (ValueRepr::Object, Value::Obj(reference)) => Some(object_bits(reference)),
        (ValueRepr::Operation, Value::Op(operation)) => Some(u64::from(operation)),
        _ => None,
    }
}
