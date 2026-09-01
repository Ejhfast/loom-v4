//! LMBC instruction dispatch.

use super::*;

pub(super) fn emit_segment_body(
    builder: &mut FunctionBuilder<'_>,
    emission: SegmentEmission<'_, '_>,
    mut stack: Vec<NativeValue>,
    mut virtual_stack: Vec<bool>,
) -> Result<(), CompileError> {
    let SegmentEmission {
        bytecode,
        segment,
        successor_blocks,
        values,
        plan,
        input,
        type_environment_sites,
    } = emission;
    {
        let mut translations = values.heap_translations.borrow_mut();
        translations.clear();
        translations.set_cached_list_data(true);
    }
    let reserved_prefix_cost = segment.reserved_prefix_cost;
    let fast_segment_cost = reserved_prefix_cost
        .checked_add(segment.cost)
        .ok_or(CompileError::Backend)?;
    let mut deferred_integer_overflow = if segment.defer_integer_overflow {
        Some(DeferredIntegerOverflow {
            flag: None,
            locals: capture_local_values(builder, values)?,
            stack: stack.clone(),
        })
    } else {
        None
    };
    // The entry guard initializes each live local in canonical state storage.
    // A store only initializes a slot that was dormant at this entry.
    let mut initialized_locals = segment.live_in.clone();
    let mut virtual_locals = segment.virtual_locals_in.clone();
    if virtual_stack.len() != stack.len() || virtual_locals.len() != plan.local_kinds.len() {
        return Err(CompileError::Backend);
    }
    let code =
        &bytecode.blocks[segment.block as usize][segment.start as usize..segment.end as usize];
    for (within, instruction) in code.iter().copied().enumerate() {
        let prefix = within as u32 + 1;
        let fault_prefix = reserved_prefix_cost
            .checked_add(prefix)
            .ok_or(CompileError::Backend)?;
        let prior_prefix = reserved_prefix_cost
            .checked_add(prefix - 1)
            .ok_or(CompileError::Backend)?;
        let position = segment.start + within as u32;
        let source_instruction = input
            .root
            .source
            .funcs
            .get(input.root.source_function as usize)
            .and_then(|function| function.blocks.get(segment.block as usize))
            .and_then(|block| block.get(position as usize))
            .copied()
            .ok_or(CompileError::Backend)?;
        if segment.virtual_barriers.binary_search(&position).is_ok() {
            emit_pending_instance_barrier(
                builder,
                values,
                FaultPoint {
                    block: segment.block,
                    instruction: position,
                    prefix: prior_prefix,
                },
                &stack,
            )?;
            virtual_locals.fill(false);
            virtual_stack.fill(false);
        }
        match instruction {
            Instr::ConstUnit => {
                let value = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, value)?;
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
                    &stack,
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
                        replay_stack: &stack,
                        fault_stack: &post_stack,
                    },
                )?;
                stack.truncate(stack_start);
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                    &stack,
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
                        replay_stack: &stack,
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
                    &stack,
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
                        replay_stack: &stack,
                        fault_stack: &post_stack,
                    },
                )?;
                stack.truncate(stack_start);
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
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
                        &stack,
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
                            deopt_stack: &stack,
                        },
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), value)?;
            }
            Instr::ConstBool(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Bool, value)?;
            }
            Instr::ConstInt(value) => {
                let value = builder.ins().iconst(types::I64, value);
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::ConstFloat(bits) => {
                let value = builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64);
                push_static(builder, &mut stack, ScalarKind::Float, value)?;
            }
            Instr::ConstChar(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
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
                    &stack,
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
                    &stack,
                )?;
                stack.push(value);
            }
            Instr::OpConst(operation) => {
                let value = builder.ins().iconst(types::I64, i64::from(operation));
                push_static(builder, &mut stack, ScalarKind::Operation, value)?;
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
                let value = pop_value(&mut stack)?;
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
                pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::LoadField(field) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
                let reference = pop_native(&mut stack)?;
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
                            fault_stack: &stack,
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
                let stored = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                            fault_stack: &stack,
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
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::EqDigest | Instr::NeDigest => {
                let deopt_stack = stack.clone();
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Extended(ExtendedInstr::AsCallback) => {
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
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
                let value = pop_value(&mut stack)?;
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
                    &stack,
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
                let value = pop_value(&mut stack)?;
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
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
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
                let reference = pop_native(&mut stack)?;
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
                            fault_stack: &stack,
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
                let needle = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
                let value = pop_value(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let option = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied();
                if let Some(access) = option {
                    let target = match access.kind {
                        OptionAccessKind::IsType { target }
                        | OptionAccessKind::CastType { target } => target,
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
                        push_static(builder, &mut stack, ScalarKind::Bool, result)?;
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
                        push_static(builder, &mut stack, ScalarKind::Bool, result)?;
                    } else {
                        let mismatch = builder.ins().bxor_imm(matches, 1);
                        emit_interpreter_replay(builder, values, mismatch, point, &deopt_stack)?;
                        stack.push(value);
                    }
                }
            }
            Instr::ListLen => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::MapLen => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::MapHas | Instr::MapAt => {
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                    },
                )?;
                if let Some(contract) = value_contract {
                    emit_native_value_contract(
                        builder,
                        values,
                        result,
                        contract,
                        point,
                        &deopt_stack,
                    )?;
                }
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapGet { .. }) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                    },
                )?;
                stack.push(result);
            }
            Instr::MapPut { discard, .. } => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction_index + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                if let Some(result) = result {
                    stack.push(result);
                }
            }
            Instr::ListAt => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::Extended(ExtendedInstr::ListSet) => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListInsert) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(
                operation @ (ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove),
            ) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::ListTruncate) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let length = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::ListPush => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListReserve) => {
                let deopt_stack = stack.clone();
                let additional = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListReorder) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListCapacity) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::ListEpoch) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::ListIterLen) => {
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::MapEpoch) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::MapIterLen) => {
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::MapNextIndex) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let cursor = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(operation @ (ExtendedInstr::MapKeyAt | ExtendedInstr::MapValueAt)) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapRemove { .. }) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapClear) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::MapReserve) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let additional = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                    &stack,
                    &deopt_stack,
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::MapProbe) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let prior = pop_native(&mut stack)?;
                let semantic = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapProbeFound) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Bool, found)?;
            }
            Instr::Extended(
                operation @ (ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue),
            ) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapProbeSetValue) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                    &stack,
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
                push_static(builder, &mut stack, ScalarKind::Unit, zero)?;
            }
            Instr::Extended(ExtendedInstr::MapProbeRemove) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapInsertHashed) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let semantic = pop_native(&mut stack)?;
                let stored = pop_value(&mut stack)?;
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                emit_native_value_contract(
                    builder,
                    values,
                    key,
                    key_contract,
                    point,
                    &deopt_stack,
                )?;
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
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
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
                emit_runtime_status(builder, values, status, point, &stack, &deopt_stack)?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::MapWriteGuard) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::SealInstance { class } = access.kind else {
                    return Err(CompileError::Backend);
                };
                emit_seal_instance(
                    builder,
                    values,
                    reference,
                    class,
                    allow_pending,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), reference)?;
            }
            Instr::Native(
                NativeInstr::SbAppendStr
                | NativeInstr::SbAppendInt
                | NativeInstr::SbAppendBool
                | NativeInstr::SbAppendChar
                | NativeInstr::BbAppend
                | NativeInstr::BbExtend
                | NativeInstr::BbReserve,
            ) => {
                let position = segment.start + within as u32;
                let site = segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == position)
                    .ok_or(CompileError::Backend)?;
                let deopt_stack = stack.clone();
                let roots =
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: position + 1,
                    prefix: fault_prefix,
                };
                let result = match instruction {
                    Instr::Native(NativeInstr::SbAppendStr) => {
                        let source = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_text(
                            builder,
                            values,
                            target,
                            source,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendBool) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_bool(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendInt) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_int(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendChar) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_char(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbAppend) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_append(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbExtend) => {
                        let source = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_extend(
                            builder,
                            values,
                            target,
                            source,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbReserve) => {
                        let additional = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_reserve(
                            builder,
                            values,
                            target,
                            additional,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    _ => return Err(CompileError::Backend),
                };
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
            }
            Instr::FaultCode
            | Instr::FaultDenied
            | Instr::Extended(ExtendedInstr::DynPack { .. })
            | Instr::Native(
                NativeInstr::SbNew
                | NativeInstr::SbBuild
                | NativeInstr::SbFinish
                | NativeInstr::BbNew
                | NativeInstr::BbBuild
                | NativeInstr::BbFinish
                | NativeInstr::BytesNew
                | NativeInstr::BytesSlice
                | NativeInstr::BytesConcat
                | NativeInstr::BytesCompact
                | NativeInstr::BytesTextView,
            )
            | Instr::Numeric(
                NumericInstr::SbAppendFloat
                | NumericInstr::BytesBitAnd
                | NumericInstr::BytesBitOr
                | NumericInstr::BytesBitXor
                | NumericInstr::BytesBitNot,
            ) => {
                let position = segment.start + within as u32;
                let site = segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == position)
                    .ok_or(CompileError::Backend)?;
                let deopt_stack = stack.clone();
                let roots =
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset) = match instruction {
                    Instr::FaultCode => {
                        let fault = pop_native(&mut stack)?;
                        (
                            [fault, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, fault_code),
                        )
                    }
                    Instr::FaultDenied => {
                        let reason = pop_native(&mut stack)?;
                        (
                            [reason, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, fault_denied),
                        )
                    }
                    Instr::Extended(ExtendedInstr::DynPack { ty }) => {
                        let value = pop_value(&mut stack)?;
                        let frame = emit_current_frame_pointer(builder, values)?;
                        let environment = load_cell_u32(
                            builder,
                            frame,
                            std_mem::offset_of!(RawNativeFrame, environment),
                        )?;
                        let environment = builder.ins().uextend(types::I64, environment);
                        let environment = builder.ins().ishl_imm(environment, 32);
                        let ty = builder.ins().iconst(types::I64, i64::from(ty));
                        let packed = builder.ins().bor(ty, environment);
                        (
                            [value.bits, value.tag, packed],
                            std_mem::offset_of!(RawNativeFunctions, dyn_pack),
                        )
                    }
                    Instr::Native(NativeInstr::SbNew) => (
                        [zero, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, string_builder_new),
                    ),
                    Instr::Native(NativeInstr::BbNew) => (
                        [zero, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, byte_buffer_new),
                    ),
                    Instr::Numeric(NumericInstr::SbAppendFloat) => {
                        let value = pop_native(&mut stack)?;
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, value, zero],
                            std_mem::offset_of!(RawNativeFunctions, string_builder_append_float),
                        )
                    }
                    Instr::Native(NativeInstr::SbBuild) => {
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, string_builder_build),
                        )
                    }
                    Instr::Native(NativeInstr::SbFinish) => {
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, string_builder_finish),
                        )
                    }
                    Instr::Native(NativeInstr::BbBuild) => {
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, byte_buffer_build),
                        )
                    }
                    Instr::Native(NativeInstr::BbFinish) => {
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, byte_buffer_finish),
                        )
                    }
                    Instr::Native(NativeInstr::BytesNew) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_from_text),
                        )
                    }
                    Instr::Native(NativeInstr::BytesSlice) => {
                        let length = pop_native(&mut stack)?;
                        let start = pop_native(&mut stack)?;
                        let source = pop_native(&mut stack)?;
                        (
                            [source, start, length],
                            std_mem::offset_of!(RawNativeFunctions, bytes_slice),
                        )
                    }
                    Instr::Native(NativeInstr::BytesConcat) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_concat),
                        )
                    }
                    Instr::Native(NativeInstr::BytesCompact) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_compact),
                        )
                    }
                    Instr::Native(NativeInstr::BytesTextView) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_text_view),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitAnd) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_bit_and),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitOr) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_bit_or),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitXor) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_bit_xor),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitNot) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_bit_not),
                        )
                    }
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_heap_operation(
                    builder,
                    values,
                    function_offset,
                    arguments,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
            }
            Instr::Native(NativeInstr::SbLen | NativeInstr::SbByteLen | NativeInstr::BbLen) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let position = segment.start + within as u32;
                let (tag, active, length) = match instruction {
                    Instr::Native(NativeInstr::SbLen) => (
                        JIT_OBJECT_STRING_BUILDER,
                        JIT_STRING_BUILDER_ACTIVE_OFFSET,
                        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
                    ),
                    Instr::Native(NativeInstr::SbByteLen) => (
                        JIT_OBJECT_STRING_BUILDER,
                        JIT_STRING_BUILDER_ACTIVE_OFFSET,
                        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
                    ),
                    Instr::Native(NativeInstr::BbLen) => (
                        JIT_OBJECT_BYTE_BUFFER,
                        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
                        JIT_BYTE_BUFFER_LEN_OFFSET,
                    ),
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_builder_len(
                    builder,
                    values,
                    reference,
                    tag,
                    (active, length),
                    FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Native(NativeInstr::SbClear | NativeInstr::BbClear) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let position = segment.start + within as u32;
                emit_builder_clear(
                    builder,
                    values,
                    reference,
                    matches!(instruction, Instr::Native(NativeInstr::SbClear)),
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), reference)?;
            }
            Instr::Native(NativeInstr::BbAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let position = segment.start + within as u32;
                let result = emit_byte_buffer_at(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Native(NativeInstr::BytesLen) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::BytesLen) {
                    return Err(CompileError::Backend);
                }
                let value = emit_bytes_len(
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::BytesAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::BytesAt) {
                    return Err(CompileError::Backend);
                }
                let value = emit_bytes_at(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::BytesGet) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::BytesGet) {
                    return Err(CompileError::Backend);
                }
                let value = emit_bytes_get(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let offset = match access.kind {
                    HeapAccessKind::TextByteLen => JIT_TEXT_BYTE_LEN_OFFSET,
                    HeapAccessKind::TextScalarLen => JIT_TEXT_SCALAR_LEN_OFFSET,
                    _ => return Err(CompileError::Backend),
                };
                let value = emit_text_len(
                    builder,
                    values,
                    reference,
                    offset,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::TextAtByte) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::TextAtByte) {
                    return Err(CompileError::Backend);
                }
                let value = emit_text_at_byte(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::Native(NativeInstr::TextAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::TextAt) {
                    return Err(CompileError::Backend);
                }
                let value = emit_text_at(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::Native(NativeInstr::TextIsBoundary) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::TextIsBoundary) {
                    return Err(CompileError::Backend);
                }
                let value = emit_text_is_boundary(
                    builder,
                    values,
                    reference,
                    index,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Bool, value)?;
            }
            Instr::Extended(
                ExtendedInstr::SyntaxTreeRoot
                | ExtendedInstr::SyntaxKind
                | ExtendedInstr::SyntaxCategory
                | ExtendedInstr::SyntaxRangeStart
                | ExtendedInstr::SyntaxRangeEnd
                | ExtendedInstr::SyntaxText
                | ExtendedInstr::SyntaxChildren
                | ExtendedInstr::SyntaxDetach
                | ExtendedInstr::SyntaxBuildToken
                | ExtendedInstr::SyntaxBuildTrivia
                | ExtendedInstr::SyntaxBuildNode
                | ExtendedInstr::SyntaxToTree,
            ) => {
                let position = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let roots = match segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == position)
                {
                    Some(site) => collect_native_roots(
                        builder,
                        values,
                        &plan.local_kinds,
                        &site.stack,
                        &stack,
                    )?,
                    None => Vec::new(),
                };
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset, result_kind) = match instruction {
                    Instr::Extended(ExtendedInstr::SyntaxTreeRoot) => {
                        let tree = pop_native(&mut stack)?;
                        (
                            [tree, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, syntax_tree_root),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Extended(
                        operation @ (ExtendedInstr::SyntaxKind
                        | ExtendedInstr::SyntaxCategory
                        | ExtendedInstr::SyntaxRangeStart
                        | ExtendedInstr::SyntaxRangeEnd),
                    ) => {
                        let element = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxKind => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_kind)
                            }
                            ExtendedInstr::SyntaxCategory => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_category)
                            }
                            ExtendedInstr::SyntaxRangeStart => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_range_start)
                            }
                            ExtendedInstr::SyntaxRangeEnd => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_range_end)
                            }
                            _ => return Err(CompileError::Backend),
                        };
                        ([element, zero, zero], function_offset, ScalarKind::Int)
                    }
                    Instr::Extended(
                        operation @ (ExtendedInstr::SyntaxText
                        | ExtendedInstr::SyntaxChildren
                        | ExtendedInstr::SyntaxDetach
                        | ExtendedInstr::SyntaxToTree),
                    ) => {
                        let element = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxText => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_text)
                            }
                            ExtendedInstr::SyntaxChildren => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_children)
                            }
                            ExtendedInstr::SyntaxDetach => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_detach)
                            }
                            ExtendedInstr::SyntaxToTree => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_to_tree)
                            }
                            _ => return Err(CompileError::Backend),
                        };
                        (
                            [element, zero, zero],
                            function_offset,
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Extended(
                        operation @ (ExtendedInstr::SyntaxBuildToken
                        | ExtendedInstr::SyntaxBuildTrivia
                        | ExtendedInstr::SyntaxBuildNode),
                    ) => {
                        let value = pop_native(&mut stack)?;
                        let kind = pop_native(&mut stack)?;
                        let builder_value = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxBuildToken => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_build_token)
                            }
                            ExtendedInstr::SyntaxBuildTrivia => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_build_trivia)
                            }
                            ExtendedInstr::SyntaxBuildNode => {
                                std_mem::offset_of!(RawNativeFunctions, syntax_build_node)
                            }
                            _ => return Err(CompileError::Backend),
                        };
                        (
                            [builder_value, kind, value],
                            function_offset,
                            ScalarKind::Object(0),
                        )
                    }
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_heap_operation(
                    builder,
                    values,
                    function_offset,
                    arguments,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, result_kind, result)?;
            }
            Instr::Add | Instr::Sub | Instr::Mul => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let (result, overflow) = match instruction {
                    Instr::Add => builder.ins().sadd_overflow(left, right),
                    Instr::Sub => builder.ins().ssub_overflow(left, right),
                    Instr::Mul => builder.ins().smul_overflow(left, right),
                    _ => unreachable!(),
                };
                let result = if let Some(deferred) = deferred_integer_overflow.as_mut() {
                    deferred.flag = Some(match deferred.flag {
                        Some(prior) => builder.ins().bor(prior, overflow),
                        None => overflow,
                    });
                    result
                } else {
                    emit_overflow_check(
                        builder,
                        values,
                        overflow,
                        result,
                        FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        &stack,
                    )?
                };
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Div | Instr::Rem => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: segment.start + prefix,
                    prefix: fault_prefix,
                };
                let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                emit_fault_check(builder, values, zero, EXIT_DIVIDE_BY_ZERO, point, &stack)?;
                let minimum = builder.ins().iconst(types::I64, i64::MIN);
                let minimum_left = builder.ins().icmp(IntCC::Equal, left, minimum);
                let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                let overflow = builder.ins().band(minimum_left, negative_one);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTEGER_OVERFLOW,
                    point,
                    &stack,
                )?;
                let result = if matches!(instruction, Instr::Div) {
                    builder.ins().sdiv(left, right)
                } else {
                    builder.ins().srem(left, right)
                };
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Neg => {
                let value = pop_native(&mut stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (result, overflow) = builder.ins().ssub_overflow(zero, value);
                let result = if let Some(deferred) = deferred_integer_overflow.as_mut() {
                    deferred.flag = Some(match deferred.flag {
                        Some(prior) => builder.ins().bor(prior, overflow),
                        None => overflow,
                    });
                    result
                } else {
                    emit_overflow_check(
                        builder,
                        values,
                        overflow,
                        result,
                        FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        &stack,
                    )?
                };
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                let result = builder.ins().bxor_imm(value, 1);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
                let value = pop_native(&mut stack)?;
                let seed = pop_native(&mut stack)?;
                let value = builder
                    .ins()
                    .iadd_imm(value, 0x9e37_79b9_7f4a_7c15_u64 as i64);
                let value = emit_stable_hash_mix(builder, value);
                let result = if matches!(instruction, Instr::Native(NativeInstr::HashCombine)) {
                    let mixed = builder.ins().bxor(seed, value);
                    emit_stable_hash_mix(builder, mixed)
                } else {
                    builder.ins().iadd(seed, value)
                };
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = match instruction {
                    Instr::LtInt => IntCC::SignedLessThan,
                    Instr::LeInt => IntCC::SignedLessThanOrEqual,
                    Instr::GtInt => IntCC::SignedGreaterThan,
                    Instr::GeInt => IntCC::SignedGreaterThanOrEqual,
                    Instr::EqInt => IntCC::Equal,
                    Instr::NeInt => IntCC::NotEqual,
                    _ => unreachable!(),
                };
                let compared = builder.ins().icmp(condition, left, right);
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::EqBool | Instr::NeBool => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqBool) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::EqValue | Instr::NeValue => {
                let deopt_stack = stack.clone();
                let right = pop_value(&mut stack)?;
                let left = pop_value(&mut stack)?;
                let equal = emit_value_equal(
                    builder,
                    values,
                    left,
                    right,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let result = if matches!(instruction, Instr::EqValue) {
                    equal
                } else {
                    builder.ins().bxor_imm(equal, 1)
                };
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Freeze => {
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
                let result = emit_typed_object_unary(
                    builder,
                    values,
                    std_mem::offset_of!(RawNativeFunctions, freeze_graph),
                    value.bits,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(NativeValue {
                    bits: result,
                    tag: value.tag,
                });
            }
            Instr::Digest { ty } => {
                let position = segment.start + within as u32;
                let site = segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == position)
                    .ok_or(CompileError::Backend)?;
                let deopt_stack = stack.clone();
                let roots =
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let reference = pop_native(&mut stack)?;
                let frame = emit_current_frame_pointer(builder, values)?;
                let environment = load_cell_u32(
                    builder,
                    frame,
                    std_mem::offset_of!(RawNativeFrame, environment),
                )?;
                let result = emit_graph_digest(
                    builder,
                    values,
                    reference,
                    ty,
                    environment,
                    &roots,
                    ReplayEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
            }
            Instr::Native(
                operation @ (NativeInstr::EqStr
                | NativeInstr::NeStr
                | NativeInstr::TextLt
                | NativeInstr::TextLe
                | NativeInstr::TextGt
                | NativeInstr::TextGe
                | NativeInstr::EqBytes
                | NativeInstr::NeBytes
                | NativeInstr::LtBytes
                | NativeInstr::LeBytes
                | NativeInstr::GtBytes
                | NativeInstr::GeBytes),
            ) => {
                let deopt_stack = stack.clone();
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let function_offset = match operation {
                    NativeInstr::EqStr
                    | NativeInstr::NeStr
                    | NativeInstr::TextLt
                    | NativeInstr::TextLe
                    | NativeInstr::TextGt
                    | NativeInstr::TextGe => {
                        std_mem::offset_of!(RawNativeFunctions, text_compare)
                    }
                    _ => std_mem::offset_of!(RawNativeFunctions, bytes_compare),
                };
                let ordering = emit_typed_object_binary(
                    builder,
                    values,
                    function_offset,
                    left,
                    right,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let condition = match operation {
                    NativeInstr::EqStr | NativeInstr::EqBytes => IntCC::Equal,
                    NativeInstr::NeStr | NativeInstr::NeBytes => IntCC::NotEqual,
                    NativeInstr::TextLt | NativeInstr::LtBytes => IntCC::SignedLessThan,
                    NativeInstr::TextLe | NativeInstr::LeBytes => IntCC::SignedLessThanOrEqual,
                    NativeInstr::TextGt | NativeInstr::GtBytes => IntCC::SignedGreaterThan,
                    NativeInstr::TextGe | NativeInstr::GeBytes => IntCC::SignedGreaterThanOrEqual,
                    _ => return Err(CompileError::Backend),
                };
                let compared = builder.ins().icmp_imm(condition, ordering, 0);
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(operation @ (NativeInstr::TextHash | NativeInstr::BytesHash)) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let function_offset = if matches!(operation, NativeInstr::TextHash) {
                    std_mem::offset_of!(RawNativeFunctions, text_hash)
                } else {
                    std_mem::offset_of!(RawNativeFunctions, bytes_hash)
                };
                let result = emit_typed_object_unary(
                    builder,
                    values,
                    function_offset,
                    reference,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Native(
                NativeInstr::StrConcat
                | NativeInstr::StrStartsWith
                | NativeInstr::StrEndsWith
                | NativeInstr::StrContains
                | NativeInstr::StrFindIndex
                | NativeInstr::TextFindByteIndex
                | NativeInstr::TextTrim
                | NativeInstr::TextTrimStart
                | NativeInstr::TextTrimEnd
                | NativeInstr::TextToLowerAscii
                | NativeInstr::TextToUpperAscii
                | NativeInstr::TextReplace
                | NativeInstr::TextParseIntStatus
                | NativeInstr::TextParseIntValue
                | NativeInstr::TextPadStart
                | NativeInstr::TextPadEnd
                | NativeInstr::BytesEndsWith
                | NativeInstr::BytesContains
                | NativeInstr::TextSplit
                | NativeInstr::TextLines
                | NativeInstr::TextSlice
                | NativeInstr::TextSliceBytes
                | NativeInstr::TextBytes
                | NativeInstr::TextToString
                | NativeInstr::BytesText
                | NativeInstr::BbFindFrom
                | NativeInstr::BytesStartsWith
                | NativeInstr::BytesFindIndex
                | NativeInstr::BytesHex
                | NativeInstr::BytesIsUtf8,
            )
            | Instr::Numeric(
                NumericInstr::TextParseFloatStatus
                | NumericInstr::TextParseFloatValue
                | NumericInstr::FloatFixed,
            ) => {
                let position = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let roots = match segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == position)
                {
                    Some(site) => collect_native_roots(
                        builder,
                        values,
                        &plan.local_kinds,
                        &site.stack,
                        &stack,
                    )?,
                    None => Vec::new(),
                };
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset, result_kind) = match instruction {
                    Instr::Native(NativeInstr::StrConcat) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_concat),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::StrStartsWith) => {
                        let prefix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, prefix, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_starts_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrEndsWith) => {
                        let suffix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, suffix, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_ends_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrContains) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_contains),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrFindIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_find_scalar),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::TextFindByteIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_find_byte),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextTrim
                        | NativeInstr::TextTrimStart
                        | NativeInstr::TextTrimEnd),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            NativeInstr::TextTrim => {
                                std_mem::offset_of!(RawNativeFunctions, text_trim)
                            }
                            NativeInstr::TextTrimStart => {
                                std_mem::offset_of!(RawNativeFunctions, text_trim_start)
                            }
                            NativeInstr::TextTrimEnd => {
                                std_mem::offset_of!(RawNativeFunctions, text_trim_end)
                            }
                            _ => return Err(CompileError::Backend),
                        };
                        ([text, zero, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextToLowerAscii | NativeInstr::TextToUpperAscii),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextToLowerAscii)
                        {
                            std_mem::offset_of!(RawNativeFunctions, text_lower_ascii)
                        } else {
                            std_mem::offset_of!(RawNativeFunctions, text_upper_ascii)
                        };
                        ([text, zero, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(NativeInstr::TextReplace) => {
                        let replacement = pop_native(&mut stack)?;
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, replacement],
                            std_mem::offset_of!(RawNativeFunctions, text_replace),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextParseIntStatus
                        | NativeInstr::TextParseIntValue),
                    ) => {
                        let radix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset =
                            if matches!(operation, NativeInstr::TextParseIntStatus) {
                                std_mem::offset_of!(RawNativeFunctions, text_parse_int_status)
                            } else {
                                std_mem::offset_of!(RawNativeFunctions, text_parse_int_value)
                            };
                        ([text, radix, zero], function_offset, ScalarKind::Int)
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextPadStart | NativeInstr::TextPadEnd),
                    ) => {
                        let width = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextPadStart) {
                            std_mem::offset_of!(RawNativeFunctions, text_pad_start)
                        } else {
                            std_mem::offset_of!(RawNativeFunctions, text_pad_end)
                        };
                        ([text, width, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(NativeInstr::BytesEndsWith) => {
                        let suffix = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, suffix, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_ends_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::BytesContains) => {
                        let needle = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, needle, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_contains),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::TextSplit) => {
                        let separator = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, separator, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_split),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextLines) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_lines),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextSlice | NativeInstr::TextSliceBytes),
                    ) => {
                        let length = pop_native(&mut stack)?;
                        let start = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextSlice) {
                            std_mem::offset_of!(RawNativeFunctions, text_slice)
                        } else {
                            std_mem::offset_of!(RawNativeFunctions, text_slice_bytes)
                        };
                        (
                            [text, start, length],
                            function_offset,
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextBytes) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_bytes),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextToString) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, text_to_string),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BytesText) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_text),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BbFindFrom) => {
                        let start = pop_native(&mut stack)?;
                        let needle = pop_native(&mut stack)?;
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, needle, start],
                            std_mem::offset_of!(RawNativeFunctions, byte_buffer_find_from),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::BytesStartsWith) => {
                        let prefix = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, prefix, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_starts_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::BytesFindIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, needle, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_find_index),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::BytesHex) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_hex),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BytesIsUtf8) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            std_mem::offset_of!(RawNativeFunctions, bytes_is_utf8),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Numeric(
                        operation @ (NumericInstr::TextParseFloatStatus
                        | NumericInstr::TextParseFloatValue),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let (function_offset, result_kind) =
                            if matches!(operation, NumericInstr::TextParseFloatStatus) {
                                (
                                    std_mem::offset_of!(
                                        RawNativeFunctions,
                                        text_parse_float_status
                                    ),
                                    ScalarKind::Int,
                                )
                            } else {
                                (
                                    std_mem::offset_of!(RawNativeFunctions, text_parse_float_value),
                                    ScalarKind::Float,
                                )
                            };
                        ([text, zero, zero], function_offset, result_kind)
                    }
                    Instr::Numeric(NumericInstr::FloatFixed) => {
                        let digits = pop_native(&mut stack)?;
                        let value = pop_native(&mut stack)?;
                        (
                            [value, digits, zero],
                            std_mem::offset_of!(RawNativeFunctions, float_fixed),
                            ScalarKind::Object(0),
                        )
                    }
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_heap_operation(
                    builder,
                    values,
                    function_offset,
                    arguments,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, result_kind, result)?;
            }
            Instr::EqRef | Instr::NeRef => {
                let release_right = virtual_stack.last().copied().unwrap_or(false);
                let release_left = virtual_stack
                    .len()
                    .checked_sub(2)
                    .and_then(|index| virtual_stack.get(index))
                    .copied()
                    .unwrap_or(false);
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqRef) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                if release_right {
                    emit_release_pending_instance(builder, values, right)?;
                }
                if release_left {
                    emit_release_pending_instance(builder, values, left)?;
                }
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(operation) => {
                emit_char_instruction(builder, &mut stack, operation)?;
            }
            Instr::Numeric(operation) => {
                let deopt_stack = stack.clone();
                emit_numeric_instruction(
                    builder,
                    values,
                    &mut stack,
                    operation,
                    NumericExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        deopt_stack: &deopt_stack,
                    },
                )?;
            }
            Instr::Call(_)
            | Instr::CallG { .. }
            | Instr::CallVirtual { .. }
            | Instr::CallVirtualG { .. }
            | Instr::CallInterface { .. }
            | Instr::CallValue { .. }
            | Instr::Extended(ExtendedInstr::CallSlot { .. } | ExtendedInstr::NewSlot { .. })
            | Instr::Perform { .. }
            | Instr::PerformValue { .. }
            | Instr::TableEdit { .. }
            | Instr::AsCall { .. }
            | Instr::CallArgs
            | Instr::RequestOp
            | Instr::RaiseUserPanic
            | Instr::RaiseAssertionFailed
            | Instr::RaiseFault
            | Instr::Extended(ExtendedInstr::LoadSlot { .. })
            | Instr::Extended(ExtendedInstr::SendSlot { .. })
            | Instr::Extended(ExtendedInstr::PrepareWait { .. })
            | Instr::Extended(ExtendedInstr::DynRender)
            | Instr::Extended(ExtendedInstr::FunctionCode { .. })
            | Instr::Extended(ExtendedInstr::ClassCode { .. })
            | Instr::Extended(ExtendedInstr::CodeSource { .. })
            | Instr::Extended(ExtendedInstr::CodeDefinition)
            | Instr::Extended(ExtendedInstr::FaultSite { .. })
            | Instr::Extended(ExtendedInstr::FaultTrace { .. })
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Unreachable
            | Instr::Return => {}
        }
        if matches!(
            crate::instruction_treatment(&instruction).class(),
            TreatmentClass::FastPath
                | TreatmentClass::Call
                | TreatmentClass::Helper
                | TreatmentClass::Exit
        ) {
            values.heap_translations.borrow_mut().clear();
        }
        let exit_handles_stack = within + 1 == code.len()
            && matches!(
                segment.exit,
                SegmentExit::Conditional { .. }
                    | SegmentExit::Call { .. }
                    | SegmentExit::VirtualCall { .. }
                    | SegmentExit::ValueCall { .. }
                    | SegmentExit::GenericVirtualCall { .. }
                    | SegmentExit::InterfaceCall { .. }
                    | SegmentExit::SlotCall { .. }
                    | SegmentExit::Effect { .. }
                    | SegmentExit::Boundary { .. }
                    | SegmentExit::Return
            );
        if !exit_handles_stack {
            let call = matches!(instruction, Instr::Call(_) | Instr::CallG { .. })
                .then_some(segment.call_contract.as_ref())
                .flatten();
            let virtual_new =
                plan.virtual_constructor
                    .is_some_and(|constructor| match instruction {
                        Instr::New(class) | Instr::NewG { class, .. } => class == constructor.class,
                        _ => false,
                    });
            transfer_virtual_instruction(
                input.root.source,
                source_instruction,
                instruction,
                call,
                virtual_new,
                &mut virtual_locals,
                &mut virtual_stack,
            )?;
        }
        debug_assert_eq!(virtual_stack.len(), stack.len());
    }

    if let Some(deferred) = deferred_integer_overflow {
        let overflow = deferred.flag.ok_or(CompileError::Backend)?;
        emit_deferred_integer_overflow_replay(
            builder,
            values,
            overflow,
            segment.block,
            segment.start,
            reserved_prefix_cost,
            &deferred.locals,
            &deferred.stack,
        )?;
    }

    if matches!(
        segment.exit,
        SegmentExit::Call { .. }
            | SegmentExit::VirtualCall { .. }
            | SegmentExit::ValueCall { .. }
            | SegmentExit::GenericVirtualCall { .. }
            | SegmentExit::InterfaceCall { .. }
            | SegmentExit::SlotCall { .. }
    ) {
        let call_instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let contract = segment
            .call_contract
            .as_ref()
            .ok_or(CompileError::Backend)?;
        let capture = if matches!(segment.exit, SegmentExit::ValueCall { .. }) {
            let callable = stack
                .len()
                .checked_sub(
                    contract
                        .params
                        .len()
                        .checked_add(1)
                        .ok_or(CompileError::Backend)?,
                )
                .and_then(|index| stack.get(index))
                .copied()
                .ok_or(CompileError::Backend)?;
            Some(callable)
        } else {
            None
        };
        let target = match segment.exit {
            SegmentExit::Call {
                target,
                app: Some(application),
                ..
            } => {
                let site = type_environment_sites
                    .iter()
                    .find(|site| {
                        site.block == segment.block
                            && site.instruction == call_instruction
                            && site.application == application
                    })
                    .ok_or(CompileError::Backend)?;
                let environment = emit_type_environment_lookup(
                    builder,
                    values,
                    site,
                    FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix: 0,
                    },
                    &stack,
                )?;
                NativeCallTarget {
                    function: builder.ins().iconst(types::I32, i64::from(target)),
                    environment,
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: None,
                }
            }
            SegmentExit::Call {
                target, app: None, ..
            } => NativeCallTarget {
                function: builder.ins().iconst(types::I32, i64::from(target)),
                environment: builder.ins().iconst(types::I32, 0),
                capture_data: builder.ins().iconst(values.pointer_type, 0),
                capture_len: builder.ins().iconst(values.pointer_type, 0),
                fault: None,
            },
            SegmentExit::VirtualCall { selector, .. } => {
                let receiver = contract.receiver.ok_or(CompileError::Backend)?;
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let target = emit_virtual_target(
                    builder,
                    values,
                    receiver_value,
                    receiver,
                    selector,
                    FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix: 0,
                    },
                    &stack,
                )?;
                NativeCallTarget {
                    function: target,
                    environment: builder.ins().iconst(types::I32, 0),
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: None,
                }
            }
            SegmentExit::ValueCall { .. } => emit_call_value_target(
                builder,
                values,
                input.root.function,
                capture.ok_or(CompileError::Backend)?,
                contract.value_target.ok_or(CompileError::Backend)?,
                FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                },
                &stack,
            )?,
            SegmentExit::GenericVirtualCall { .. } => {
                let receiver = contract.receiver.ok_or(CompileError::Backend)?;
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let receiver = emit_generic_virtual_receiver_key(
                    builder,
                    values,
                    receiver_value,
                    receiver,
                    point,
                    &stack,
                )?;
                emit_resolved_call_lookup(
                    builder,
                    values,
                    input.root.function,
                    point,
                    receiver,
                    receiver_value,
                    EXIT_GENERIC_VIRTUAL_CALL,
                    &stack,
                )?
            }
            SegmentExit::InterfaceCall { .. } => {
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let receiver =
                    emit_interface_receiver_key(builder, values, receiver_value, point, &stack)?;
                emit_resolved_call_lookup(
                    builder,
                    values,
                    input.root.function,
                    point,
                    receiver,
                    receiver_value,
                    EXIT_INTERFACE_CALL,
                    &stack,
                )?
            }
            SegmentExit::SlotCall {
                slot,
                application,
                constructor,
                ..
            } => {
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let (function, fault) =
                    emit_image_slot_call_target(builder, values, slot, constructor)?;
                let environment = if let Some(application) = application {
                    let site = type_environment_sites
                        .iter()
                        .find(|site| {
                            site.block == segment.block
                                && site.instruction == call_instruction
                                && site.application == application
                        })
                        .ok_or(CompileError::Backend)?;
                    emit_type_environment_lookup(builder, values, site, point, &stack)?
                } else {
                    builder.ins().iconst(types::I32, 0)
                };
                NativeCallTarget {
                    function,
                    environment,
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: Some(fault),
                }
            }
            _ => return Err(CompileError::Backend),
        };
        emit_native_call(
            builder,
            values,
            &mut stack,
            NativeCallEmission {
                target,
                capture,
                fallback: if capture.is_some() {
                    NativeCallFallback::Replay
                } else {
                    NativeCallFallback::Direct
                },
                contract,
                local_kinds: &plan.local_kinds,
                boundary_kinds: &segment.boundary_stack,
                block: segment.block,
                instruction: call_instruction,
                successor_entry: u32::try_from(segment.successors[0])
                    .map_err(|_| CompileError::Backend)?,
                successor: successor_blocks[0],
            },
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Effect { .. }) {
        let effect_instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_EFFECT,
                block: segment.block,
                instruction: effect_instruction,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Boundary { .. }) {
        let instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_BOUNDARY,
                block: segment.block,
                instruction,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Unreachable) {
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_UNREACHABLE,
                block: segment.block,
                instruction: segment.end,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    match segment.exit {
        SegmentExit::Continue { .. } => {
            emit_segment_charge(builder, values, fast_segment_cost);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Jump { .. } => {
            let carries = segment
                .carry_reserved_cost
                .first()
                .copied()
                .ok_or(CompileError::Backend)?;
            if !carries {
                emit_charge(builder, values, fast_segment_cost);
            }
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Conditional { jump_on_true, .. } => {
            if segment.carry_reserved_cost.len() != 2 {
                return Err(CompileError::Backend);
            }
            let condition = pop_native(&mut stack)?;
            define_stack(builder, values, &stack)?;
            let condition = builder.ins().icmp_imm(IntCC::NotEqual, condition, 0);
            let mut target = successor_blocks[0];
            let mut fallthrough = successor_blocks[1];
            let mut charged_target = None;
            let mut charged_fallthrough = None;
            let carries_target = segment.carry_reserved_cost[0];
            let carries_fallthrough = segment.carry_reserved_cost[1];
            match (carries_target, carries_fallthrough) {
                (false, false) => emit_charge(builder, values, fast_segment_cost),
                (true, true) => {}
                (false, true) => {
                    let block = builder.create_block();
                    target = block;
                    charged_target = Some(block);
                }
                (true, false) => {
                    let block = builder.create_block();
                    fallthrough = block;
                    charged_fallthrough = Some(block);
                }
            }
            if jump_on_true {
                builder.ins().brif(condition, target, &[], fallthrough, &[]);
            } else {
                builder.ins().brif(condition, fallthrough, &[], target, &[]);
            }
            if let Some(block) = charged_target {
                builder.switch_to_block(block);
                emit_charge(builder, values, fast_segment_cost);
                builder.ins().jump(successor_blocks[0], &[]);
            }
            if let Some(block) = charged_fallthrough {
                builder.switch_to_block(block);
                emit_charge(builder, values, fast_segment_cost);
                builder.ins().jump(successor_blocks[1], &[]);
            }
        }
        SegmentExit::Call { .. } => unreachable!(),
        SegmentExit::VirtualCall { .. } => unreachable!(),
        SegmentExit::ValueCall { .. } => unreachable!(),
        SegmentExit::GenericVirtualCall { .. } => unreachable!(),
        SegmentExit::InterfaceCall { .. } => unreachable!(),
        SegmentExit::SlotCall { .. } => unreachable!(),
        SegmentExit::Allocation { .. } => {
            emit_segment_charge(builder, values, fast_segment_cost);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Effect { .. } => unreachable!(),
        SegmentExit::Boundary { .. } => unreachable!(),
        SegmentExit::Unreachable => unreachable!(),
        SegmentExit::Return => {
            emit_segment_charge(builder, values, fast_segment_cost);
            let result = pop_value(&mut stack)?;
            for (slot, pending) in virtual_locals.iter().copied().enumerate() {
                if pending {
                    let local = builder.use_var(values.locals[slot]);
                    emit_release_pending_instance(builder, values, local)?;
                }
            }
            emit_function_return(builder, values, segment.block, segment.end, result, &stack)?;
        }
    }
    Ok(())
}
