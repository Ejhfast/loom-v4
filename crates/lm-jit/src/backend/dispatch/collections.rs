//! Instruction emission for one dispatch domain.

use super::*;

pub(super) fn emit(emission: &mut InstructionEmission<'_, '_, '_, '_>) -> Result<(), CompileError> {
    let instruction = emission.instruction;
    let builder = &mut *emission.builder;
    let values = emission.values;
    let plan = emission.plan;
    let input = emission.input;
    let segment = emission.segment;
    let stack = &mut *emission.stack;
    let within = emission.within;
    let fault_prefix = emission.fault_prefix;
    let prior_prefix = emission.prior_prefix;
    match instruction {
        Instr::ListLen => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListLen) {
                return Err(CompileError::Backend);
            }
            let value = emit_list_len(
                builder,
                values,
                reference,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::MapLen => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapLen) {
                return Err(CompileError::Backend);
            }
            let value = emit_map_len(
                builder,
                values,
                reference,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::MapHas | Instr::MapAt => {
            let deopt_stack = stack.clone();
            let key = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let (key_contract, value_contract) = match (instruction, access.kind) {
                (Instr::MapHas, HeapAccessKind::MapHas { key }) => (key, None),
                (Instr::MapAt, HeapAccessKind::MapAt { key, value }) => (key, Some(value)),
                _ => return Err(CompileError::Backend),
            };
            let point = FaultPoint {
                block: segment.block,
                instruction: instruction_index + 1,
                prefix: fault_prefix,
            };
            let result = emit_map_lookup(
                builder,
                values,
                MapLookupEmission {
                    reference,
                    key,
                    key_contract,
                    result: if matches!(instruction, Instr::MapAt) {
                        MapLookupResult::At
                    } else {
                        MapLookupResult::Has
                    },
                    exit: HeapExitEmission {
                        point,
                        fault_stack: stack,
                        deopt_stack: &deopt_stack,
                    },
                },
            )?;
            if let Some(contract) = value_contract {
                emit_native_value_contract(builder, values, result, contract, point, &deopt_stack)?;
            }
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapGet { .. }) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let key = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let heap_access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapGet { key: key_contract } = heap_access.kind else {
                return Err(CompileError::Backend);
            };
            let option_access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::MapGet { value } = option_access.kind else {
                return Err(CompileError::Backend);
            };
            let family = emit_option_family(
                builder,
                values,
                input.root.function,
                option_access.family_type,
                FaultPoint {
                    block: segment.block,
                    instruction,
                    prefix: prior_prefix,
                },
                &deopt_stack,
            )?;
            let result = emit_map_lookup(
                builder,
                values,
                MapLookupEmission {
                    reference,
                    key,
                    key_contract,
                    result: MapLookupResult::Get { family, value },
                    exit: HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: stack,
                        deopt_stack: &deopt_stack,
                    },
                },
            )?;
            stack.push(result);
        }
        Instr::MapPut { .. } | Instr::Extended(ExtendedInstr::MapPutText { .. }) => {
            let (discard, own_text_key) = match instruction {
                Instr::MapPut { discard, .. } => (discard, false),
                Instr::Extended(ExtendedInstr::MapPutText { discard, .. }) => (discard, true),
                _ => return Err(CompileError::Backend),
            };
            let deopt_stack = stack.clone();
            let stored = pop_value(stack)?;
            let key = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let heap_access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapPut { key: key_contract } = heap_access.kind else {
                return Err(CompileError::Backend);
            };
            let option_access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::MapPut {
                value: previous_contract,
                discard: planned_discard,
            } = option_access.kind
            else {
                return Err(CompileError::Backend);
            };
            if planned_discard != discard {
                return Err(CompileError::Backend);
            }
            let family = if discard {
                None
            } else {
                Some(emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    option_access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: prior_prefix,
                    },
                    &deopt_stack,
                )?)
            };
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction_index)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            let result = emit_map_put(
                builder,
                values,
                reference,
                key,
                key_contract,
                stored,
                family,
                previous_contract,
                &roots,
                own_text_key,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            if let Some(result) = result {
                stack.push(result);
            }
        }
        Instr::Extended(ExtendedInstr::MapInternTextRange) => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let deopt_stack = stack.clone();
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?;
            let length = pop_native(stack)?;
            let start = pop_native(stack)?;
            let source = pop_native(stack)?;
            let map = pop_native(stack)?;
            let result = emit_map_intern_text_range(
                builder,
                values,
                map,
                source,
                start,
                length,
                &roots,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::ListAt => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::ListAt { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let value = emit_list_at(
                builder,
                values,
                reference,
                index,
                value,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(value);
        }
        Instr::Extended(ExtendedInstr::ListSet) => {
            let deopt_stack = stack.clone();
            let stored = pop_value(stack)?;
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::ListSet { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            emit_list_set(
                builder,
                values,
                reference,
                index,
                stored,
                value,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::ListInsert) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let stored = pop_value(stack)?;
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::ListInsert { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            emit_list_insert(
                builder,
                values,
                ListInsertEmission {
                    reference,
                    index,
                    stored,
                    contract: value,
                    roots: &roots,
                    exit: HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: stack,
                        deopt_stack: &deopt_stack,
                    },
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(
            operation @ (ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove),
        ) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::ListRemove { value, swap } = access.kind else {
                return Err(CompileError::Backend);
            };
            if swap != matches!(operation, ExtendedInstr::ListSwapRemove) {
                return Err(CompileError::Backend);
            }
            let result = emit_list_remove(
                builder,
                values,
                reference,
                index,
                value,
                swap,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::ListTruncate) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let length = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListTruncate) {
                return Err(CompileError::Backend);
            }
            emit_list_truncate(
                builder,
                values,
                reference,
                length,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::ListPush => {
            let deopt_stack = stack.clone();
            let stored = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::ListPush { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            emit_list_push(
                builder,
                values,
                reference,
                stored,
                value,
                &roots,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::ListReserve) => {
            let deopt_stack = stack.clone();
            let additional = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListReserve) {
                return Err(CompileError::Backend);
            }
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            emit_list_reserve(
                builder,
                values,
                reference,
                additional,
                &roots,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::ListReorder) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListReorder) {
                return Err(CompileError::Backend);
            }
            emit_list_reorder(
                builder,
                values,
                reference,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::ListCapacity) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListCapacity) {
                return Err(CompileError::Backend);
            }
            let value = emit_list_capacity(
                builder,
                values,
                reference,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Extended(ExtendedInstr::ListEpoch) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListEpoch) {
                return Err(CompileError::Backend);
            }
            let value = emit_list_epoch(
                builder,
                values,
                reference,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Extended(ExtendedInstr::ListIterLen) => {
            let deopt_stack = stack.clone();
            let expected = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::ListIterLen) {
                return Err(CompileError::Backend);
            }
            let value = emit_list_iter_len(
                builder,
                values,
                reference,
                expected,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Extended(ExtendedInstr::MapEpoch) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapEpoch) {
                return Err(CompileError::Backend);
            }
            let value = emit_map_epoch(
                builder,
                values,
                reference,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Extended(ExtendedInstr::MapIterLen) => {
            let deopt_stack = stack.clone();
            let expected = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapIterLen) {
                return Err(CompileError::Backend);
            }
            let value = emit_map_iter_len(
                builder,
                values,
                reference,
                expected,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Extended(ExtendedInstr::MapNextIndex) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let expected = pop_native(stack)?;
            let cursor = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapNextIndex) {
                return Err(CompileError::Backend);
            }
            let result = emit_map_next_index(
                builder,
                values,
                reference,
                cursor,
                expected,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(operation @ (ExtendedInstr::MapKeyAt | ExtendedInstr::MapValueAt)) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let contract = match (operation, access.kind) {
                (ExtendedInstr::MapKeyAt, HeapAccessKind::MapKeyAt { value }) => value,
                (ExtendedInstr::MapValueAt, HeapAccessKind::MapValueAt { value }) => value,
                _ => return Err(CompileError::Backend),
            };
            let result = emit_map_entry_at(
                builder,
                values,
                reference,
                index,
                matches!(operation, ExtendedInstr::MapValueAt),
                contract,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapRemove { .. }) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let key = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let heap_access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapRemove { key: key_contract } = heap_access.kind else {
                return Err(CompileError::Backend);
            };
            let option_access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::MapRemove { value } = option_access.kind else {
                return Err(CompileError::Backend);
            };
            let family = emit_option_family(
                builder,
                values,
                input.root.function,
                option_access.family_type,
                FaultPoint {
                    block: segment.block,
                    instruction,
                    prefix: prior_prefix,
                },
                &deopt_stack,
            )?;
            let result = emit_map_remove(
                builder,
                values,
                reference,
                key,
                key_contract,
                family,
                value,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapClear) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapClear) {
                return Err(CompileError::Backend);
            }
            emit_object_unary_runtime_value(
                builder,
                values,
                std_mem::offset_of!(RawNativeFunctions, map_clear),
                reference,
                ValueContract {
                    kind: ScalarKind::Unit,
                    object: None,
                },
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::MapReserve) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let additional = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapReserve) {
                return Err(CompileError::Backend);
            }
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            let status = emit_map_reserve_call(builder, values, reference, additional, &roots)?;
            emit_runtime_status(
                builder,
                values,
                status,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                stack,
                &deopt_stack,
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::MapProbe) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let prior = pop_native(stack)?;
            let semantic = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapProbe) {
                return Err(CompileError::Backend);
            }
            let result = emit_map_runtime_value(
                builder,
                values,
                std_mem::offset_of!(RawNativeFunctions, map_probe),
                reference,
                semantic,
                prior,
                ValueContract {
                    kind: ScalarKind::Int,
                    object: None,
                },
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapProbeFound) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let token = pop_native(stack)?;
            let epoch = builder.ins().ushr_imm(token, 32);
            let invalid = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
            emit_interpreter_replay(
                builder,
                values,
                invalid,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            let low = builder.ins().ireduce(types::I32, token);
            let found = builder.ins().icmp_imm(IntCC::NotEqual, low, 0);
            let found = builder.ins().uextend(types::I64, found);
            push_static(builder, stack, ScalarKind::Bool, found)?;
        }
        Instr::Extended(
            operation @ (ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue),
        ) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let token = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let (function_offset, contract) = match (operation, access.kind) {
                (ExtendedInstr::MapProbeKey, HeapAccessKind::MapProbeKey { value }) => (
                    std_mem::offset_of!(RawNativeFunctions, map_probe_key),
                    value,
                ),
                (ExtendedInstr::MapProbeValue, HeapAccessKind::MapProbeValue { value }) => (
                    std_mem::offset_of!(RawNativeFunctions, map_probe_value),
                    value,
                ),
                _ => return Err(CompileError::Backend),
            };
            let result = emit_object_binary_runtime_value(
                builder,
                values,
                function_offset,
                reference,
                token,
                contract,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapProbeSetValue) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let stored = pop_value(stack)?;
            let token = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapProbeSetValue { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            emit_native_value_contract(
                builder,
                values,
                stored,
                value,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            let function = load_value(
                builder,
                values.pointer_type,
                values.runtime_functions,
                std_mem::offset_of!(RawNativeFunctions, map_probe_set_value),
            )?;
            let call = builder.ins().call_indirect(
                values.value_equal_signature,
                function,
                &[
                    values.runtime_context,
                    reference,
                    token,
                    stored.bits,
                    stored.tag,
                    values.allocation_result_pointer,
                ],
            );
            let status = builder.inst_results(call)[0];
            emit_runtime_status(
                builder,
                values,
                status,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                stack,
                &deopt_stack,
            )?;
            let unit = NativeValue {
                bits: builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    values.allocation_result_pointer,
                    0,
                ),
                tag: builder.ins().load(
                    types::I64,
                    MemFlags::new(),
                    values.allocation_result_pointer,
                    8,
                ),
            };
            emit_native_value_contract(
                builder,
                values,
                unit,
                ValueContract {
                    kind: ScalarKind::Unit,
                    object: None,
                },
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            let zero = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, zero)?;
        }
        Instr::Extended(ExtendedInstr::MapProbeRemove) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let token = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapProbeRemove { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let result = emit_object_binary_runtime_value(
                builder,
                values,
                std_mem::offset_of!(RawNativeFunctions, map_probe_remove),
                reference,
                token,
                value,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::MapInsertHashed) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let token = pop_native(stack)?;
            let semantic = pop_native(stack)?;
            let stored = pop_value(stack)?;
            let key = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::MapInsertHashed {
                key: key_contract,
                value: value_contract,
            } = access.kind
            else {
                return Err(CompileError::Backend);
            };
            let point = FaultPoint {
                block: segment.block,
                instruction: instruction + 1,
                prefix: fault_prefix,
            };
            emit_native_value_contract(builder, values, key, key_contract, point, &deopt_stack)?;
            emit_native_value_contract(
                builder,
                values,
                stored,
                value_contract,
                point,
                &deopt_stack,
            )?;
            let root_kinds = segment
                .replay_stacks
                .iter()
                .find(|(position, _)| *position == instruction)
                .map(|(_, stack)| stack.as_slice())
                .ok_or(CompileError::Backend)?;
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, root_kinds, &deopt_stack)?;
            let root_count = emit_runtime_roots(builder, values, &roots)?;
            let function = load_value(
                builder,
                values.pointer_type,
                values.runtime_functions,
                std_mem::offset_of!(RawNativeFunctions, map_insert_hashed),
            )?;
            let call = builder.ins().call_indirect(
                values.map_insert_hashed_signature,
                function,
                &[
                    values.runtime_context,
                    reference,
                    key.bits,
                    key.tag,
                    stored.bits,
                    stored.tag,
                    semantic,
                    token,
                    root_count,
                ],
            );
            let status = builder.inst_results(call)[0];
            emit_runtime_status(builder, values, status, point, stack, &deopt_stack)?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        Instr::Extended(ExtendedInstr::MapWriteGuard) => {
            let instruction = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::MapWriteGuard) {
                return Err(CompileError::Backend);
            }
            let point = FaultPoint {
                block: segment.block,
                instruction: instruction + 1,
                prefix: fault_prefix,
            };
            let entry = emit_object_entry(
                builder,
                values,
                reference,
                JIT_OBJECT_MAP,
                point,
                ObjectGuard::Replay(&deopt_stack),
            )?;
            emit_mutable_guard(
                builder,
                values,
                entry,
                HeapExitEmission {
                    point,
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let unit = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, unit)?;
        }
        _ => return Err(CompileError::Backend),
    }
    Ok(())
}
