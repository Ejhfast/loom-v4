//! Native call and dispatch-cache emission.

use super::*;

pub(super) fn emit_type_environment_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    site: &TypeEnvironmentSite,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    emit_type_cache_lookup(
        builder,
        values,
        site.function,
        point,
        TypeCacheRequest::Environment,
        stack,
    )
}

pub(super) fn emit_interface_receiver_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let object = builder.create_block();
    let immediate = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let is_object = builder
        .ins()
        .icmp_imm(IntCC::Equal, receiver.tag, ValueTag::Obj as u64 as i64);
    builder.ins().brif(is_object, object, &[], immediate, &[]);

    builder.switch_to_block(immediate);
    builder.ins().jump(done, &[receiver.tag.into()]);

    builder.switch_to_block(object);
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let entry = emit_heap_entry(
        builder,
        values,
        receiver.bits,
        guard_point,
        ObjectGuard::Replay(stack),
    )?;
    let object_tag = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let object_key = builder.ins().uextend(types::I64, object_tag);
    let object_key = builder.ins().bor_imm(object_key, 1_i64 << 62);
    let instance = builder.create_block();
    let other_object = builder.create_block();
    let is_instance =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, object_tag, i64::from(JIT_OBJECT_INSTANCE));
    builder
        .ins()
        .brif(is_instance, instance, &[], other_object, &[]);

    builder.switch_to_block(instance);
    let class = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let class_key = builder.ins().uextend(types::I64, class);
    let class_key = builder.ins().bor_imm(class_key, i64::MIN);
    builder.ins().jump(done, &[class_key.into()]);

    builder.switch_to_block(other_object);
    builder.ins().jump(done, &[object_key.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_image_slot_call_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    slot: u32,
    constructor: bool,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I32);

    let count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, image_slot_count),
    )?;
    let slot_index = builder.ins().iconst(values.pointer_type, i64::from(slot));
    let in_range = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, count, slot_index);
    let zero = builder.ins().iconst(types::I32, 0);
    let invalid = builder.ins().iconst(
        types::I32,
        i64::from(abi_fault_index(lm_abi::FaultCode::InvalidVmState)? + 1),
    );
    builder.ins().brif(in_range, present, &[], missing, &[]);

    builder.switch_to_block(missing);
    builder.ins().jump(done, &[zero.into(), invalid.into()]);

    builder.switch_to_block(present);
    let slots = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, image_slots),
    )?;
    let offset = builder.ins().imul_imm(
        slot_index,
        i64::try_from(std_mem::size_of::<NativeImageSlot>()).map_err(|_| CompileError::Backend)?,
    );
    let address = builder.ins().iadd(slots, offset);
    let kind = load_value(
        builder,
        types::I32,
        address,
        std_mem::offset_of!(NativeImageSlot, kind),
    )?;
    let target_offset = if constructor {
        std_mem::offset_of!(NativeImageSlot, second)
    } else {
        std_mem::offset_of!(NativeImageSlot, first)
    };
    let target = load_value(builder, types::I32, address, target_offset)?;
    let expected_kind = if constructor {
        IMAGE_SLOT_CLASS
    } else {
        IMAGE_SLOT_FUNCTION
    };
    let valid = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(expected_kind));
    let empty = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(IMAGE_SLOT_EMPTY));
    let malformed = builder.ins().iconst(
        types::I32,
        i64::from(abi_fault_index(lm_abi::FaultCode::MalformedState)? + 1),
    );
    let fault = builder.ins().select(empty, invalid, malformed);
    let fault = builder.ins().select(valid, zero, fault);
    builder.ins().jump(done, &[target.into(), fault.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

pub(super) fn abi_fault_index(fault: lm_abi::FaultCode) -> Result<u32, CompileError> {
    lm_abi::FAULT_CODES
        .iter()
        .position(|candidate| *candidate == fault)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(CompileError::Backend)
}

pub(super) fn emit_generic_virtual_receiver_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    contract: VirtualReceiver,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let VirtualReceiver::Instance { class } = contract else {
        return Err(CompileError::Backend);
    };
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let (entry, actual) = emit_instance_entry(
        builder,
        values,
        receiver.bits,
        class,
        guard_point,
        ObjectGuard::Replay(stack),
        ObjectGuard::Replay(stack),
    )?;
    let environment = load_value(builder, types::I32, entry, JIT_INSTANCE_ENV_OFFSET)?;
    let class_key = builder.ins().uextend(types::I64, actual);
    let environment_key = builder.ins().uextend(types::I64, environment);
    let environment_key = builder.ins().ishl_imm(environment_key, 32);
    Ok(builder.ins().bor(environment_key, class_key))
}

pub(super) fn emit_call_value_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    callable: NativeValue,
    target: ValueCallTarget,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<NativeCallTarget, CompileError> {
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    match target {
        ValueCallTarget::Closure => {
            let wrong_tag =
                builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, callable.tag, ValueTag::Obj as u64 as i64);
            emit_interpreter_replay(builder, values, wrong_tag, guard_point, stack)?;
            let entry = emit_object_entry(
                builder,
                values,
                callable.bits,
                JIT_OBJECT_CLOSURE,
                guard_point,
                ObjectGuard::Replay(stack),
            )?;
            let function = load_value(builder, types::I32, entry, JIT_CLOSURE_FUNCTION_OFFSET)?;
            let environment = load_value(builder, types::I32, entry, JIT_CLOSURE_ENV_OFFSET)?;
            let capture_data = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_DATA_OFFSET,
            )?;
            let capture_len = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_LEN_OFFSET,
            )?;
            Ok(NativeCallTarget {
                function,
                environment,
                capture_data,
                capture_len,
                fault: None,
            })
        }
        ValueCallTarget::Callback => {
            let closure = builder.create_block();
            let test_callback = builder.create_block();
            let callback = builder.create_block();
            let invalid = builder.create_block();
            let done = builder.create_block();
            builder.append_block_param(done, types::I32);
            builder.append_block_param(done, types::I32);
            builder.append_block_param(done, values.pointer_type);
            builder.append_block_param(done, values.pointer_type);
            let is_closure =
                builder
                    .ins()
                    .icmp_imm(IntCC::Equal, callable.tag, ValueTag::Obj as u64 as i64);
            builder
                .ins()
                .brif(is_closure, closure, &[], test_callback, &[]);

            builder.switch_to_block(test_callback);
            let is_callback = builder.ins().icmp_imm(
                IntCC::Equal,
                callable.tag,
                ValueTag::Callback as u64 as i64,
            );
            builder.ins().brif(is_callback, callback, &[], invalid, &[]);

            builder.switch_to_block(invalid);
            let retired = emit_retired(builder, values);
            let zero = builder.ins().iconst(types::I64, 0);
            emit_exit(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_REPLAY,
                    block: point.block,
                    instruction: point.instruction,
                    result: NativeValue {
                        bits: zero,
                        tag: zero,
                    },
                },
                stack,
            )?;

            builder.switch_to_block(closure);
            let entry = emit_object_entry(
                builder,
                values,
                callable.bits,
                JIT_OBJECT_CLOSURE,
                guard_point,
                ObjectGuard::Replay(stack),
            )?;
            let closure_function =
                load_value(builder, types::I32, entry, JIT_CLOSURE_FUNCTION_OFFSET)?;
            let closure_environment =
                load_value(builder, types::I32, entry, JIT_CLOSURE_ENV_OFFSET)?;
            let closure_capture_data = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_DATA_OFFSET,
            )?;
            let closure_capture_len = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_LEN_OFFSET,
            )?;
            builder.ins().jump(
                done,
                &[
                    closure_function.into(),
                    closure_environment.into(),
                    closure_capture_data.into(),
                    closure_capture_len.into(),
                ],
            );

            builder.switch_to_block(callback);
            let callback_target = emit_resolved_call_lookup(
                builder,
                values,
                function,
                point,
                callable.bits,
                callable,
                EXIT_CALLBACK_CALL,
                stack,
            )?;
            builder.ins().jump(
                done,
                &[
                    callback_target.function.into(),
                    callback_target.environment.into(),
                    callback_target.capture_data.into(),
                    callback_target.capture_len.into(),
                ],
            );

            builder.switch_to_block(done);
            let values = builder.block_params(done);
            Ok(NativeCallTarget {
                function: values[0],
                environment: values[1],
                capture_data: values[2],
                capture_len: values[3],
                fault: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_resolved_call_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    point: FaultPoint,
    receiver_key: ir::Value,
    receiver: NativeValue,
    exit_kind: u32,
    stack: &[NativeValue],
) -> Result<NativeCallTarget, CompileError> {
    let hit = builder.create_block();
    let miss = builder.create_block();
    builder.append_block_param(hit, types::I32);
    builder.append_block_param(hit, types::I32);
    builder.append_block_param(hit, values.pointer_type);
    builder.append_block_param(hit, values.pointer_type);
    let store = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, type_store_id),
    )?;
    let frame = emit_current_frame_pointer(builder, values)?;
    let parent = load_cell_u32(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, environment),
    )?;
    let cache = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, resolved_calls),
    )?;
    let mask = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, resolved_call_mask),
    )?;
    let shifted_parent = builder.ins().ushr_imm(parent, 16);
    let parent_hash = builder.ins().bxor(parent, shifted_parent);
    let shifted_receiver = builder.ins().ushr_imm(receiver_key, 32);
    let receiver_hash = builder.ins().bxor(receiver_key, shifted_receiver);
    let receiver_hash = builder.ins().ireduce(types::I32, receiver_hash);
    let rotated_receiver = builder.ins().rotl_imm(receiver_hash, 7);
    let receiver_hash = builder.ins().bxor(receiver_hash, rotated_receiver);
    let site_hash =
        crate::activation::type_environment_site_hash(function, point.block, point.instruction);
    let site_hash = builder.ins().iconst(types::I32, i64::from(site_hash));
    let set = builder.ins().bxor(site_hash, parent_hash);
    let set = builder.ins().bxor(set, receiver_hash);
    let set = builder.ins().band(set, mask);
    let set = builder.ins().uextend(values.pointer_type, set);
    let first = builder.ins().imul_imm(
        set,
        (RESOLVED_CALL_CACHE_WAYS * std_mem::size_of::<RawResolvedCallCacheEntry>()) as i64,
    );
    let first = builder.ins().iadd(cache, first);
    for index in 0..RESOLVED_CALL_CACHE_WAYS {
        let next = builder.create_block();
        let entry_offset = index
            .checked_mul(std_mem::size_of::<RawResolvedCallCacheEntry>())
            .ok_or(CompileError::Backend)?;
        let entry_offset = i64::try_from(entry_offset).map_err(|_| CompileError::Backend)?;
        let entry = builder.ins().iadd_imm(first, entry_offset);
        let cached_store = atomic_load_field(
            builder,
            entry,
            types::I64,
            std_mem::offset_of!(RawResolvedCallCacheEntry, store),
        )?;
        let cached_function = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, function),
        )?;
        let cached_block = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, block),
        )?;
        let cached_instruction = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, instruction),
        )?;
        let cached_parent = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, parent),
        )?;
        let cached_receiver = atomic_load_field(
            builder,
            entry,
            types::I64,
            std_mem::offset_of!(RawResolvedCallCacheEntry, receiver),
        )?;
        let target = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, target),
        )?;
        let environment = atomic_load_field(
            builder,
            entry,
            types::I32,
            std_mem::offset_of!(RawResolvedCallCacheEntry, environment),
        )?;
        let capture_data = atomic_load_field(
            builder,
            entry,
            values.pointer_type,
            std_mem::offset_of!(RawResolvedCallCacheEntry, capture_data),
        )?;
        let capture_len = atomic_load_field(
            builder,
            entry,
            values.pointer_type,
            std_mem::offset_of!(RawResolvedCallCacheEntry, capture_len),
        )?;
        let same_store = builder.ins().icmp(IntCC::Equal, cached_store, store);
        let same_function =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, cached_function, i64::from(function));
        let same_block = builder
            .ins()
            .icmp_imm(IntCC::Equal, cached_block, i64::from(point.block));
        let same_instruction = builder.ins().icmp_imm(
            IntCC::Equal,
            cached_instruction,
            i64::from(point.instruction),
        );
        let same_parent = builder.ins().icmp(IntCC::Equal, cached_parent, parent);
        let same_receiver = builder
            .ins()
            .icmp(IntCC::Equal, cached_receiver, receiver_key);
        let matched = builder.ins().band(same_store, same_function);
        let matched = builder.ins().band(matched, same_block);
        let matched = builder.ins().band(matched, same_instruction);
        let matched = builder.ins().band(matched, same_parent);
        let matched = builder.ins().band(matched, same_receiver);
        builder.ins().brif(
            matched,
            hit,
            &[
                target.into(),
                environment.into(),
                capture_data.into(),
                capture_len.into(),
            ],
            next,
            &[],
        );
        builder.switch_to_block(next);
    }
    builder.ins().jump(miss, &[]);

    builder.switch_to_block(miss);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: exit_kind,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: receiver_key,
                tag: receiver.tag,
            },
        },
        stack,
    )?;

    builder.switch_to_block(hit);
    let values = builder.block_params(hit);
    Ok(NativeCallTarget {
        function: values[0],
        environment: values[1],
        capture_data: values[2],
        capture_len: values[3],
        fault: None,
    })
}

pub(super) fn atomic_load_field(
    builder: &mut FunctionBuilder<'_>,
    base: ir::Value,
    ty: ir::Type,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    let address = builder.ins().iadd_imm(
        base,
        i64::try_from(offset).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().atomic_load(ty, MemFlags::new(), address))
}

#[derive(Clone, Copy)]
pub(super) enum TypeCacheRequest {
    Environment,
    OptionFamily { ty: u32 },
}

pub(super) fn emit_type_cache_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    point: FaultPoint,
    request: TypeCacheRequest,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let hit = builder.create_block();
    let miss = builder.create_block();
    builder.append_block_param(hit, types::I32);
    let store = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, type_store_id),
    )?;
    let frame = emit_current_frame_pointer(builder, values)?;
    let parent = load_cell_u32(
        builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, environment),
    )?;
    let cache = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, type_environments),
    )?;
    let mask = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, type_environment_mask),
    )?;
    let shifted_parent = builder.ins().ushr_imm(parent, 16);
    let parent_hash = builder.ins().bxor(parent, shifted_parent);
    let site_hash =
        crate::activation::type_environment_site_hash(function, point.block, point.instruction);
    let site_hash = builder.ins().iconst(types::I32, i64::from(site_hash));
    let set = builder.ins().bxor(site_hash, parent_hash);
    let set = builder.ins().band(set, mask);
    let set = builder.ins().uextend(values.pointer_type, set);
    let first = builder.ins().imul_imm(
        set,
        (TYPE_ENVIRONMENT_CACHE_WAYS * std_mem::size_of::<RawTypeEnvironmentCacheEntry>()) as i64,
    );
    let first = builder.ins().iadd(cache, first);
    for index in 0..TYPE_ENVIRONMENT_CACHE_WAYS {
        let next = builder.create_block();
        let entry_offset = index
            .checked_mul(std_mem::size_of::<RawTypeEnvironmentCacheEntry>())
            .ok_or(CompileError::Backend)?;
        let entry_offset = i64::try_from(entry_offset).map_err(|_| CompileError::Backend)?;
        let entry = builder.ins().iadd_imm(first, entry_offset);
        let store_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(RawTypeEnvironmentCacheEntry, store))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_store = builder
            .ins()
            .atomic_load(types::I64, MemFlags::new(), store_address);
        let function_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(RawTypeEnvironmentCacheEntry, function))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_function =
            builder
                .ins()
                .atomic_load(types::I32, MemFlags::new(), function_address);
        let block_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(RawTypeEnvironmentCacheEntry, block))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_block = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), block_address);
        let instruction_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(
                RawTypeEnvironmentCacheEntry,
                instruction
            ))
            .map_err(|_| CompileError::Backend)?,
        );
        let cached_instruction =
            builder
                .ins()
                .atomic_load(types::I32, MemFlags::new(), instruction_address);
        let parent_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(RawTypeEnvironmentCacheEntry, parent))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_parent = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), parent_address);
        let child_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(std_mem::offset_of!(RawTypeEnvironmentCacheEntry, child))
                .map_err(|_| CompileError::Backend)?,
        );
        let child = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), child_address);
        let same_store = builder.ins().icmp(IntCC::Equal, cached_store, store);
        let same_function =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, cached_function, i64::from(function));
        let same_block = builder
            .ins()
            .icmp_imm(IntCC::Equal, cached_block, i64::from(point.block));
        let same_instruction = builder.ins().icmp_imm(
            IntCC::Equal,
            cached_instruction,
            i64::from(point.instruction),
        );
        let same_parent = builder.ins().icmp(IntCC::Equal, cached_parent, parent);
        let matched = builder.ins().band(same_store, same_function);
        let matched = builder.ins().band(matched, same_block);
        let matched = builder.ins().band(matched, same_instruction);
        let matched = builder.ins().band(matched, same_parent);
        builder.ins().brif(matched, hit, &[child.into()], next, &[]);
        builder.switch_to_block(next);
    }
    builder.ins().jump(miss, &[]);

    builder.switch_to_block(miss);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    let parent_bits = builder.ins().uextend(types::I64, parent);
    let (kind, bits) = match request {
        TypeCacheRequest::Environment => (EXIT_TYPE_ENVIRONMENT, parent_bits),
        TypeCacheRequest::OptionFamily { ty } => (
            EXIT_TYPE_RESOLUTION,
            builder.ins().iconst(types::I64, i64::from(ty)),
        ),
    };
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits,
                tag: parent_bits,
            },
        },
        stack,
    )?;

    builder.switch_to_block(hit);
    Ok(builder.block_params(hit)[0])
}

pub(super) fn emit_native_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &mut Vec<NativeValue>,
    call: NativeCallEmission<'_>,
) -> Result<(), CompileError> {
    let NativeCallEmission {
        target,
        capture,
        fallback: fallback_kind,
        contract,
        local_kinds,
        boundary_kinds,
        block,
        instruction,
        successor_entry,
        successor,
    } = call;
    let NativeCallTarget {
        function: target,
        environment,
        capture_data,
        capture_len,
        fault,
    } = target;
    let argument_start = stack
        .len()
        .checked_sub(contract.params.len())
        .ok_or(CompileError::Backend)?;
    let caller_end = argument_start
        .checked_sub(usize::from(capture.is_some()))
        .ok_or(CompileError::Backend)?;
    let boundary_stack = stack.clone();
    let caller_stack = stack[..caller_end].to_vec();
    if boundary_kinds.len() != boundary_stack.len() {
        return Err(CompileError::Backend);
    }
    let caller_stack_kinds = &boundary_kinds[..caller_end];
    let arguments = stack[argument_start..].to_vec();
    let stack_limit_stack = if capture.is_some() {
        caller_stack
            .iter()
            .chain(arguments.iter())
            .copied()
            .collect::<Vec<_>>()
    } else {
        boundary_stack.clone()
    };
    if let Some(scalar) = contract.scalar_result.as_ref() {
        let scalar_path = builder.create_block();
        let native_path = builder.create_block();
        let ready = emit_scalar_replacement_guard(builder, values, scalar)?;
        builder
            .ins()
            .brif(ready, scalar_path, &[], native_path, &[]);

        builder.switch_to_block(scalar_path);
        emit_scalar_replacement(
            builder,
            values,
            scalar,
            &arguments,
            &caller_stack,
            successor,
        )?;

        builder.switch_to_block(native_path);
    }
    let hard_check = builder.create_block();
    let fuel_exit = builder.create_block();
    let lookup = builder.create_block();
    let fallback = builder.create_block();
    let stack_rollover = builder.create_block();
    let root_check = builder.create_block();
    let grow_roots = contract
        .behavior
        .may_collect()
        .then(|| builder.create_block());
    let stack_limit = builder.create_block();
    let capacity = builder.create_block();
    let storage = builder.create_block();
    let grow = builder.create_block();
    let invoke = builder.create_block();
    let returned = builder.create_block();
    let propagate = builder.create_block();
    let preflight_exit = builder.create_block();
    builder.append_block_param(preflight_exit, types::I32);
    builder.append_block_param(preflight_exit, types::I64);
    builder.append_block_param(preflight_exit, types::I64);

    builder.set_cold_block(hard_check);
    builder.set_cold_block(fuel_exit);
    builder.set_cold_block(preflight_exit);
    builder.set_cold_block(stack_rollover);
    let fuel = builder.use_var(values.fuel);
    let has_fuel = builder
        .ins()
        .icmp_imm(IntCC::SignedGreaterThanOrEqual, fuel, 1);
    builder.ins().brif(has_fuel, lookup, &[], hard_check, &[]);

    builder.switch_to_block(hard_check);
    let retired = emit_retired(builder, values);
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let remaining = builder.ins().isub(hard_fuel, retired);
    let has_hard_fuel = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, remaining, 1);
    builder
        .ins()
        .brif(has_hard_fuel, lookup, &[], fuel_exit, &[]);

    builder.switch_to_block(fuel_exit);
    let kind = builder.ins().iconst(types::I32, i64::from(EXIT_FUEL));
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .jump(preflight_exit, &[kind.into(), zero.into(), zero.into()]);

    builder.switch_to_block(lookup);
    if let Some(fault) = fault {
        let invalid = builder.ins().icmp_imm(IntCC::NotEqual, fault, 0);
        let fault_block = builder.create_block();
        let valid_block = builder.create_block();
        builder
            .ins()
            .brif(invalid, fault_block, &[], valid_block, &[]);

        builder.switch_to_block(fault_block);
        emit_charge(builder, values, 1);
        let retired = emit_retired(builder, values);
        let code = builder.ins().iadd_imm(fault, -1);
        let code = builder.ins().uextend(types::I64, code);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_GUEST_FAULT,
                block,
                instruction: instruction + 1,
                result: NativeValue {
                    bits: code,
                    tag: zero,
                },
            },
            &boundary_stack,
        )?;

        builder.switch_to_block(valid_block);
    }
    let entry_count = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, entry_count),
    )?;
    let target_in_range = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, entry_count, target);
    let have_target = builder.create_block();
    builder
        .ins()
        .brif(target_in_range, have_target, &[], fallback, &[]);

    builder.switch_to_block(have_target);
    let entries = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, entries),
    )?;
    let target_index = builder.ins().uextend(values.pointer_type, target);
    let entry_offset = builder
        .ins()
        .imul_imm(target_index, std_mem::size_of::<usize>() as i64);
    let entry_address = builder.ins().iadd(entries, entry_offset);
    let cell = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), entry_address, 0);
    let code = builder
        .ins()
        .atomic_load(values.pointer_type, MemFlags::new(), cell);
    let published = builder.ins().icmp_imm(IntCC::NotEqual, code, 0);
    let limits = builder.create_block();
    builder
        .ins()
        .brif(published, root_check, &[], fallback, &[]);

    builder.switch_to_block(root_check);
    if let Some(grow_roots) = grow_roots {
        let required_roots = load_cell_u32(
            builder,
            cell,
            std_mem::offset_of!(NativeEntryCell, max_roots),
        )?;
        let root_capacity = load_activation_u32(builder, values, RawActivationField::RootCapacity)?;
        let roots_fit = builder.ins().icmp(
            IntCC::UnsignedLessThanOrEqual,
            required_roots,
            root_capacity,
        );
        builder.ins().brif(roots_fit, limits, &[], grow_roots, &[]);

        builder.switch_to_block(grow_roots);
        let kind = builder.ins().iconst(types::I32, i64::from(EXIT_GROW_ROOTS));
        let required_roots = builder.ins().uextend(types::I64, required_roots);
        let zero = builder.ins().iconst(types::I64, 0);
        builder.ins().jump(
            preflight_exit,
            &[kind.into(), required_roots.into(), zero.into()],
        );
    } else {
        builder.ins().jump(limits, &[]);
    }

    builder.switch_to_block(limits);
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let base_frames = load_activation_u32(builder, values, RawActivationField::BaseFrames)?;
    let max_frames = load_activation_u32(builder, values, RawActivationField::MaxFrames)?;
    let total_frames = builder.ins().iadd(base_frames, frame_len);
    let frame_overflow =
        builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, total_frames, max_frames);
    let native_stack_check = builder.create_block();
    let stack_check = builder.create_block();
    builder
        .ins()
        .brif(frame_overflow, stack_limit, &[], native_stack_check, &[]);

    builder.switch_to_block(native_stack_check);
    let frames = load_activation_pointer(builder, values, RawActivationField::Frames)?;
    let active_frame_index = builder.ins().iadd_imm(frame_len, -1);
    let active_frame_index = builder
        .ins()
        .uextend(values.pointer_type, active_frame_index);
    let active_frame_offset = builder.ins().imul_imm(
        active_frame_index,
        std_mem::size_of::<RawNativeFrame>() as i64,
    );
    let active_frame = builder.ins().iadd(frames, active_frame_offset);
    let native_stack_bytes = load_cell_u32(
        builder,
        active_frame,
        std_mem::offset_of!(RawNativeFrame, native_stack_bytes),
    )?;
    let callee_stack_bytes = load_cell_u32(
        builder,
        cell,
        std_mem::offset_of!(NativeEntryCell, native_stack_bytes),
    )?;
    let next_native_stack_bytes = builder.ins().iadd(native_stack_bytes, callee_stack_bytes);
    let native_stack_wrapped = builder.ins().icmp(
        IntCC::UnsignedLessThan,
        next_native_stack_bytes,
        native_stack_bytes,
    );
    let native_stack_exceeded = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThan,
        next_native_stack_bytes,
        i64::from(crate::NATIVE_STACK_BUDGET),
    );
    let native_stack_blocked = builder
        .ins()
        .bor(native_stack_wrapped, native_stack_exceeded);
    builder
        .ins()
        .brif(native_stack_blocked, stack_rollover, &[], stack_check, &[]);

    builder.switch_to_block(stack_rollover);
    let kind = builder
        .ins()
        .iconst(types::I32, i64::from(EXIT_STACK_ROLLOVER));
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .jump(preflight_exit, &[kind.into(), zero.into(), zero.into()]);

    builder.switch_to_block(stack_check);
    let caller_prefix = load_cell_u32(
        builder,
        active_frame,
        std_mem::offset_of!(RawNativeFrame, caller_stack_values),
    )?;
    let active_local_count = load_cell_u32(
        builder,
        active_frame,
        std_mem::offset_of!(RawNativeFrame, local_count),
    )?;
    let active_values = builder.ins().iadd(caller_prefix, active_local_count);
    let active_values = builder.ins().iadd_imm(
        active_values,
        i64::try_from(boundary_stack.len()).map_err(|_| CompileError::Backend)?,
    );
    let caller_values = builder.ins().iadd_imm(
        active_values,
        -i64::try_from(
            contract
                .params
                .len()
                .checked_add(usize::from(capture.is_some()))
                .ok_or(CompileError::Backend)?,
        )
        .map_err(|_| CompileError::Backend)?,
    );
    let local_count = load_cell_u32(
        builder,
        cell,
        std_mem::offset_of!(NativeEntryCell, local_count),
    )?;
    let pushed_values = builder.ins().iadd(caller_values, local_count);
    let max_values = load_activation_u32(builder, values, RawActivationField::MaxStackValues)?;
    let stack_overflow = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, pushed_values, max_values);
    builder
        .ins()
        .brif(stack_overflow, stack_limit, &[], capacity, &[]);

    builder.switch_to_block(stack_limit);
    emit_charge(builder, values, 1);
    let retired = emit_retired(builder, values);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_STACK_LIMIT,
            block,
            instruction: instruction + 1,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        &stack_limit_stack,
    )?;

    builder.switch_to_block(capacity);
    let max_stack = load_cell_u32(
        builder,
        cell,
        std_mem::offset_of!(NativeEntryCell, max_stack),
    )?;
    let callee_stack_values = load_cell_u32(
        builder,
        cell,
        std_mem::offset_of!(NativeEntryCell, max_stack_values),
    )?;
    let body_values = builder.ins().iadd(pushed_values, callee_stack_values);
    let body_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, body_values, max_values);
    let frame_capacity = load_activation_u32(builder, values, RawActivationField::FrameCapacity)?;
    let frame_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, frame_len, frame_capacity);
    let scalar_len = load_activation_u32(builder, values, RawActivationField::ScalarLen)?;
    let scalar_capacity = load_activation_u32(builder, values, RawActivationField::ScalarCapacity)?;
    let window = builder.ins().iadd(local_count, max_stack);
    let scalar_end = builder.ins().iadd(scalar_len, window);
    let scalars_fit =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, scalar_end, scalar_capacity);
    let compatible = match contract.local_count {
        Some(expected) => {
            let local_count_matches = builder.ins().icmp_imm(
                IntCC::Equal,
                local_count,
                i64::try_from(expected).map_err(|_| CompileError::Backend)?,
            );
            builder.ins().band(body_fits, local_count_matches)
        }
        None => body_fits,
    };
    builder.ins().brif(compatible, storage, &[], fallback, &[]);

    builder.switch_to_block(storage);
    let storage_fits = builder.ins().band(frame_fits, scalars_fit);
    builder.ins().brif(storage_fits, invoke, &[], grow, &[]);

    builder.switch_to_block(grow);
    let target_value = builder.ins().uextend(types::I64, target);
    let required_scalars = builder.ins().uextend(types::I64, scalar_end);
    let required_scalars = builder.ins().ishl_imm(required_scalars, 32);
    let growth = builder.ins().bor(required_scalars, target_value);
    let environment_tag = builder.ins().uextend(types::I64, environment);
    let kind = builder
        .ins()
        .iconst(types::I32, i64::from(EXIT_GROW_ACTIVATION));
    builder.ins().jump(
        preflight_exit,
        &[kind.into(), growth.into(), environment_tag.into()],
    );

    builder.switch_to_block(fallback);
    let (kind, result) = match fallback_kind {
        NativeCallFallback::Direct => {
            let target_value = builder.ins().uextend(types::I64, target);
            let environment_tag = builder.ins().uextend(types::I64, environment);
            (
                EXIT_CALL,
                NativeValue {
                    bits: target_value,
                    tag: environment_tag,
                },
            )
        }
        NativeCallFallback::Replay => {
            let zero = builder.ins().iconst(types::I64, 0);
            (
                EXIT_REPLAY,
                NativeValue {
                    bits: zero,
                    tag: zero,
                },
            )
        }
    };
    let kind = builder.ins().iconst(types::I32, i64::from(kind));
    builder.ins().jump(
        preflight_exit,
        &[kind.into(), result.bits.into(), result.tag.into()],
    );

    builder.switch_to_block(preflight_exit);
    let kind = builder.block_params(preflight_exit)[0];
    let result = NativeValue {
        bits: builder.block_params(preflight_exit)[1],
        tag: builder.block_params(preflight_exit)[2],
    };
    let retired = emit_retired(builder, values);
    let locals = capture_local_values(builder, values)?;
    emit_exit_with_locals_and_kind(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_FUEL,
            block,
            instruction,
            result,
        },
        kind,
        &locals,
        &boundary_stack,
    )?;

    builder.switch_to_block(invoke);
    emit_charge(builder, values, 1);
    let prior_changed = load_activation_u32(builder, values, RawActivationField::ChangedFrom)?;
    let caller_frame = active_frame;
    if contract.behavior.may_collect() {
        emit_spill_frame_roots(
            builder,
            values,
            caller_frame,
            local_kinds,
            caller_stack_kinds,
            &caller_stack,
        )?;
    }
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let scalar_base = scalar_len;
    let scalar_base_pointer = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_byte_offset = builder.ins().ishl_imm(scalar_base_pointer, 3);
    let child_locals = builder.ins().iadd(scalars, scalar_byte_offset);
    let child_tags = builder.ins().iadd(tags, scalar_byte_offset);
    let child_states = builder.ins().iadd(states, scalar_base_pointer);
    let zero_i8 = builder.ins().iconst(types::I8, 0);
    match contract.local_count {
        Some(local_count) => {
            for slot in 0..local_count {
                let offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
                builder
                    .ins()
                    .store(MemFlags::new(), zero_i8, child_states, offset);
            }
        }
        None => emit_clear_local_states(builder, child_states, local_count, zero_i8),
    }
    let initialized = builder
        .ins()
        .iconst(types::I8, i64::from(LOCAL_INITIALIZED));
    for (slot, argument) in arguments.iter().copied().enumerate() {
        let value_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), argument.bits, child_locals, value_offset);
        builder
            .ins()
            .store(MemFlags::new(), argument.tag, child_tags, value_offset);
        builder
            .ins()
            .store(MemFlags::new(), initialized, child_states, state_offset);
    }
    let frame_index = builder.ins().uextend(values.pointer_type, frame_len);
    let frame_offset = builder
        .ins()
        .imul_imm(frame_index, std_mem::size_of::<RawNativeFrame>() as i64);
    let child_frame = builder.ins().iadd(frames, frame_offset);
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, function),
        target,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, environment),
        environment,
    )?;
    let capture_tag = capture.map_or_else(
        || {
            builder
                .ins()
                .iconst(types::I64, ValueTag::Uninit as u64 as i64)
        },
        |capture| capture.tag,
    );
    let capture_bits = capture.map_or_else(
        || builder.ins().iconst(types::I64, 0),
        |capture| capture.bits,
    );
    store_i64(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, capture_tag),
        capture_tag,
    )?;
    store_i64(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, capture_bits),
        capture_bits,
    )?;
    store_i64(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, capture_data),
        capture_data,
    )?;
    store_i64(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, capture_len),
        capture_len,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, block),
        0,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, instruction),
        0,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, resume_entry),
        0,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, scalar_base),
        scalar_base,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, local_count),
        local_count,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, max_stack),
        max_stack,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, operand_len),
        0,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, native_created),
        1,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, caller_stack_values),
        caller_values,
    )?;
    store_i32_value(
        builder,
        child_frame,
        std_mem::offset_of!(RawNativeFrame, native_stack_bytes),
        next_native_stack_bytes,
    )?;
    let next_frame_len = builder.ins().iadd_imm(frame_len, 1);
    store_activation_u32(builder, values, RawActivationField::ScalarLen, scalar_end)?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::FrameLen,
        next_frame_len,
    )?;
    if contract.virtual_result {
        let request = builder.ins().iconst(types::I32, 1);
        store_i32_value(
            builder,
            values.activation_pointer,
            std_mem::offset_of!(RawNativeActivation, virtual_request),
            request,
        )?;
    }
    let caller_retired = emit_retired(builder, values);
    let zero_entry = builder.ins().iconst(types::I32, 0);
    builder.ins().call_indirect(
        values.native_signature,
        code,
        &[values.activation_pointer, caller_retired, zero_entry],
    );
    let child_retired = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, retired),
    )?;
    let poll_deadline = load_activation_u64(builder, values, RawActivationField::PollDeadline)?;
    let remaining_fuel = builder.ins().isub(poll_deadline, child_retired);
    builder.def_var(values.fuel, remaining_fuel);
    builder.def_var(values.retired, child_retired);
    let total_retired = child_retired;
    let exit_kind = load_value(
        builder,
        types::I32,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, kind),
    )?;
    let normal_return = builder
        .ins()
        .icmp_imm(IntCC::Equal, exit_kind, i64::from(EXIT_RETURN));
    builder
        .ins()
        .brif(normal_return, returned, &[], propagate, &[]);

    builder.switch_to_block(propagate);
    let frame_is_earlier = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, frame_len, prior_changed);
    let changed_from = builder
        .ins()
        .select(frame_is_earlier, frame_len, prior_changed);
    store_activation_u32(
        builder,
        values,
        RawActivationField::ChangedFrom,
        changed_from,
    )?;
    emit_spill_frame_to(
        builder,
        values,
        caller_frame,
        block,
        instruction + 1,
        &caller_stack,
    )?;
    store_i32_constant(
        builder,
        caller_frame,
        std_mem::offset_of!(RawNativeFrame, resume_entry),
        successor_entry,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, retired),
        total_retired,
    )?;
    builder.ins().return_(&[]);

    builder.switch_to_block(returned);
    let result = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result),
    )?;
    let result_tag = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        std_mem::offset_of!(RawExit, result_tag),
    )?;
    store_activation_u32(builder, values, RawActivationField::ScalarLen, scalar_base)?;
    store_activation_u32(builder, values, RawActivationField::FrameLen, frame_len)?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::ChangedFrom,
        prior_changed,
    )?;
    stack.truncate(caller_end);
    stack.push(NativeValue {
        bits: result,
        tag: result_tag,
    });
    define_stack(builder, values, stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

pub(super) fn emit_inline_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    input: &FunctionInput<'_>,
    stack: &mut Vec<NativeValue>,
    call: InlineCallEmission<'_, '_>,
) -> Result<(), CompileError> {
    let InlineCallEmission {
        definition,
        inline,
        contract,
        boundary_len,
        block,
        instruction,
        successor,
    } = call;
    let plan = inline.plan.as_ref();
    let argument_start = stack
        .len()
        .checked_sub(contract.params.len())
        .ok_or(CompileError::Backend)?;
    let arguments = stack[argument_start..].to_vec();
    emit_inline_call_preflight(
        builder,
        values,
        InlineCallPreflight {
            plan,
            max_path_cost: inline.max_path_cost,
            parameter_count: contract.params.len(),
            boundary_len,
            block,
            instruction,
            stack,
        },
    )?;
    emit_charge(builder, values, 1);

    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    let zero_pointer = builder.ins().iconst(values.pointer_type, 0);
    let mut locals = Vec::with_capacity(plan.local_kinds.len());
    let mut local_tags = Vec::with_capacity(plan.local_kinds.len());
    for kind in plan.local_kinds.iter().copied() {
        let local = builder.declare_var(types::I64);
        builder.def_var(local, zero_i64);
        locals.push(local);
        let tag = value_tag(kind).is_none().then(|| {
            let tag = builder.declare_var(types::I64);
            builder.def_var(tag, zero_i64);
            tag
        });
        local_tags.push(tag);
    }
    if arguments.len() > locals.len() {
        return Err(CompileError::Backend);
    }
    for (slot, argument) in arguments.iter().copied().enumerate() {
        builder.def_var(locals[slot], argument.bits);
        define_slot_tag(
            builder,
            local_tags[slot],
            plan.local_kinds[slot],
            argument.tag,
        )?;
    }

    let local_heap_caches = plan
        .local_kinds
        .iter()
        .copied()
        .map(|kind| {
            if !matches!(
                kind,
                ScalarKind::Object(_) | ScalarKind::Tagged(_) | ScalarKind::Callback(_)
            ) {
                return None;
            }
            let cache = LocalHeapCache {
                entry: builder.declare_var(values.pointer_type),
                object_kind: builder.declare_var(types::I64),
                class: builder.declare_var(types::I64),
                actual_class: builder.declare_var(types::I32),
                list_data: None,
                preloaded_list_data: false,
            };
            builder.def_var(cache.entry, zero_pointer);
            builder.def_var(cache.object_kind, zero_i64);
            builder.def_var(cache.class, zero_i64);
            builder.def_var(cache.actual_class, zero_i32);
            Some(cache)
        })
        .collect::<Vec<_>>();

    let mut child_stack = Vec::with_capacity(plan.max_stack);
    let mut child_stack_tags = Vec::with_capacity(plan.max_stack);
    for slot in 0..plan.max_stack {
        let variable = builder.declare_var(types::I64);
        builder.def_var(variable, zero_i64);
        child_stack.push(variable);
        let dynamic = plan.segments.iter().any(|segment| {
            segment.fuel_stacks.iter().any(|(_, kinds)| {
                kinds
                    .get(slot)
                    .copied()
                    .is_some_and(|kind| value_tag(kind).is_none())
            })
        });
        let tag = dynamic.then(|| {
            let tag = builder.declare_var(types::I64);
            builder.def_var(tag, zero_i64);
            tag
        });
        child_stack_tags.push(tag);
    }

    let return_block = builder.create_block();
    builder.append_block_param(return_block, types::I64);
    builder.append_block_param(return_block, types::I64);
    let entry_blocks = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect::<Vec<_>>();
    let body_blocks = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect::<Vec<_>>();
    let entry = plan
        .entries
        .get(&(0, 0))
        .copied()
        .ok_or(CompileError::Backend)?;
    builder.ins().jump(entry_blocks[entry], &[]);

    let scalar_instances = Vec::<ScalarInstanceValues>::new();
    let heap_translations = RefCell::new(HeapTranslationCache::default());
    let child_values = NativeValues {
        plan,
        locals: &locals,
        local_kinds: &plan.local_kinds,
        dirty_locals: None,
        local_tags: &local_tags,
        local_heap_caches: &local_heap_caches,
        scalar_instances: &scalar_instances,
        stack: &child_stack,
        stack_tags: &child_stack_tags,
        replay_blocks: values.replay_blocks,
        replay_failures: true,
        inline_return: Some(return_block),
        heap_translations: &heap_translations,
        ..values
    };
    let child_input = input
        .child(definition.function)
        .ok_or(CompileError::Backend)?;
    for (index, segment) in plan.segments.iter().enumerate() {
        builder.switch_to_block(entry_blocks[index]);
        let fuel = builder.use_var(values.fuel);
        let enough = builder.ins().icmp_imm(
            IntCC::SignedGreaterThanOrEqual,
            fuel,
            i64::from(segment.fuel_reserve),
        );
        let replay = builder.ins().bxor_imm(enough, 1);
        emit_interpreter_replay(
            builder,
            child_values,
            replay,
            FaultPoint {
                block: segment.block,
                instruction: segment.start,
                prefix: 0,
            },
            &[],
        )?;
        builder.ins().jump(body_blocks[index], &[]);

        builder.switch_to_block(body_blocks[index]);
        let successor_blocks = segment
            .successors
            .iter()
            .map(|successor| {
                if bypasses_fuel_check(&plan.segments, index, *successor) {
                    body_blocks[*successor]
                } else {
                    entry_blocks[*successor]
                }
            })
            .collect::<Vec<_>>();
        let segment_values = NativeValues {
            dirty_locals: Some(&segment.dirty_locals),
            ..child_values
        };
        let entry_stack = segment_entry_values(builder, segment_values, segment)?;
        emit_segment_body(
            builder,
            SegmentEmission {
                bytecode: definition.runtime,
                segment,
                successor_blocks: &successor_blocks,
                values: segment_values,
                plan,
                input: &child_input,
                type_environment_sites: &[],
            },
            entry_stack,
            segment.virtual_stack_in.clone(),
        )?;
    }

    builder.switch_to_block(return_block);
    stack.truncate(argument_start);
    stack.push(NativeValue {
        bits: builder.block_params(return_block)[0],
        tag: builder.block_params(return_block)[1],
    });
    define_stack(builder, values, stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

struct InlineCallPreflight<'a> {
    plan: &'a RegionPlan,
    max_path_cost: u32,
    parameter_count: usize,
    boundary_len: usize,
    block: u32,
    instruction: u32,
    stack: &'a [NativeValue],
}

fn emit_inline_call_preflight(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    preflight: InlineCallPreflight<'_>,
) -> Result<(), CompileError> {
    let InlineCallPreflight {
        plan,
        max_path_cost,
        parameter_count,
        boundary_len,
        block,
        instruction,
        stack,
    } = preflight;
    let required_fuel = max_path_cost.checked_add(1).ok_or(CompileError::Backend)?;
    let inline = builder.create_block();
    let boundary = builder.create_block();
    builder.set_cold_block(boundary);
    let poll_remaining = builder.use_var(values.fuel);
    let can_start = builder.ins().icmp_imm(
        IntCC::SignedGreaterThanOrEqual,
        poll_remaining,
        i64::from(required_fuel),
    );
    builder.ins().brif(can_start, inline, &[], boundary, &[]);

    builder.switch_to_block(boundary);
    let retired = emit_retired(builder, values);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_INLINE_CALL,
            block,
            instruction,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        stack,
    )?;

    builder.switch_to_block(inline);

    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let base_frames = load_activation_u32(builder, values, RawActivationField::BaseFrames)?;
    let max_frames = load_activation_u32(builder, values, RawActivationField::MaxFrames)?;
    let total_frames = builder.ins().iadd(base_frames, frame_len);
    let frames_fit = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, total_frames, max_frames);

    let frames = load_activation_pointer(builder, values, RawActivationField::Frames)?;
    let active_frame_index = builder.ins().iadd_imm(frame_len, -1);
    let active_frame_index = builder
        .ins()
        .uextend(values.pointer_type, active_frame_index);
    let active_frame_offset = builder.ins().imul_imm(
        active_frame_index,
        std_mem::size_of::<RawNativeFrame>() as i64,
    );
    let active_frame = builder.ins().iadd(frames, active_frame_offset);
    let caller_prefix = load_cell_u32(
        builder,
        active_frame,
        std_mem::offset_of!(RawNativeFrame, caller_stack_values),
    )?;
    let active_local_count = load_cell_u32(
        builder,
        active_frame,
        std_mem::offset_of!(RawNativeFrame, local_count),
    )?;
    let active_values = builder.ins().iadd(caller_prefix, active_local_count);
    let active_values = builder.ins().iadd_imm(
        active_values,
        i64::try_from(boundary_len).map_err(|_| CompileError::Backend)?,
    );
    let caller_values = builder.ins().iadd_imm(
        active_values,
        -i64::try_from(parameter_count).map_err(|_| CompileError::Backend)?,
    );
    let pushed_values = builder.ins().iadd_imm(
        caller_values,
        i64::try_from(plan.local_kinds.len()).map_err(|_| CompileError::Backend)?,
    );
    let max_values = load_activation_u32(builder, values, RawActivationField::MaxStackValues)?;
    let locals_fit = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, pushed_values, max_values);

    let stack_fits = builder.ins().band(frames_fit, locals_fit);
    let stack_fault = builder.ins().bxor_imm(stack_fits, 1);
    let fault = builder.create_block();
    let body_check = builder.create_block();
    builder.set_cold_block(fault);
    builder.ins().brif(stack_fault, fault, &[], body_check, &[]);

    builder.switch_to_block(fault);
    emit_charge(builder, values, 1);
    let retired = emit_retired(builder, values);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_STACK_LIMIT,
            block,
            instruction: instruction + 1,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        stack,
    )?;

    builder.switch_to_block(body_check);
    let body_values = builder.ins().iadd_imm(
        pushed_values,
        i64::try_from(plan.max_stack_values).map_err(|_| CompileError::Backend)?,
    );
    let body_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, body_values, max_values);
    let replay = builder.ins().bxor_imm(body_fits, 1);

    emit_interpreter_replay(
        builder,
        values,
        replay,
        FaultPoint {
            block,
            instruction,
            prefix: 0,
        },
        stack,
    )
}

pub(super) fn emit_clear_local_states(
    builder: &mut FunctionBuilder<'_>,
    states: ir::Value,
    count: ir::Value,
    zero: ir::Value,
) {
    let test = builder.create_block();
    let clear = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(test, types::I32);
    builder.append_block_param(clear, types::I32);
    let first = builder.ins().iconst(types::I32, 0);
    builder.ins().jump(test, &[first.into()]);

    builder.switch_to_block(test);
    let index = builder.block_params(test)[0];
    let complete = builder.ins().icmp(IntCC::Equal, index, count);
    builder
        .ins()
        .brif(complete, done, &[], clear, &[index.into()]);

    builder.switch_to_block(clear);
    let index = builder.block_params(clear)[0];
    let pointer_type = builder.func.dfg.value_type(states);
    let offset = builder.ins().uextend(pointer_type, index);
    let address = builder.ins().iadd(states, offset);
    builder.ins().store(MemFlags::new(), zero, address, 0);
    let next = builder.ins().iadd_imm(index, 1);
    builder.ins().jump(test, &[next.into()]);

    builder.switch_to_block(done);
}

pub(super) fn emit_virtual_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    contract: VirtualReceiver,
    selector: u32,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let class = match contract {
        VirtualReceiver::Immediate { class } => builder.ins().iconst(types::I32, i64::from(class)),
        VirtualReceiver::Object { tag, class } => {
            emit_object_entry(
                builder,
                values,
                receiver.bits,
                tag,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
            )?;
            builder.ins().iconst(types::I32, i64::from(class))
        }
        VirtualReceiver::Instance { class } => {
            let (_, actual) = emit_instance_entry(
                builder,
                values,
                receiver.bits,
                class,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
                ObjectGuard::Replay(deopt_stack),
            )?;
            actual
        }
        VirtualReceiver::Text { string, substring } => {
            let entry = emit_text_entry(
                builder,
                values,
                receiver.bits,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
            )?;
            let tag = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
            let is_string = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, i64::from(JIT_OBJECT_STR));
            let string = builder.ins().iconst(types::I32, i64::from(string));
            let substring = builder.ins().iconst(types::I32, i64::from(substring));
            builder.ins().select(is_string, string, substring)
        }
    };

    let class_index = builder.ins().uextend(values.pointer_type, class);
    let row_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, dispatch_row_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, class_index, row_count);
    emit_interpreter_replay(builder, values, outside, guard_point, deopt_stack)?;
    let rows = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, dispatch_rows),
    )?;
    let row_offset = builder
        .ins()
        .imul_imm(class_index, std_mem::size_of::<NativeDispatchRow>() as i64);
    let row = builder.ins().iadd(rows, row_offset);
    let base = load_value(
        builder,
        types::I32,
        row,
        std_mem::offset_of!(NativeDispatchRow, base),
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        row,
        std_mem::offset_of!(NativeDispatchRow, len),
    )?;
    let start = load_value(
        builder,
        values.pointer_type,
        row,
        std_mem::offset_of!(NativeDispatchRow, start),
    )?;
    let selector = builder.ins().iconst(types::I32, i64::from(selector));
    let below = builder.ins().icmp(IntCC::UnsignedLessThan, selector, base);
    emit_interpreter_replay(builder, values, below, guard_point, deopt_stack)?;
    let offset = builder.ins().isub(selector, base);
    let offset = builder.ins().uextend(values.pointer_type, offset);
    let past = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, offset, len);
    emit_interpreter_replay(builder, values, past, guard_point, deopt_stack)?;
    let method_index = builder.ins().iadd(start, offset);
    let method_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, dispatch_method_count),
    )?;
    let method_outside = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        method_index,
        method_count,
    );
    emit_interpreter_replay(builder, values, method_outside, guard_point, deopt_stack)?;
    let methods = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, dispatch_methods),
    )?;
    let method_offset = builder
        .ins()
        .imul_imm(method_index, std_mem::size_of::<u32>() as i64);
    let method_address = builder.ins().iadd(methods, method_offset);
    let target = builder
        .ins()
        .load(types::I32, MemFlags::trusted(), method_address, 0);
    let missing = builder
        .ins()
        .icmp_imm(IntCC::Equal, target, u32::MAX as i64);
    emit_interpreter_replay(builder, values, missing, guard_point, deopt_stack)?;
    Ok(target)
}
