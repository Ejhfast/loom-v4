//! Canonical machine-state adapter for native execution.

use crate::engine::Engine;
use crate::machine::{ExecError, ExecOutcome, Machine};
use crate::NamespaceRuntime;
use lm_jit::{ExitKind, Failure, FunctionInput, ScalarKind};
use lm_value::{canonical_float_bits, TypeEnvId, Value};

pub(crate) enum NativeAttempt {
    Fallback,
    AdvanceToEntry {
        instructions: u32,
    },
    Continue {
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
            Ok(FunctionInput::new(
                hash,
                runtime,
                unit.module(),
                module.bundle(),
                local,
            ))
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
        self.execute_region(engine, machine, &region, instruction_limit)
    }

    fn execute_region(
        &self,
        engine: &Engine,
        machine: &mut Machine,
        region: &lm_jit::CompiledRegion,
        instruction_limit: u32,
    ) -> NativeAttempt {
        let Some(frame) = machine.vm.frames.last() else {
            engine.note_missing_entry_fallback();
            return NativeAttempt::Fallback;
        };
        if machine.vm.frames.len() != 1 || frame.closure.is_some() || frame.env != TypeEnvId::EMPTY
        {
            engine.note_missing_entry_fallback();
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
            .and_then(|used| used.checked_add(region.max_stack()))
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
        let mut dirty = vec![0; region.local_kinds().len()];
        let mut stack_bits = vec![0; region.max_stack()];
        let mut guarded = 0u64;
        for (slot, live) in entry.live_locals().iter().copied().enumerate() {
            if !live {
                continue;
            }
            guarded += 1;
            let Some(value) = scalar_bits(region.local_kinds()[slot], locals[slot]) else {
                engine.note_guard_failure(guarded);
                return NativeAttempt::Fallback;
            };
            bits[slot] = value;
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
        let exit = match region.execute(
            entry.index(),
            &mut bits,
            &mut dirty,
            &mut stack_bits,
            batch_fuel,
        ) {
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
        for (slot, changed) in dirty.iter().copied().enumerate() {
            if changed != 0 {
                machine.vm.locals[base + slot] = bits_value(region.local_kinds()[slot], bits[slot]);
            }
        }
        engine.note_native_retired(retired as u64);
        engine.note_materialization();

        match exit.kind() {
            ExitKind::Fuel => {
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
                if exit.retired() == batch_fuel {
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
                NativeAttempt::Complete {
                    outcome: machine
                        .finish_native_return(value)
                        .map(Some)
                        .map_err(ExecError::Fault),
                    retired,
                }
            }
            ExitKind::IntegerOverflow => {
                if exit.stack_len() != 0 {
                    return malformed_native_exit(retired);
                }
                if let Some(frame) = machine.vm.frames.last_mut() {
                    frame.block = exit.block();
                    frame.ip = exit.instruction();
                }
                machine.vm.operands.truncate(operand_base);
                engine.note_native_fault_exit();
                NativeAttempt::Complete {
                    outcome: Err(ExecError::Fault(crate::FaultCode::IntegerOverflow)),
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
        _ => None,
    }
}

fn bits_value(kind: ScalarKind, bits: u64) -> Value {
    match kind {
        ScalarKind::Unit => Value::Unit,
        ScalarKind::Bool => Value::Bool(bits != 0),
        ScalarKind::Int => Value::Int(bits as i64),
        ScalarKind::Float => Value::Float(canonical_float_bits(bits)),
    }
}
