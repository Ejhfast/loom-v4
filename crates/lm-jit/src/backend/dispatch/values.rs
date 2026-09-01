//! Instruction emission for one dispatch domain.

use super::*;

pub(super) fn emit(emission: &mut InstructionEmission<'_, '_, '_, '_>) -> Result<(), CompileError> {
    let instruction = emission.instruction;
    let builder = &mut *emission.builder;
    let values = emission.values;
    let plan = emission.plan;
    let input = emission.input;
    let segment = emission.segment;
    let type_environment_sites = emission.type_environment_sites;
    let stack = &mut *emission.stack;
    let virtual_stack = &mut *emission.virtual_stack;
    let initialized_locals = &mut *emission.initialized_locals;
    let virtual_locals = &mut *emission.virtual_locals;
    let within = emission.within;
    let prefix = emission.prefix;
    let fault_prefix = emission.fault_prefix;
    let prior_prefix = emission.prior_prefix;
    match instruction {
        Instr::ConstUnit => {
            let value = builder.ins().iconst(types::I64, 0);
            push_static(builder, stack, ScalarKind::Unit, value)?;
        }
        Instr::MakeClosure { func, captures } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let capture_count = usize::try_from(captures).map_err(|_| CompileError::Backend)?;
            let stack_start = stack
                .len()
                .checked_sub(capture_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, capture_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                capture_count,
            )?;
            let frame = emit_current_frame_pointer(builder, values)?;
            let environment = load_cell_u32(
                builder,
                frame,
                std_mem::offset_of!(RawNativeFrame, environment),
            )?;
            let result = emit_capture_allocation(
                builder,
                values,
                CaptureAllocationEmission {
                    function: func,
                    environment,
                    capture_start,
                    capture_count,
                    roots: &roots,
                    callback: false,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::Extended(ExtendedInstr::MakeCallback { func, captures }) => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let capture_count = usize::try_from(captures).map_err(|_| CompileError::Backend)?;
            let stack_start = stack
                .len()
                .checked_sub(capture_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, capture_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                capture_count,
            )?;
            let frame = emit_current_frame_pointer(builder, values)?;
            let environment = load_cell_u32(
                builder,
                frame,
                std_mem::offset_of!(RawNativeFrame, environment),
            )?;
            let result = emit_capture_allocation(
                builder,
                values,
                CaptureAllocationEmission {
                    function: func,
                    environment,
                    capture_start,
                    capture_count,
                    roots: &roots,
                    callback: true,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::Callback as u64 as i64);
            stack.push(NativeValue { bits: result, tag });
        }
        Instr::TupleNew { count, .. }
        | Instr::ListNew { count, .. }
        | Instr::MapNew { count, .. } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let item_count = usize::try_from(count).map_err(|_| CompileError::Backend)?;
            let item_count = if matches!(instruction, Instr::MapNew { .. }) {
                item_count.checked_mul(2).ok_or(CompileError::Backend)?
            } else {
                item_count
            };
            let stack_start = stack
                .len()
                .checked_sub(item_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, item_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                item_count,
            )?;
            let kind = match instruction {
                Instr::TupleNew { .. } => ValueArrayAllocationKind::Tuple,
                Instr::ListNew { .. } => ValueArrayAllocationKind::List,
                Instr::MapNew { .. } => ValueArrayAllocationKind::Map,
                _ => return Err(CompileError::Backend),
            };
            let result = emit_value_array_allocation(
                builder,
                values,
                ValueArrayAllocationEmission {
                    kind,
                    item_start,
                    item_count,
                    roots: &roots,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::New(class) | Instr::NewG { class, .. } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let mut roots = Vec::new();
            for (slot, (kind, variable)) in plan
                .local_kinds
                .iter()
                .copied()
                .zip(values.locals.iter().copied())
                .enumerate()
            {
                if is_root_kind(kind) {
                    roots.push(NativeRoot {
                        bits: builder.use_var(variable),
                        tag: emit_slot_tag(builder, values.local_tags[slot], kind)?,
                        state: Some(emit_local_state(builder, values, slot)?),
                    });
                }
            }
            extend_stack_roots(&mut roots, &site.stack, stack)?;
            let environment = if matches!(instruction, Instr::NewG { .. }) {
                let site = type_environment_sites
                    .iter()
                    .find(|site| site.block == segment.block && site.instruction == position)
                    .ok_or(CompileError::Backend)?;
                emit_type_environment_lookup(
                    builder,
                    values,
                    site,
                    FaultPoint {
                        block: segment.block,
                        instruction: position,
                        prefix: prior_prefix,
                    },
                    stack,
                )?
            } else {
                builder.ins().iconst(types::I32, 0)
            };
            let value = emit_allocate_instance(
                builder,
                values,
                class,
                instance_field_count(input, class),
                environment,
                InstanceAllocationEmission {
                    roots: &roots,
                    allow_pending: plan
                        .virtual_constructor
                        .is_some_and(|constructor| constructor.class == class),
                    exit: ReplayEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        deopt_stack: stack,
                    },
                },
            )?;
            push_static(builder, stack, ScalarKind::Object(0), value)?;
        }
        Instr::ConstBool(value) => {
            let value = builder.ins().iconst(types::I64, i64::from(value));
            push_static(builder, stack, ScalarKind::Bool, value)?;
        }
        Instr::ConstInt(value) => {
            let value = builder.ins().iconst(types::I64, value);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::ConstFloat(bits) => {
            let value = builder
                .ins()
                .iconst(types::I64, canonical_float_bits(bits) as i64);
            push_static(builder, stack, ScalarKind::Float, value)?;
        }
        Instr::ConstChar(value) => {
            let value = builder.ins().iconst(types::I64, i64::from(value));
            push_static(builder, stack, ScalarKind::Char, value)?;
        }
        Instr::ConstStr(index) => {
            let instruction = segment.start + within as u32;
            let value = emit_literal_load(
                builder,
                values,
                index as usize,
                FaultPoint {
                    block: segment.block,
                    instruction,
                    prefix: prior_prefix,
                },
                stack,
            )?;
            stack.push(value);
        }
        Instr::ConstBytes(index) => {
            let literal = input
                .runtime_string_count()
                .checked_add(index as usize)
                .ok_or(CompileError::Backend)?;
            let instruction = segment.start + within as u32;
            let value = emit_literal_load(
                builder,
                values,
                literal,
                FaultPoint {
                    block: segment.block,
                    instruction,
                    prefix: prior_prefix,
                },
                stack,
            )?;
            stack.push(value);
        }
        Instr::OpConst(operation) => {
            let value = builder.ins().iconst(types::I64, i64::from(operation));
            push_static(builder, stack, ScalarKind::Operation, value)?;
        }
        Instr::LoadLocal(slot) => {
            let slot = slot as usize;
            let bits = builder.use_var(values.locals[slot]);
            if virtual_locals[slot] {
                emit_retain_pending_instance(builder, values, bits)?;
            }
            if values.local_heap_caches[slot].is_some() {
                values
                    .heap_translations
                    .borrow_mut()
                    .record_local(bits, slot);
            }
            stack.push(NativeValue {
                bits,
                tag: emit_slot_tag(builder, values.local_tags[slot], plan.local_kinds[slot])?,
            });
        }
        Instr::StoreLocal(slot) => {
            let slot = slot as usize;
            if virtual_locals[slot] {
                let old = builder.use_var(values.locals[slot]);
                emit_release_pending_instance(builder, values, old)?;
            }
            let value = pop_value(stack)?;
            builder.def_var(values.locals[slot], value.bits);
            define_slot_tag(
                builder,
                values.local_tags[slot],
                plan.local_kinds[slot],
                value.tag,
            )?;
            if !initialized_locals[slot] {
                let state = builder
                    .ins()
                    .iconst(types::I8, i64::from(LOCAL_INITIALIZED));
                store_local_state(builder, values, slot, state)?;
                initialized_locals[slot] = true;
            }
            values.heap_translations.borrow_mut().forget_local(slot);
            clear_local_heap_cache(builder, values, slot);
        }
        Instr::Pop => {
            if virtual_stack.last().copied().unwrap_or(false) {
                let value = stack.last().copied().ok_or(CompileError::Backend)?;
                emit_release_pending_instance(builder, values, value.bits)?;
            }
            pop_native(stack)?;
        }
        Instr::LoadCapture(index) => {
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::LoadCapture { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let value = emit_load_capture(
                builder,
                values,
                index,
                value,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: stack,
                },
            )?;
            stack.push(value);
        }
        Instr::LoadField(field) => {
            let deopt_stack = stack.clone();
            let allow_pending = virtual_stack.last().copied().unwrap_or(false);
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::LoadField {
                receiver_class,
                value,
            } = access.kind
            else {
                return Err(CompileError::Backend);
            };
            let value = emit_load_field(
                builder,
                values,
                reference,
                LoadFieldEmission {
                    field,
                    receiver_class,
                    contract: value,
                    allow_pending,
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
            if allow_pending {
                emit_release_pending_instance(builder, values, reference)?;
            }
            stack.push(value);
        }
        Instr::StoreField(field) => {
            let deopt_stack = stack.clone();
            let allow_pending = virtual_stack
                .len()
                .checked_sub(2)
                .and_then(|index| virtual_stack.get(index))
                .copied()
                .unwrap_or(false);
            let stored = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::StoreField {
                receiver_class,
                value,
            } = access.kind
            else {
                return Err(CompileError::Backend);
            };
            emit_store_field(
                builder,
                values,
                reference,
                stored,
                allow_pending,
                StoreFieldEmission {
                    field,
                    receiver_class,
                    contract: value,
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
            if allow_pending {
                emit_release_pending_instance(builder, values, reference)?;
            }
        }
        Instr::TupleGet(index) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::TupleGet { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let value = emit_tuple_get(
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
        Instr::EqDigest | Instr::NeDigest => {
            let deopt_stack = stack.clone();
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::DigestCompare) {
                return Err(CompileError::Backend);
            }
            let equal = emit_digest_equal(
                builder,
                values,
                left,
                right,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            let result = if matches!(instruction, Instr::EqDigest) {
                equal
            } else {
                builder.ins().bxor_imm(equal, 1)
            };
            let result = builder.ins().uextend(types::I64, result);
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::Extended(ExtendedInstr::AsCallback) => {
            let deopt_stack = stack.clone();
            let value = pop_value(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::AsCallback) {
                return Err(CompileError::Backend);
            }
            emit_object_entry(
                builder,
                values,
                value.bits,
                JIT_OBJECT_CLOSURE,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                ObjectGuard::Replay(&deopt_stack),
            )?;
            stack.push(value);
        }
        Instr::Extended(ExtendedInstr::OptionSome { .. }) => {
            let value = pop_value(stack)?;
            stack.push(value);
        }
        Instr::Extended(ExtendedInstr::OptionNone { .. }) => {
            let instruction_index = segment.start + within as u32;
            let access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, OptionAccessKind::None) {
                return Err(CompileError::Backend);
            }
            let family = emit_option_family(
                builder,
                values,
                input.root.function,
                access.family_type,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index,
                    prefix: prior_prefix,
                },
                stack,
            )?;
            let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
            let payload = builder.ins().bor(family, arm);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
            stack.push(NativeValue { bits: payload, tag });
        }
        Instr::Extended(ExtendedInstr::OptionPayload { .. }) => {
            let instruction_index = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let value = pop_value(stack)?;
            let access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::Payload { value: contract } = access.kind else {
                return Err(CompileError::Backend);
            };
            let family = emit_option_family(
                builder,
                values,
                input.root.function,
                access.family_type,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index,
                    prefix: prior_prefix,
                },
                &deopt_stack,
            )?;
            let exact_none = emit_exact_option_none(builder, value, family);
            emit_fault_check(
                builder,
                values,
                exact_none,
                EXIT_TYPE_MISMATCH,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            emit_native_value_contract(
                builder,
                values,
                value,
                contract,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            stack.push(value);
        }
        Instr::Extended(ExtendedInstr::ListGet { .. }) => {
            let instruction_index = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::ListGet { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let result = emit_list_get(
                builder,
                values,
                input.root.function,
                reference,
                index,
                value,
                access.family_type,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index,
                    prefix: prior_prefix,
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::ListPop { .. }) => {
            let instruction_index = segment.start + within as u32;
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let OptionAccessKind::ListPop { value } = access.kind else {
                return Err(CompileError::Backend);
            };
            let result = emit_list_pop(
                builder,
                values,
                reference,
                ListOptionEmission {
                    function: input.root.function,
                    result: value,
                    family_type: access.family_type,
                    exit: HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction_index + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: stack,
                        deopt_stack: &deopt_stack,
                    },
                    resolve: FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: prior_prefix,
                    },
                },
            )?;
            stack.push(result);
        }
        Instr::Extended(ExtendedInstr::ListContains) => {
            let deopt_stack = stack.clone();
            let needle = pop_value(stack)?;
            let reference = pop_native(stack)?;
            let result = emit_runtime_value_lookup(
                builder,
                values,
                std_mem::offset_of!(RawNativeFunctions, list_contains),
                reference,
                needle,
                HeapExitEmission {
                    point: FaultPoint {
                        block: segment.block,
                        instruction: segment.start + prefix,
                        prefix: fault_prefix,
                    },
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            stack.push(result);
        }
        Instr::IsType(_) | Instr::CastType(_) => {
            let deopt_stack = stack.clone();
            let allow_pending = virtual_stack.last().copied().unwrap_or(false);
            let value = pop_value(stack)?;
            let instruction_index = segment.start + within as u32;
            let option = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied();
            if let Some(access) = option {
                let target = match access.kind {
                    OptionAccessKind::IsType { target } | OptionAccessKind::CastType { target } => {
                        target
                    }
                    _ => return Err(CompileError::Backend),
                };
                let family = emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: prior_prefix,
                    },
                    &deopt_stack,
                )?;
                let exact_none = emit_exact_option_none(builder, value, family);
                let matches = match target {
                    OptionTarget::Family => builder.ins().iconst(types::I8, 1),
                    OptionTarget::Some => builder.ins().bxor_imm(exact_none, 1),
                    OptionTarget::None => exact_none,
                };
                if matches!(instruction, Instr::IsType(_)) {
                    let result = builder.ins().uextend(types::I64, matches);
                    if allow_pending {
                        emit_release_pending_instance(builder, values, value.bits)?;
                    }
                    push_static(builder, stack, ScalarKind::Bool, result)?;
                } else {
                    let mismatch = builder.ins().bxor_imm(matches, 1);
                    emit_interpreter_replay(
                        builder,
                        values,
                        mismatch,
                        FaultPoint {
                            block: segment.block,
                            instruction: instruction_index + 1,
                            prefix: fault_prefix,
                        },
                        &deopt_stack,
                    )?;
                    stack.push(value);
                }
            } else {
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let target_class = match access.kind {
                    HeapAccessKind::IsType { target_class }
                    | HeapAccessKind::CastType { target_class } => target_class,
                    _ => return Err(CompileError::Backend),
                };
                let point = FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                };
                let actual = if allow_pending {
                    emit_instance_storage(
                        builder,
                        values,
                        value.bits,
                        None,
                        point,
                        ObjectGuard::Replay(&deopt_stack),
                        ObjectGuard::Replay(&deopt_stack),
                    )?
                    .actual_class
                } else {
                    let entry = emit_object_entry(
                        builder,
                        values,
                        value.bits,
                        JIT_OBJECT_INSTANCE,
                        point,
                        ObjectGuard::Replay(&deopt_stack),
                    )?;
                    load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?
                };
                let matches = emit_class_matches(builder, values, actual, target_class)?;
                if matches!(instruction, Instr::IsType(_)) {
                    let result = builder.ins().uextend(types::I64, matches);
                    if allow_pending {
                        emit_release_pending_instance(builder, values, value.bits)?;
                    }
                    push_static(builder, stack, ScalarKind::Bool, result)?;
                } else {
                    let mismatch = builder.ins().bxor_imm(matches, 1);
                    emit_interpreter_replay(builder, values, mismatch, point, &deopt_stack)?;
                    stack.push(value);
                }
            }
        }
        _ => return Err(CompileError::Backend),
    }
    Ok(())
}
