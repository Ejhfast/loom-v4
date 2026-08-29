//! Canonical machine-state adapter for native execution.

use crate::engine::Engine;
use crate::machine::{ExecError, ExecOutcome, Machine};
use crate::NamespaceRuntime;
use lm_jit::{
    ExitKind, Failure, FunctionInput, Runtime, RuntimeResult, ScalarKind, ValueRepr, LOCAL_DIRTY,
    LOCAL_INITIALIZED,
};
use lm_value::{canonical_float_bits, ObjRef, TypeEnvId, Value};

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
#[derive(Debug, Default)]
pub(crate) struct JitEngine {
    compiler: lm_jit::JitEngine,
}

impl JitEngine {
    pub(crate) fn metrics(&self) -> lm_jit::CompilerMetrics {
        self.compiler.metrics()
    }

    pub(crate) fn reset_metrics(&self) {
        self.compiler.reset_metrics();
    }

    pub(crate) fn execute(
        &self,
        engine: &Engine,
        machine: &mut Machine,
        module: &NamespaceRuntime,
        instruction_limit: u32,
    ) -> NativeAttempt {
        engine.note_native_entry_attempt();
        let Some(frame) = machine.vm.frames.last() else {
            engine.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        let function = frame.func;
        let Some(hash) = module
            .code_namespace()
            .func_hashes()
            .get(function as usize)
            .copied()
        else {
            engine.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        let region = match self.compiler.region(hash, || {
            let runtime = module
                .funcs
                .get(function as usize)
                .ok_or(Failure::Unsupported)?;
            let (unit, local) = module
                .code_namespace()
                .function_unit(function)
                .map_err(|_| Failure::Unsupported)?;
            let mut input = FunctionInput::new(
                hash,
                function,
                runtime,
                unit.module(),
                module.bundle(),
                local,
            );
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
                engine.note_unsupported_region_fallback();
                return NativeAttempt::Fallback;
            }
            Err(Failure::BackendUnavailable) => {
                engine.note_backend_unavailable();
                return NativeAttempt::Fallback;
            }
        };
        self.execute_region(engine, machine, module, &region, instruction_limit)
    }

    fn execute_region(
        &self,
        engine: &Engine,
        machine: &mut Machine,
        module: &NamespaceRuntime,
        region: &lm_jit::CompiledRegion,
        instruction_limit: u32,
    ) -> NativeAttempt {
        let Some(frame) = machine.vm.frames.last() else {
            engine.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        if frame.closure.is_some() || frame.env != TypeEnvId::EMPTY {
            engine.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        }
        let Some(required_frames) =
            (machine.vm.frames.len() as u32).checked_add(region.additional_frames())
        else {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        };
        if required_frames > machine.config.max_frames {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        }
        let Some(entry) = region.entry_plan(frame.block, frame.ip) else {
            engine.note_missing_entry_fallback();
            return region
                .distance_to_entry(frame.block, frame.ip)
                .map_or(NativeAttempt::Fallback, |instructions| {
                    NativeAttempt::AdvanceToEntry { instructions }
                });
        };
        let base = frame.base_local as usize;
        let operand_base = frame.base_operand as usize;
        if operand_base > machine.vm.operands.len() {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        }
        let Some(end) = base.checked_add(region.local_kinds().len()) else {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        };
        let Some(locals) = machine.vm.locals.get(base..end) else {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        };
        let Some(stack_bound) = machine
            .vm
            .locals
            .len()
            .checked_add(operand_base)
            .and_then(|used| used.checked_add(region.max_stack_values()))
        else {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        };
        if stack_bound > machine.config.max_stack_values as usize {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        }

        let operands = &machine.vm.operands[operand_base..];
        if operands.len() != entry.operand_kinds().len() {
            engine.note_guard_failure(0);
            return NativeAttempt::Fallback;
        }
        let mut bits = vec![0; region.local_kinds().len()];
        let mut local_states = vec![0; region.local_kinds().len()];
        let mut stack_bits = vec![0; region.max_stack()];
        let mut guarded = 0u64;
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
                engine.note_guard_failure(guarded);
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
                engine.note_guard_failure(guarded);
                return NativeAttempt::Fallback;
            };
            stack_bits[slot] = value;
        }
        engine.note_guarded_values(guarded);

        let original_fuel = machine.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(instruction_limit));
        engine.note_native_entry();
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
                entry.index(),
                &mut bits,
                &mut local_states,
                &mut stack_bits,
                batch_fuel,
            );
            (exit, runtime.heap_reads, runtime.allocations)
        };
        engine.note_native_heap_reads(heap_reads);
        engine.note_native_allocations(allocations);
        let exit = match exit {
            Ok(exit) => exit,
            Err(_) => {
                engine.note_backend_unavailable();
                return NativeAttempt::Fallback;
            }
        };
        let Ok(retired) = u32::try_from(exit.retired()) else {
            engine.note_backend_unavailable();
            return NativeAttempt::Fallback;
        };
        if retired > instruction_limit || exit.retired() > original_fuel {
            engine.note_backend_unavailable();
            return NativeAttempt::Fallback;
        }
        machine.vm.fuel -= exit.retired();
        for (slot, state) in local_states.iter().copied().enumerate() {
            if state & LOCAL_DIRTY != 0 {
                machine.vm.locals[base + slot] = bits_value(region.local_kinds()[slot], bits[slot]);
            }
        }
        engine.note_native_retired(retired as u64);
        engine.note_materialization();

        match exit.kind() {
            ExitKind::Fuel
            | ExitKind::Interpreter
            | ExitKind::Call
            | ExitKind::Allocation
            | ExitKind::Effect => {
                let interpreter = matches!(exit.kind(), ExitKind::Interpreter);
                let Some(stack_kinds) = region.operand_kinds(exit.block(), exit.instruction())
                else {
                    return malformed_native_exit(retired);
                };
                if exit.stack_len() as usize != stack_kinds.len() {
                    return malformed_native_exit(retired);
                }
                machine.vm.operands.truncate(operand_base);
                machine.vm.operands.extend(
                    stack_kinds
                        .iter()
                        .copied()
                        .zip(stack_bits.iter().copied())
                        .map(|(kind, bits)| bits_value(kind, bits)),
                );
                let Some(frame) = machine.vm.frames.last_mut() else {
                    return NativeAttempt::Complete {
                        outcome: Err(ExecError::Fault(crate::FaultCode::MalformedState)),
                        retired,
                    };
                };
                frame.block = exit.block();
                frame.ip = exit.instruction();
                if matches!(
                    exit.kind(),
                    ExitKind::Call | ExitKind::Allocation | ExitKind::Effect
                ) {
                    if matches!(exit.kind(), ExitKind::Allocation) {
                        engine.note_native_allocation_exit();
                    }
                    if matches!(exit.kind(), ExitKind::Effect) {
                        engine.note_native_effect_exit();
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
                if exit.stack_len() != 0 {
                    return malformed_native_exit(retired);
                }
                let value = bits_value(region.result_kind(), exit.result());
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
            | ExitKind::HeapLimit => {
                let fault_kinds = region
                    .fault_operand_kinds(exit.block(), exit.instruction())
                    .unwrap_or(&[]);
                if exit.stack_len() as usize != fault_kinds.len() {
                    return malformed_native_exit(retired);
                }
                if let Some(frame) = machine.vm.frames.last_mut() {
                    frame.block = exit.block();
                    frame.ip = exit.instruction();
                }
                machine.vm.operands.truncate(operand_base);
                machine.vm.operands.extend(
                    fault_kinds
                        .iter()
                        .copied()
                        .zip(stack_bits.iter().copied())
                        .map(|(kind, bits)| bits_value(kind, bits)),
                );
                engine.note_native_fault_exit();
                let fault = match exit.kind() {
                    ExitKind::IntegerOverflow => crate::FaultCode::IntegerOverflow,
                    ExitKind::DivideByZero => crate::FaultCode::DivideByZero,
                    ExitKind::TypeMismatch => crate::FaultCode::TypeMismatch,
                    ExitKind::UninitializedField => crate::FaultCode::UninitializedField,
                    ExitKind::HeapLimit => crate::FaultCode::HeapLimit,
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

fn malformed_native_exit(retired: u32) -> NativeAttempt {
    NativeAttempt::Complete {
        outcome: Err(ExecError::Fault(crate::FaultCode::MalformedState)),
        retired,
    }
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
