//! Fuel, replay, fault, and exit emission.

use super::*;

pub(super) fn emit_charge(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>, cost: u32) {
    let fuel = builder.use_var(values.fuel);
    let fuel = builder.ins().iadd_imm(fuel, -i64::from(cost));
    builder.def_var(values.fuel, fuel);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(cost));
    builder.def_var(values.retired, retired);
}

pub(super) fn emit_retired(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> ir::Value {
    builder.use_var(values.retired)
}

pub(super) fn emit_retired_with_prefix(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    prefix: u32,
) -> ir::Value {
    let retired = emit_retired(builder, values);
    builder.ins().iadd_imm(retired, i64::from(prefix))
}

pub(super) fn emit_segment_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    cost: u32,
) {
    emit_charge(builder, values, cost);
}

pub(super) fn emit_reservation_boundary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
    continuation: ir::Block,
) -> Result<(), CompileError> {
    let check_poll = builder.create_block();
    let check_request = builder.create_block();
    let load_request = builder.create_block();
    let rearm = builder.create_block();
    let fuel_exit = builder.create_block();
    let poll_exit = builder.create_block();
    let yield_exit = builder.create_block();
    builder.append_block_param(yield_exit, types::I32);
    builder.set_cold_block(check_poll);
    builder.set_cold_block(check_request);
    builder.set_cold_block(load_request);
    builder.set_cold_block(rearm);
    builder.set_cold_block(fuel_exit);
    builder.set_cold_block(poll_exit);
    builder.set_cold_block(yield_exit);
    let retired = emit_retired(builder, values);
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let hard_remaining = builder.ins().isub(hard_fuel, retired);
    let has_hard_fuel = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        hard_remaining,
        i64::from(segment.fuel_reserve),
    );
    builder
        .ins()
        .brif(has_hard_fuel, check_poll, &[], fuel_exit, &[]);

    builder.switch_to_block(fuel_exit);
    let fuel_kind = builder.ins().iconst(types::I32, i64::from(EXIT_FUEL));
    builder.ins().jump(yield_exit, &[fuel_kind.into()]);

    builder.switch_to_block(check_poll);
    let deadline = load_activation_u64(builder, values, RawActivationField::PollDeadline)?;
    let due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, retired, deadline);
    builder
        .ins()
        .brif(due, check_request, &[], continuation, &[]);

    builder.switch_to_block(check_request);
    let requested = load_activation_pointer(builder, values, RawActivationField::PollRequested)?;
    let enabled = builder.ins().icmp_imm(IntCC::NotEqual, requested, 0);
    builder.ins().brif(enabled, load_request, &[], rearm, &[]);

    builder.switch_to_block(load_request);
    let request = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), requested);
    let idle = builder.ins().icmp_imm(IntCC::Equal, request, 0);
    builder.ins().brif(idle, rearm, &[], poll_exit, &[]);

    builder.switch_to_block(rearm);
    emit_native_poll_rearm_values(builder, values, retired)?;
    builder.ins().jump(continuation, &[]);

    builder.switch_to_block(poll_exit);
    let poll_kind = builder.ins().iconst(types::I32, i64::from(EXIT_POLL));
    builder.ins().jump(yield_exit, &[poll_kind.into()]);

    builder.switch_to_block(yield_exit);
    let kind = builder.block_params(yield_exit)[0];
    emit_entry_exit_with_kind(builder, values, segment, EXIT_FUEL, kind)
}

pub(super) fn emit_native_poll_rearm_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    retired: ir::Value,
) -> Result<(), CompileError> {
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let remaining = builder.ins().isub(hard_fuel, retired);
    let interval = load_activation_u32(builder, values, RawActivationField::PollInterval)?;
    let interval = builder.ins().uextend(types::I64, interval);
    let use_interval = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, interval, remaining);
    let next_fuel = builder.ins().select(use_interval, interval, remaining);
    let next_deadline = builder.ins().iadd(retired, next_fuel);
    store_activation_u64(
        builder,
        values,
        RawActivationField::PollDeadline,
        next_deadline,
    )?;
    builder.def_var(values.fuel, next_fuel);
    Ok(())
}

pub(super) fn capture_local_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<Vec<NativeValue>, CompileError> {
    values
        .locals
        .iter()
        .copied()
        .enumerate()
        .map(|(slot, variable)| {
            Ok(NativeValue {
                bits: builder.use_var(variable),
                tag: emit_slot_tag(builder, values.local_tags[slot], values.local_kinds[slot])?,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_deferred_integer_overflow_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    overflow: ir::Value,
    block: u32,
    instruction: u32,
    retired_prefix: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if values.replay_failures {
        return emit_interpreter_replay(
            builder,
            values,
            overflow,
            FaultPoint {
                block,
                instruction,
                prefix: retired_prefix,
            },
            stack,
        );
    }
    let replay = builder.create_block();
    let success = builder.create_block();
    builder.set_cold_block(replay);
    builder.ins().brif(overflow, replay, &[], success, &[]);

    builder.switch_to_block(replay);
    let retired = emit_retired_with_prefix(builder, values, retired_prefix);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit_with_locals(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_REPLAY,
            block,
            instruction,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        locals,
        stack,
    )?;

    builder.switch_to_block(success);
    Ok(())
}

pub(super) fn emit_overflow_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    overflow: ir::Value,
    result: ir::Value,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    emit_fault_check(
        builder,
        values,
        overflow,
        EXIT_INTEGER_OVERFLOW,
        point,
        stack,
    )?;
    Ok(result)
}

pub(super) fn emit_fault_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    faulted: ir::Value,
    kind: u32,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if values.replay_failures {
        return emit_interpreter_replay(builder, values, faulted, point, stack);
    }
    let fault = builder.create_block();
    let success = builder.create_block();
    builder.set_cold_block(fault);
    builder.ins().brif(faulted, fault, &[], success, &[]);
    builder.switch_to_block(fault);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

pub(super) fn emit_runtime_status(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    status: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
    replay_stack: &[NativeValue],
) -> Result<(), CompileError> {
    emit_runtime_fault_status(builder, values, status, point, fault_stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)
}

pub(super) fn emit_runtime_fault_status(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    status: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let fault = builder
        .ins()
        .band_imm(status, i64::from(RUNTIME_FAULT_FLAG));
    let fault = builder.ins().icmp_imm(IntCC::NotEqual, fault, 0);
    if values.replay_failures {
        return emit_interpreter_replay(builder, values, fault, point, fault_stack);
    }
    let fault_block = builder.create_block();
    let checked = builder.create_block();
    builder.ins().brif(fault, fault_block, &[], checked, &[]);

    builder.switch_to_block(fault_block);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    let code = builder
        .ins()
        .band_imm(status, i64::from(!RUNTIME_FAULT_FLAG));
    let code = builder.ins().uextend(types::I64, code);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_GUEST_FAULT,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: code,
                tag: zero,
            },
        },
        fault_stack,
    )?;

    builder.switch_to_block(checked);
    Ok(())
}

pub(super) fn emit_runtime_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    for (slot, root) in roots.iter().copied().enumerate() {
        let value_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder.ins().store(
            MemFlags::new(),
            root.bits,
            values.root_pointer,
            value_offset,
        );
        builder.ins().store(
            MemFlags::new(),
            root.tag,
            values.root_tag_pointer,
            value_offset,
        );
        let state = root.state.unwrap_or_else(|| {
            builder
                .ins()
                .iconst(types::I8, i64::from(LOCAL_INITIALIZED))
        });
        builder.ins().store(
            MemFlags::new(),
            state,
            values.root_state_pointer,
            state_offset,
        );
    }
    Ok(builder.ins().iconst(
        types::I32,
        i64::try_from(roots.len()).map_err(|_| CompileError::Backend)?,
    ))
}

pub(super) fn emit_interpreter_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    replay: ir::Value,
    _point: FaultPoint,
    _stack: &[NativeValue],
) -> Result<(), CompileError> {
    let replay_block = values.replay_blocks.first().ok_or(CompileError::Backend)?;
    replay_block.used.set(true);
    let success = builder.create_block();
    builder
        .ins()
        .brif(replay, replay_block.block, &[], success, &[]);
    builder.switch_to_block(success);
    Ok(())
}

pub(super) fn emit_pending_instance_barrier(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let pending = builder.ins().icmp_imm(IntCC::NotEqual, available, -1);
    emit_interpreter_replay(builder, values, pending, point, stack)
}

pub(super) fn emit_exit(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let locals = capture_local_values(builder, values)?;
    emit_exit_with_locals(builder, values, exit, &locals, stack)
}

pub(super) fn emit_exit_with_locals(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let kind = builder.ins().iconst(types::I32, i64::from(exit.kind));
    emit_exit_with_locals_and_kind(builder, values, exit, kind, locals, stack)
}

pub(super) fn emit_exit_with_locals_and_kind(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    kind: ir::Value,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if exit.kind == EXIT_RETURN {
        emit_release_scalar_charges(builder, values)?;
    } else {
        emit_scalar_deopt_records(builder, values)?;
    }
    let storage = reload_active_frame_storage(builder, values)?;
    let stack_kinds = crate::decode_exit_kind(exit.kind)
        .and_then(|kind| {
            values
                .plan
                .materialization_operand_kinds(kind, exit.block, exit.instruction)
        })
        .filter(|kinds| kinds.len() == stack.len());
    emit_spill_frame_values(
        builder,
        storage,
        exit.block,
        exit.instruction,
        locals,
        stack,
        stack_kinds,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, retired),
        exit.retired,
    )?;
    store_i32_value(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, kind),
        kind,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, block),
        exit.block,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, instruction),
        exit.instruction,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, stack_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result_tag),
        exit.result.tag,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result),
        exit.result.bits,
    )?;
    builder.ins().return_(&[]);
    Ok(())
}

pub(super) fn reload_active_frame_storage<'a>(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'a>,
) -> Result<NativeValues<'a>, CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    let scalar_base = load_cell_u32(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, scalar_base),
    )?;
    let scalar_base = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_offset = builder.ins().ishl_imm(scalar_base, 3);
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let local_pointer = builder.ins().iadd(scalars, scalar_offset);
    let local_tag_pointer = builder.ins().iadd(tags, scalar_offset);
    let local_state_pointer = builder.ins().iadd(states, scalar_base);
    let local_bytes = i64::try_from(
        values
            .locals
            .len()
            .checked_mul(8)
            .ok_or(CompileError::Backend)?,
    )
    .map_err(|_| CompileError::Backend)?;
    let stack_pointer = builder.ins().iadd_imm(local_pointer, local_bytes);
    let stack_tag_pointer = builder.ins().iadd_imm(local_tag_pointer, local_bytes);
    Ok(NativeValues {
        local_pointer,
        local_tag_pointer,
        local_state_pointer,
        stack_pointer,
        stack_tag_pointer,
        ..values
    })
}

pub(super) fn emit_function_return(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    result: NativeValue,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let normal = builder.create_block();
    let direct = builder.create_block();
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let has_parent = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, frame_len, 1);
    builder.ins().brif(has_parent, direct, &[], normal, &[]);

    builder.switch_to_block(direct);
    let retired = emit_retired(builder, values);
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, retired),
        retired,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, kind),
        EXIT_RETURN,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result_tag),
        result.tag,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result),
        result.bits,
    )?;
    builder.ins().return_(&[]);

    builder.switch_to_block(normal);
    let retired = emit_retired(builder, values);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_RETURN,
            block,
            instruction,
            result,
        },
        stack,
    )
}

pub(super) fn emit_spill_frame_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
    stack_kinds: Option<&[ScalarKind]>,
) -> Result<(), CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    emit_spill_frame_values_to(
        builder,
        values,
        frame,
        block,
        instruction,
        locals,
        stack,
        stack_kinds,
    )
}

pub(super) fn emit_spill_frame_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if local_kinds.len() != values.locals.len() || stack_kinds.len() != stack.len() {
        return Err(CompileError::Backend);
    }
    // Keep every scanned tag canonical when a later call reuses this frame window.
    for (slot, kind) in local_kinds.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let tag = emit_slot_tag(builder, values.local_tags[slot], kind)?;
        builder
            .ins()
            .store(MemFlags::new(), tag, values.local_tag_pointer, offset);
        if is_root_kind(kind) {
            let bits = builder.use_var(values.locals[slot]);
            builder
                .ins()
                .store(MemFlags::new(), bits, values.local_pointer, offset);
        }
    }
    for (slot, (kind, value)) in stack_kinds
        .iter()
        .copied()
        .zip(stack.iter().copied())
        .enumerate()
    {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value.tag, values.stack_tag_pointer, offset);
        if is_root_kind(kind) {
            builder
                .ins()
                .store(MemFlags::new(), value.bits, values.stack_pointer, offset);
        }
    }
    store_i32_constant(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, operand_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )
}

pub(super) fn emit_spill_frame_to(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    block: u32,
    instruction: u32,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let locals = capture_local_values(builder, values)?;
    let stack_kinds = values
        .plan
        .suspended_operand_kinds(block, instruction)
        .filter(|kinds| kinds.len() == stack.len());
    emit_spill_frame_values_to(
        builder,
        values,
        frame,
        block,
        instruction,
        &locals,
        stack,
        stack_kinds,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_spill_frame_values_to(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    block: u32,
    instruction: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
    stack_kinds: Option<&[ScalarKind]>,
) -> Result<(), CompileError> {
    if locals.len() != values.locals.len()
        || values
            .dirty_locals
            .is_some_and(|dirty_locals| dirty_locals.len() != locals.len())
    {
        return Err(CompileError::Backend);
    }
    for (slot, (kind, value)) in values
        .local_kinds
        .iter()
        .copied()
        .zip(locals.iter().copied())
        .enumerate()
    {
        if values
            .dirty_locals
            .is_some_and(|dirty_locals| !dirty_locals[slot])
        {
            continue;
        }
        let local_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder.ins().store(
            MemFlags::new(),
            value.bits,
            values.local_pointer,
            local_offset,
        );
        if value_tag(kind).is_none() {
            builder.ins().store(
                MemFlags::new(),
                value.tag,
                values.local_tag_pointer,
                local_offset,
            );
        }
    }
    for (slot, value) in stack.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value.bits, values.stack_pointer, offset);
        if stack_kinds
            .and_then(|kinds| kinds.get(slot).copied())
            .and_then(value_tag)
            .is_none()
        {
            builder
                .ins()
                .store(MemFlags::new(), value.tag, values.stack_tag_pointer, offset);
        }
    }
    store_i32_constant(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, block),
        block,
    )?;
    store_i32_constant(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, instruction),
        instruction,
    )?;
    store_i32_constant(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, operand_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )?;
    Ok(())
}

pub(super) fn emit_current_frame_pointer(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<ir::Value, CompileError> {
    let frames = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, frames),
    )?;
    let frame_len = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, frame_len),
    )?;
    let frame_index = builder.ins().iadd_imm(frame_len, -1);
    let frame_index = builder.ins().uextend(values.pointer_type, frame_index);
    let offset = builder
        .ins()
        .imul_imm(frame_index, std_mem::size_of::<RawNativeFrame>() as i64);
    Ok(builder.ins().iadd(frames, offset))
}

pub(super) fn define_stack(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if stack.len() > values.stack.len() {
        return Err(CompileError::Backend);
    }
    for (slot, (variable, value)) in values
        .stack
        .iter()
        .copied()
        .zip(stack.iter().copied())
        .enumerate()
    {
        builder.def_var(variable, value.bits);
        if let Some(tag) = values.stack_tags[slot] {
            builder.def_var(tag, value.tag);
        }
    }
    Ok(())
}
