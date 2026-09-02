//! Heap access and value-guard emission.

use super::*;

pub(super) fn emit_load_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    emission: LoadFieldEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let LoadFieldEmission {
        field,
        receiver_class,
        contract,
        allow_pending,
        exit,
    } = emission;
    let scalar_sites = values
        .plan
        .scalar_instances
        .iter()
        .enumerate()
        .filter_map(|(site, instance)| {
            (instance.class == receiver_class && field < instance.field_count).then_some(site)
        })
        .collect::<Vec<_>>();
    if !scalar_sites.is_empty() {
        let fallback = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I64);
        let mut test = None;
        for site in scalar_sites {
            if let Some(test) = test {
                builder.switch_to_block(test);
            }
            let scalar = values
                .scalar_instances
                .get(site)
                .ok_or(CompileError::Backend)?;
            let matched = builder
                .ins()
                .icmp_imm(IntCC::Equal, reference, scalar.token as i64);
            let hit = builder.create_block();
            let miss = builder.create_block();
            builder.ins().brif(matched, hit, &[], miss, &[]);

            builder.switch_to_block(hit);
            let value = scalar
                .fields
                .get(field as usize)
                .ok_or(CompileError::Backend)?;
            let bits = builder.use_var(value.bits);
            let tag = builder.use_var(value.tag);
            builder.ins().jump(done, &[bits.into(), tag.into()]);
            test = Some(miss);
        }
        builder.switch_to_block(test.ok_or(CompileError::Backend)?);
        builder.ins().jump(fallback, &[]);

        builder.switch_to_block(fallback);
        let value = emit_regular_load_field(
            builder,
            values,
            reference,
            field,
            receiver_class,
            contract,
            allow_pending,
            exit,
        )?;
        builder
            .ins()
            .jump(done, &[value.bits.into(), value.tag.into()]);

        builder.switch_to_block(done);
        return Ok(NativeValue {
            bits: builder.block_params(done)[0],
            tag: builder.block_params(done)[1],
        });
    }
    emit_regular_load_field(
        builder,
        values,
        reference,
        field,
        receiver_class,
        contract,
        allow_pending,
        exit,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_regular_load_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    field: u32,
    receiver_class: u32,
    contract: ValueContract,
    allow_pending: bool,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let value = if allow_pending {
        let storage = emit_instance_storage(
            builder,
            values,
            reference,
            Some(receiver_class),
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_instance_storage_field(
            builder,
            values,
            storage,
            field,
            exit.point,
            exit.fault_stack,
        )?
    } else {
        let (entry, _) = emit_instance_entry(
            builder,
            values,
            reference,
            receiver_class,
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
        emit_array_element(
            builder,
            values,
            entry,
            JIT_INSTANCE_FIELDS_OFFSET,
            field_index,
            exit.point,
            exit.fault_stack,
        )?
    };
    let tag = load_value(builder, types::I64, value, VALUE_TAG_OFFSET)?;
    let uninitialized = builder
        .ins()
        .icmp_imm(IntCC::Equal, tag, ValueTag::Uninit as u64 as i64);
    emit_fault_check(
        builder,
        values,
        uninitialized,
        EXIT_UNINITIALIZED_FIELD,
        exit.point,
        exit.fault_stack,
    )?;
    emit_loaded_value(
        builder,
        values,
        value,
        contract,
        exit.point,
        exit.deopt_stack,
    )
}

pub(super) fn emit_load_capture(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    index: u32,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    let capture_data = load_value(
        builder,
        values.pointer_type,
        frame,
        std_mem::offset_of!(RawNativeFrame, capture_data),
    )?;
    let capture_len = load_value(
        builder,
        values.pointer_type,
        frame,
        std_mem::offset_of!(RawNativeFrame, capture_len),
    )?;
    let index = builder.ins().iconst(values.pointer_type, i64::from(index));
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, capture_len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let byte_offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let value = builder.ins().iadd(capture_data, byte_offset);
    emit_loaded_value(builder, values, value, result, exit.point, exit.deopt_stack)
}

pub(super) fn emit_store_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    allow_pending: bool,
    emission: StoreFieldEmission<'_>,
) -> Result<(), CompileError> {
    let StoreFieldEmission {
        field,
        receiver_class,
        contract,
        exit,
    } = emission;
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let address = if allow_pending {
        let storage = emit_instance_storage(
            builder,
            values,
            reference,
            Some(receiver_class),
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_mutable_flag_guard(builder, values, storage.frozen, exit)?;
        emit_instance_storage_field(
            builder,
            values,
            storage,
            field,
            exit.point,
            exit.fault_stack,
        )?
    } else {
        let (entry, _) = emit_instance_entry(
            builder,
            values,
            reference,
            receiver_class,
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_mutable_guard(builder, values, entry, exit)?;
        let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
        emit_array_element(
            builder,
            values,
            entry,
            JIT_INSTANCE_FIELDS_OFFSET,
            field_index,
            exit.point,
            exit.fault_stack,
        )?
    };
    emit_store_value(builder, address, stored, contract.kind)
}

pub(super) fn emit_tuple_get(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: u32,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_TUPLE,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let index = builder.ins().iconst(values.pointer_type, i64::from(index));
    let address = emit_array_element(
        builder,
        values,
        entry,
        JIT_TUPLE_ITEMS_OFFSET,
        index,
        exit.point,
        exit.fault_stack,
    )?;
    emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )
}

pub(super) fn emit_active_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    offset: usize,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let active = load_heap_value(builder, types::I8, entry, offset)?;
    let inactive = builder.ins().icmp_imm(IntCC::Equal, active, 0);
    emit_interpreter_replay(builder, values, inactive, point, deopt_stack)
}

pub(super) fn emit_mutable_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let frozen = builder.ins().iadd_imm(
        entry,
        i64::try_from(JIT_ENTRY_FROZEN_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_mutable_flag_guard(builder, values, frozen, exit)
}

pub(super) fn emit_mutable_flag_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frozen: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let frozen = load_heap_value(builder, types::I8, frozen, 0)?;
    let frozen = builder.ins().icmp_imm(IntCC::NotEqual, frozen, 0);
    emit_interpreter_replay(builder, values, frozen, exit.point, exit.deopt_stack)
}

pub(super) fn emit_checked_list_index(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    index: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    Ok(index)
}

pub(super) fn emit_array_element(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    array_offset: usize,
    index: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let len = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        array_offset + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        point,
        fault_stack,
    )?;
    emit_array_address(builder, values, entry, array_offset, index)
}

pub(super) fn emit_array_address(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    array_offset: usize,
    index: ir::Value,
) -> Result<ir::Value, CompileError> {
    let cached = array_offset == JIT_LIST_ITEMS_OFFSET
        && values.heap_translations.borrow().use_cached_list_data;
    let data = if cached {
        local_heap_cache(values, entry)
            .and_then(|cache| cache.list_data)
            .map(|data| builder.use_var(data))
    } else {
        None
    };
    let data = if let Some(data) = data {
        data
    } else if matches!(
        array_offset,
        JIT_INSTANCE_FIELDS_OFFSET | JIT_TUPLE_ITEMS_OFFSET
    ) {
        load_immutable_heap_value(
            builder,
            values.pointer_type,
            entry,
            array_offset + VALUE_ARRAY_DATA_OFFSET,
        )?
    } else {
        load_heap_value(
            builder,
            values.pointer_type,
            entry,
            array_offset + VALUE_ARRAY_DATA_OFFSET,
        )?
    };
    let byte_offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().iadd(data, byte_offset))
}

pub(super) fn emit_option_family(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    family_type: u32,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let family = emit_type_cache_lookup(
        builder,
        values,
        function,
        point,
        TypeCacheRequest::OptionFamily { ty: family_type },
        stack,
    )?;
    Ok(builder.ins().uextend(types::I64, family))
}

pub(super) fn emit_literal_load(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    literal: usize,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<NativeValue, CompileError> {
    let load = builder.create_block();
    let missing = (!values.replay_failures).then(|| builder.create_block());
    let ready = builder.create_block();
    let index = builder.ins().iconst(
        values.pointer_type,
        i64::try_from(literal).map_err(|_| CompileError::Backend)?,
    );
    let count = load_activation_pointer(builder, values, RawActivationField::LiteralCount)?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, count);
    if let Some(missing) = missing {
        builder.ins().brif(outside, missing, &[], load, &[]);
    } else {
        emit_interpreter_replay(builder, values, outside, point, stack)?;
        builder.ins().jump(load, &[]);
    }

    builder.switch_to_block(load);
    let literals = load_activation_pointer(builder, values, RawActivationField::LiteralValues)?;
    let offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let address = builder.ins().iadd(literals, offset);
    let tag = load_value(builder, types::I64, address, VALUE_TAG_OFFSET)?;
    let invalid = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, ValueTag::Obj as u64 as i64);
    if let Some(missing) = missing {
        builder.ins().brif(invalid, missing, &[], ready, &[]);

        builder.switch_to_block(missing);
        let retired = emit_retired_with_prefix(builder, values, point.prefix);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_LITERAL,
                block: point.block,
                instruction: point.instruction,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            stack,
        )?;
    } else {
        emit_interpreter_replay(builder, values, invalid, point, stack)?;
        builder.ins().jump(ready, &[]);
    }

    builder.switch_to_block(ready);
    let bits = load_value(builder, types::I64, address, VALUE_PAYLOAD_OFFSET)?;
    Ok(NativeValue { bits, tag })
}

pub(super) fn emit_exact_option_none(
    builder: &mut FunctionBuilder<'_>,
    value: NativeValue,
    family: ir::Value,
) -> ir::Value {
    let empty = builder
        .ins()
        .icmp_imm(IntCC::Equal, value.tag, ValueTag::EmptyCase as u64 as i64);
    let stored_family = builder.ins().ireduce(types::I32, value.bits);
    let family = builder.ins().ireduce(types::I32, family);
    let same_family = builder.ins().icmp(IntCC::Equal, stored_family, family);
    let arm = builder.ins().ushr_imm(value.bits, 32);
    let none_arm = builder.ins().icmp_imm(IntCC::Equal, arm, 1);
    let exact_none = builder.ins().band(empty, same_family);
    builder.ins().band(exact_none, none_arm)
}

pub(super) fn emit_native_value_contract(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: NativeValue,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    emit_scalar_tag_guard(
        builder,
        values,
        value.tag,
        contract.kind,
        point,
        deopt_stack,
    )?;
    if matches!(contract.kind, ScalarKind::Float) {
        emit_canonical_float_guard(builder, values, value.bits, point, deopt_stack)?;
    }
    emit_value_contract(builder, values, value.bits, contract, point, deopt_stack)
}

pub(super) fn emit_loaded_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    address: ir::Value,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<NativeValue, CompileError> {
    let tag = load_value(builder, types::I64, address, VALUE_TAG_OFFSET)?;
    emit_scalar_tag_guard(builder, values, tag, contract.kind, point, deopt_stack)?;
    let payload = emit_value_payload(
        builder,
        values,
        address,
        tag,
        contract.kind,
        point,
        deopt_stack,
    )?;
    emit_value_contract(builder, values, payload, contract, point, deopt_stack)?;
    Ok(NativeValue { bits: payload, tag })
}

pub(super) fn emit_scalar_tag_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    tag: ir::Value,
    kind: ScalarKind,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let invalid = if matches!(kind, ScalarKind::Callback(_)) {
        let closure = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, ValueTag::Obj as u64 as i64);
        let callback = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, ValueTag::Callback as u64 as i64);
        let valid = builder.ins().bor(closure, callback);
        builder.ins().bxor_imm(valid, 1)
    } else if let Some(expected_tag) = value_tag(kind) {
        builder
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, expected_tag as u64 as i64)
    } else {
        return Ok(());
    };
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)
}

pub(super) fn emit_value_contract(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    payload: ir::Value,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let Some(object) = contract.object else {
        return Ok(());
    };
    if matches!(object, ObjectContract::Text) {
        emit_text_entry(
            builder,
            values,
            payload,
            point,
            ObjectGuard::Replay(deopt_stack),
        )?;
        return Ok(());
    }
    if let ObjectContract::Instance(class) = object {
        emit_instance_entry(
            builder,
            values,
            payload,
            class,
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?;
        return Ok(());
    }
    let tag = match object {
        ObjectContract::Str => JIT_OBJECT_STR,
        ObjectContract::Text => unreachable!(),
        ObjectContract::Instance(_) => unreachable!(),
        ObjectContract::List => JIT_OBJECT_LIST,
        ObjectContract::Map => JIT_OBJECT_MAP,
        ObjectContract::Tuple => JIT_OBJECT_TUPLE,
        ObjectContract::Closure => JIT_OBJECT_CLOSURE,
        ObjectContract::Bytes => JIT_OBJECT_BYTES,
        ObjectContract::Digest => JIT_OBJECT_DIGEST,
        ObjectContract::StringBuilder => JIT_OBJECT_STRING_BUILDER,
        ObjectContract::ByteBuffer => JIT_OBJECT_BYTE_BUFFER,
    };
    emit_object_entry(
        builder,
        values,
        payload,
        tag,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    Ok(())
}

pub(super) fn emit_class_matches(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    actual: ir::Value,
    target: u32,
) -> Result<ir::Value, CompileError> {
    let test = builder.create_block();
    let parent = builder.create_block();
    let matched = builder.create_block();
    let missed = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(test, types::I32);
    builder.append_block_param(done, types::I8);
    builder.ins().jump(test, &[actual.into()]);

    builder.switch_to_block(test);
    let current = builder.block_params(test)[0];
    let equal = builder
        .ins()
        .icmp_imm(IntCC::Equal, current, i64::from(target));
    builder.ins().brif(equal, matched, &[], parent, &[]);

    builder.switch_to_block(parent);
    let current_index = builder.ins().uextend(values.pointer_type, current);
    let class_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, class_count),
    )?;
    let outside = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        current_index,
        class_count,
    );
    let load_parent = builder.create_block();
    builder.ins().brif(outside, missed, &[], load_parent, &[]);

    builder.switch_to_block(load_parent);
    let parents = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, class_parents),
    )?;
    let offset = builder
        .ins()
        .imul_imm(current_index, std_mem::size_of::<u32>() as i64);
    let address = builder.ins().iadd(parents, offset);
    let next = builder
        .ins()
        .load(types::I32, MemFlags::trusted(), address, 0);
    let at_root = builder
        .ins()
        .icmp_imm(IntCC::Equal, next, i64::from(lm_bytecode::NO_PARENT));
    builder
        .ins()
        .brif(at_root, missed, &[], test, &[next.into()]);

    builder.switch_to_block(matched);
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(done, &[one.into()]);

    builder.switch_to_block(missed);
    let zero = builder.ins().iconst(types::I8, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_store_value(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
    value: NativeValue,
    kind: ScalarKind,
) -> Result<(), CompileError> {
    let tag = match value_tag(kind) {
        Some(tag) => builder.ins().iconst(types::I64, tag as u64 as i64),
        None => value.tag,
    };
    store_heap_value(builder, address, VALUE_TAG_OFFSET, tag)?;
    store_heap_value(builder, address, VALUE_PAYLOAD_OFFSET, value.bits)
}

pub(super) fn emit_object_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    object_tag: u32,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let local_slot = values.heap_translations.borrow().local(reference);
    let local_cache = local_slot
        .and_then(|slot| values.local_heap_caches.get(slot))
        .copied()
        .flatten();
    let preloaded = object_tag == JIT_OBJECT_LIST
        && values.heap_translations.borrow().use_cached_list_data
        && local_cache.is_some_and(|cache| cache.preloaded_list_data);
    if preloaded {
        let cache = local_cache.ok_or(CompileError::Backend)?;
        let entry = builder.use_var(cache.entry);
        if let Some(slot) = local_slot {
            values
                .heap_translations
                .borrow_mut()
                .record_local(entry, slot);
        }
        return Ok(entry);
    }
    let expected = i64::from(object_tag) + 1;
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached_kind = builder.use_var(cache.object_kind);
        let proven = builder.ins().icmp_imm(IntCC::Equal, cached_kind, expected);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(miss);
        let entry = emit_heap_entry(builder, values, reference, point, guard)?;
        let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
        let wrong_kind = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(object_tag));
        emit_object_guard(builder, values, wrong_kind, point, guard)?;
        if object_tag == JIT_OBJECT_LIST {
            if let Some(list_data) = cache.list_data {
                let data = load_immutable_heap_value(
                    builder,
                    values.pointer_type,
                    entry,
                    JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
                )?;
                builder.def_var(list_data, data);
            }
        }
        let expected = builder.ins().iconst(types::I64, expected);
        builder.def_var(cache.object_kind, expected);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    } else {
        let entry = emit_heap_entry(builder, values, reference, point, guard)?;
        let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
        let wrong_kind = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(object_tag));
        emit_object_guard(builder, values, wrong_kind, point, guard)?;
        entry
    };
    if let Some(slot) = local_slot {
        values
            .heap_translations
            .borrow_mut()
            .record_local(entry, slot);
    }
    Ok(entry)
}

pub(super) fn emit_instance_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let local_cache = local_heap_cache(values, reference);
    let expected = i64::from(class) + 1;
    let (entry, actual) = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        builder.append_block_param(done, types::I32);
        let cached_class = builder.use_var(cache.class);
        let proven = builder.ins().icmp_imm(IntCC::Equal, cached_class, expected);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        let actual = builder.use_var(cache.actual_class);
        builder.ins().jump(done, &[entry.into(), actual.into()]);

        builder.switch_to_block(miss);
        let (entry, actual) = emit_instance_entry_miss(
            builder,
            values,
            reference,
            class,
            point,
            object_guard,
            class_guard,
        )?;
        let expected = builder.ins().iconst(types::I64, expected);
        builder.def_var(cache.class, expected);
        builder.def_var(cache.actual_class, actual);
        builder.ins().jump(done, &[entry.into(), actual.into()]);

        builder.switch_to_block(done);
        (builder.block_params(done)[0], builder.block_params(done)[1])
    } else {
        emit_instance_entry_miss(
            builder,
            values,
            reference,
            class,
            point,
            object_guard,
            class_guard,
        )?
    };
    Ok((entry, actual))
}

#[derive(Clone, Copy)]
pub(super) struct NativeInstanceStorage {
    pub(super) frozen: ir::Value,
    pub(super) actual_class: ir::Value,
    pub(super) data: ir::Value,
    pub(super) len: ir::Value,
}

#[derive(Clone, Copy)]
pub(super) struct PendingRecordLookup {
    record: ir::Value,
    record_index: ir::Value,
}

pub(super) fn emit_pending_record_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    miss: ir::Block,
) -> Result<PendingRecordLookup, CompileError> {
    let check_record = builder.create_block();
    let use_record = builder.create_block();
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    let marker = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        slot,
        i64::from(PENDING_INSTANCE_SLOT_BASE),
    );
    let pending = builder.ins().band(outside, marker);
    builder.ins().brif(pending, check_record, &[], miss, &[]);

    builder.switch_to_block(check_record);
    let maximum = builder.ins().iconst(types::I32, i64::from(u32::MAX));
    let record_index = builder.ins().isub(maximum, slot);
    let record_outside = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_COUNT).map_err(|_| CompileError::Backend)?,
    );
    let record_index_pointer = builder.ins().uextend(values.pointer_type, record_index);
    let records = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let record_offset = builder.ins().imul_imm(
        record_index_pointer,
        i64::try_from(std_mem::size_of::<RawVirtualInstance>())
            .map_err(|_| CompileError::Backend)?,
    );
    let record = builder.ins().iadd(records, record_offset);
    builder
        .ins()
        .brif(record_outside, miss, &[], use_record, &[]);

    builder.switch_to_block(use_record);
    let active = load_value(
        builder,
        types::I32,
        record,
        std_mem::offset_of!(RawVirtualInstance, active),
    )?;
    let record_bits = load_value(
        builder,
        types::I64,
        record,
        std_mem::offset_of!(RawVirtualInstance, object_bits),
    )?;
    let inactive = builder.ins().icmp_imm(IntCC::Equal, active, 0);
    let wrong_record = builder.ins().icmp(IntCC::NotEqual, record_bits, reference);
    let invalid_record = builder.ins().bor(inactive, wrong_record);
    let valid_record = builder.create_block();
    builder
        .ins()
        .brif(invalid_record, miss, &[], valid_record, &[]);
    builder.switch_to_block(valid_record);
    Ok(PendingRecordLookup {
        record,
        record_index,
    })
}

pub(super) fn emit_retain_pending_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
) -> Result<(), CompileError> {
    let done = builder.create_block();
    let lookup = emit_pending_record_lookup(builder, values, reference, done)?;
    let references = load_value(
        builder,
        types::I32,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, references),
    )?;
    let next = builder.ins().iadd_imm(references, 1);
    store_i32_value(
        builder,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, references),
        next,
    )?;
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_release_pending_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
) -> Result<(), CompileError> {
    let done = builder.create_block();
    let lookup = emit_pending_record_lookup(builder, values, reference, done)?;
    let references = load_value(
        builder,
        types::I32,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, references),
    )?;
    let next = builder.ins().iadd_imm(references, -1);
    store_i32_value(
        builder,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, references),
        next,
    )?;
    let retained = builder.ins().icmp_imm(IntCC::NotEqual, next, 0);
    let release = builder.create_block();
    builder.ins().brif(retained, done, &[], release, &[]);

    builder.switch_to_block(release);
    let field_count = load_value(
        builder,
        types::I32,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, field_count),
    )?;
    let field_count = builder.ins().uextend(values.pointer_type, field_count);
    let bytes = builder.ins().imul_imm(
        field_count,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let bytes = builder.ins().iadd_imm(
        bytes,
        i64::try_from(MIN_OBJECT_COST).map_err(|_| CompileError::Backend)?,
    );
    let dead = builder.ins().iconst(types::I32, 0);
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().isub(used, bytes);
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, -1);
    store_heap_value(builder, live_pointer, 0, next_live)?;

    store_i32_value(
        builder,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, active),
        dead,
    )?;
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let one = builder.ins().iconst(types::I64, 1);
    let record_index = builder.ins().uextend(types::I64, lookup.record_index);
    let bit = builder.ins().ishl(one, record_index);
    let available = builder.ins().bor(available, bit);
    let available_offset =
        i32::try_from(std_mem::offset_of!(RawNativeActivation, virtual_available))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        available,
        values.activation_pointer,
        available_offset,
    );
    let releases = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, pending_instance_releases),
    )?;
    let releases = builder.ins().iadd_imm(releases, 1);
    let releases_offset = i32::try_from(std_mem::offset_of!(
        RawNativeActivation,
        pending_instance_releases
    ))
    .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        releases,
        values.activation_pointer,
        releases_offset,
    );
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_instance_storage(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: Option<u32>,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<NativeInstanceStorage, CompileError> {
    let canonical = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);

    let lookup = emit_pending_record_lookup(builder, values, reference, canonical)?;
    let actual = load_value(
        builder,
        types::I32,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, class),
    )?;
    let len = load_value(
        builder,
        types::I32,
        lookup.record,
        std_mem::offset_of!(RawVirtualInstance, field_count),
    )?;
    let len = builder.ins().uextend(values.pointer_type, len);
    let fields = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    let record_index = builder
        .ins()
        .uextend(values.pointer_type, lookup.record_index);
    let data_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_FIELDS.saturating_mul(VALUE_SIZE))
            .map_err(|_| CompileError::Backend)?,
    );
    let data = builder.ins().iadd(fields, data_offset);
    let frozen = builder.ins().iadd_imm(
        lookup.record,
        i64::try_from(std_mem::offset_of!(RawVirtualInstance, frozen))
            .map_err(|_| CompileError::Backend)?,
    );
    builder.ins().jump(
        done,
        &[actual.into(), data.into(), len.into(), frozen.into()],
    );

    builder.switch_to_block(canonical);
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        point,
        object_guard,
    )?;
    let actual = load_heap_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let data = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
    )?;
    let len = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let frozen = builder.ins().iadd_imm(
        entry,
        i64::try_from(JIT_ENTRY_FROZEN_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    builder.ins().jump(
        done,
        &[actual.into(), data.into(), len.into(), frozen.into()],
    );

    builder.switch_to_block(done);
    let actual_class = builder.block_params(done)[0];
    if let Some(class) = class {
        let matches = emit_class_matches(builder, values, actual_class, class)?;
        let mismatch = builder.ins().bxor_imm(matches, 1);
        emit_object_guard(builder, values, mismatch, point, class_guard)?;
    }
    Ok(NativeInstanceStorage {
        frozen: builder.block_params(done)[3],
        actual_class,
        data: builder.block_params(done)[1],
        len: builder.block_params(done)[2],
    })
}

pub(super) fn emit_instance_storage_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    storage: NativeInstanceStorage,
    field: u32,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let index = builder.ins().iconst(values.pointer_type, i64::from(field));
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, storage.len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        point,
        fault_stack,
    )?;
    let offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().iadd(storage.data, offset))
}

pub(super) fn emit_instance_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        point,
        object_guard,
    )?;
    let actual = load_heap_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let matches = emit_class_matches(builder, values, actual, class)?;
    let mismatch = builder.ins().bxor_imm(matches, 1);
    emit_object_guard(builder, values, mismatch, point, class_guard)?;
    Ok((entry, actual))
}

#[derive(Clone, Copy)]
pub(super) struct NativeTextEntry {
    pub(super) payload: ir::Value,
    pub(super) is_string: ir::Value,
}

pub(super) fn emit_text_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<NativeTextEntry, CompileError> {
    let generation = builder.ins().ushr_imm(reference, 32);
    let generation = builder.ins().ireduce(types::I32, generation);
    let storage_tag = builder
        .ins()
        .band_imm(generation, i64::from(TEXT_VIEW_GENERATION_TAG));
    let compact = builder.ins().icmp_imm(IntCC::NotEqual, storage_tag, 0);
    let normal_block = builder.create_block();
    let compact_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, types::I8);
    builder
        .ins()
        .brif(compact, compact_block, &[], normal_block, &[]);

    builder.switch_to_block(normal_block);
    let entry = emit_normal_text_entry(builder, values, reference, point, guard)?;
    let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let is_string = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(JIT_OBJECT_STR));
    let payload_offset = JIT_TEXT_DATA_OFFSET
        .checked_sub(JIT_TEXT_PAYLOAD_DATA_OFFSET)
        .ok_or(CompileError::Backend)?;
    let payload = builder.ins().iadd_imm(
        entry,
        i64::try_from(payload_offset).map_err(|_| CompileError::Backend)?,
    );
    builder
        .ins()
        .jump(done, &[payload.into(), is_string.into()]);

    builder.switch_to_block(compact_block);
    let payload = emit_compact_text_entry(builder, values, reference, generation, point, guard)?;
    let is_string = builder.ins().iconst(types::I8, 0);
    builder
        .ins()
        .jump(done, &[payload.into(), is_string.into()]);

    builder.switch_to_block(done);
    Ok(NativeTextEntry {
        payload: builder.block_params(done)[0],
        is_string: builder.block_params(done)[1],
    })
}

fn emit_normal_text_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    const TEXT_PROOF: i64 = (u32::MAX as i64) + 2;
    let local_cache = local_heap_cache(values, reference);
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached_kind = builder.use_var(cache.object_kind);
        let proven = builder
            .ins()
            .icmp_imm(IntCC::Equal, cached_kind, TEXT_PROOF);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(miss);
        let entry = emit_text_entry_miss(builder, values, reference, point, guard)?;
        let proof = builder.ins().iconst(types::I64, TEXT_PROOF);
        builder.def_var(cache.object_kind, proof);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    } else {
        emit_text_entry_miss(builder, values, reference, point, guard)?
    };
    Ok(entry)
}

fn emit_text_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_heap_entry(builder, values, reference, point, guard)?;
    let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let string = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(JIT_OBJECT_STR));
    let substring = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(JIT_OBJECT_SUBSTRING));
    let valid = builder.ins().bor(string, substring);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_object_guard(builder, values, invalid, point, guard)?;
    Ok(entry)
}

fn emit_compact_text_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    expected_generation: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, text_view_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    emit_object_guard(builder, values, outside, point, guard)?;

    let pages = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, text_view_pages),
    )?;
    let page_index = builder
        .ins()
        .ushr_imm(slot_index, i64::from(JIT_TEXT_VIEW_PAGE_SHIFT));
    let page_offset = builder.ins().imul_imm(
        page_index,
        i64::try_from(std_mem::size_of::<usize>()).map_err(|_| CompileError::Backend)?,
    );
    let page_address = builder.ins().iadd(pages, page_offset);
    let page = builder
        .ins()
        .load(values.pointer_type, table_mem_flags(), page_address, 0);
    let within_page = builder
        .ins()
        .band_imm(slot_index, i64::from(JIT_TEXT_VIEW_PAGE_MASK));
    let entry_offset = builder.ins().imul_imm(
        within_page,
        i64::try_from(JIT_TEXT_VIEW_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(page, entry_offset);
    let expected_generation = builder.ins().band_imm(
        expected_generation,
        i64::from(u32::MAX ^ TEXT_VIEW_GENERATION_TAG),
    );
    let generation = load_heap_value(builder, types::I32, entry, JIT_TEXT_VIEW_GENERATION_OFFSET)?;
    let root = load_heap_value(builder, types::I32, entry, JIT_TEXT_VIEW_ROOT_OFFSET)?;
    let stale = builder
        .ins()
        .icmp(IntCC::NotEqual, generation, expected_generation);
    let dead = builder
        .ins()
        .icmp_imm(IntCC::Equal, root, i64::from(u32::MAX));
    let invalid = builder.ins().bor(stale, dead);
    emit_object_guard(builder, values, invalid, point, guard)?;
    Ok(builder.ins().iadd_imm(
        entry,
        i64::try_from(JIT_TEXT_VIEW_PAYLOAD_OFFSET).map_err(|_| CompileError::Backend)?,
    ))
}

pub(super) fn emit_heap_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let local_cache = local_heap_cache(values, reference);
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached = builder.use_var(cache.entry);
        let present = builder.ins().icmp_imm(IntCC::NotEqual, cached, 0);
        builder.ins().brif(present, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        builder.ins().jump(done, &[cached.into()]);

        builder.switch_to_block(miss);
        let entry = emit_heap_entry_miss(builder, values, reference, point, guard)?;
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        let entry = builder.block_params(done)[0];
        builder.def_var(cache.entry, entry);
        entry
    } else {
        emit_heap_entry_miss(builder, values, reference, point, guard)?
    };
    Ok(entry)
}

pub(super) fn emit_heap_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    emit_object_guard(builder, values, outside, point, guard)?;

    let pages = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_pages),
    )?;
    let page_index = builder
        .ins()
        .ushr_imm(slot_index, i64::from(JIT_PAGE_SHIFT));
    let page_offset = builder.ins().imul_imm(
        page_index,
        i64::try_from(std_mem::size_of::<usize>()).map_err(|_| CompileError::Backend)?,
    );
    let page_address = builder.ins().iadd(pages, page_offset);
    let page = builder
        .ins()
        .load(values.pointer_type, table_mem_flags(), page_address, 0);
    let within_page = builder.ins().band_imm(slot_index, i64::from(JIT_PAGE_MASK));
    let entry_offset = builder.ins().imul_imm(
        within_page,
        i64::try_from(JIT_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(page, entry_offset);
    let expected_generation = builder.ins().ushr_imm(reference, 32);
    let expected_generation = builder.ins().ireduce(types::I32, expected_generation);
    let generation = load_heap_value(builder, types::I32, entry, JIT_ENTRY_GENERATION_OFFSET)?;
    let live = load_heap_value(builder, types::I32, entry, JIT_ENTRY_LIVE_OFFSET)?;
    let stale = builder
        .ins()
        .icmp(IntCC::NotEqual, generation, expected_generation);
    let dead = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, live, i64::from(JIT_ENTRY_LIVE_TAG));
    let invalid = builder.ins().bor(stale, dead);
    emit_object_guard(builder, values, invalid, point, guard)?;
    Ok(entry)
}

pub(super) fn local_heap_cache(
    values: NativeValues<'_>,
    reference: ir::Value,
) -> Option<LocalHeapCache> {
    let slot = values.heap_translations.borrow().local(reference)?;
    values.local_heap_caches.get(slot).copied().flatten()
}

pub(super) fn emit_object_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    invalid: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<(), CompileError> {
    match guard {
        ObjectGuard::Fault(stack) => {
            emit_fault_check(builder, values, invalid, EXIT_TYPE_MISMATCH, point, stack)
        }
        ObjectGuard::Replay(stack) => {
            emit_interpreter_replay(builder, values, invalid, point, stack)
        }
        ObjectGuard::Branch(target) => {
            let success = builder.create_block();
            builder.ins().brif(invalid, target, &[], success, &[]);
            builder.switch_to_block(success);
            Ok(())
        }
    }
}

pub(super) fn value_tag(kind: ScalarKind) -> Option<ValueTag> {
    Some(match kind {
        ScalarKind::Unit => ValueTag::Unit,
        ScalarKind::Bool => ValueTag::Bool,
        ScalarKind::Int => ValueTag::Int,
        ScalarKind::Float => ValueTag::Float,
        ScalarKind::Char => ValueTag::Char,
        ScalarKind::Object(_) => ValueTag::Obj,
        ScalarKind::Tagged(_) => return None,
        ScalarKind::Callback(_) => return None,
        ScalarKind::Operation => ValueTag::Op,
    })
}

pub(super) fn emit_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    tag: ir::Value,
    kind: ScalarKind,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let payload = match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool => {
            let byte = load_value(builder, types::I8, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, byte)
        }
        ScalarKind::Int | ScalarKind::Object(_) | ScalarKind::Callback(_) => {
            load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?
        }
        ScalarKind::Tagged(_) => {
            emit_tagged_value_payload(builder, values, value, tag, point, deopt_stack)?
        }
        ScalarKind::Char => {
            let scalar = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, scalar)
        }
        ScalarKind::Float => {
            let bits = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
            emit_canonical_float_guard(builder, values, bits, point, deopt_stack)?;
            bits
        }
        ScalarKind::Operation => {
            let operation = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, operation)
        }
    };
    Ok(payload)
}

pub(super) fn emit_tagged_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    tag: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let unit = builder.create_block();
    let boolean = builder.create_block();
    let narrow = builder.create_block();
    let full = builder.create_block();
    let float = builder.create_block();
    let invalid = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let mut dispatch = Switch::new();
    dispatch.set_entry(ValueTag::Unit as u128, unit);
    dispatch.set_entry(ValueTag::Bool as u128, boolean);
    dispatch.set_entry(ValueTag::Char as u128, narrow);
    dispatch.set_entry(ValueTag::Op as u128, narrow);
    dispatch.set_entry(ValueTag::Int as u128, full);
    dispatch.set_entry(ValueTag::Obj as u128, full);
    dispatch.set_entry(ValueTag::Callback as u128, full);
    dispatch.set_entry(ValueTag::EmptyCase as u128, full);
    dispatch.set_entry(ValueTag::Float as u128, float);
    dispatch.emit(builder, tag, invalid);

    builder.switch_to_block(unit);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(boolean);
    let payload = load_value(builder, types::I8, value, VALUE_PAYLOAD_OFFSET)?;
    let payload = builder.ins().uextend(types::I64, payload);
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(narrow);
    let payload = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
    let payload = builder.ins().uextend(types::I64, payload);
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(full);
    let payload = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(float);
    let payload = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
    emit_canonical_float_guard(builder, values, payload, point, deopt_stack)?;
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(invalid);
    let reject = builder.ins().iconst(types::I8, 1);
    emit_interpreter_replay(builder, values, reject, point, deopt_stack)?;
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_canonical_float_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    bits: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let exponent = builder.ins().band_imm(bits, 0x7ff0_0000_0000_0000);
    let exponent_is_nan = builder
        .ins()
        .icmp_imm(IntCC::Equal, exponent, 0x7ff0_0000_0000_0000);
    let fraction = builder.ins().band_imm(bits, 0x000f_ffff_ffff_ffff);
    let has_fraction = builder.ins().icmp_imm(IntCC::NotEqual, fraction, 0);
    let is_nan = builder.ins().band(exponent_is_nan, has_fraction);
    let canonical = builder
        .ins()
        .icmp_imm(IntCC::Equal, bits, CANONICAL_NAN_BITS as i64);
    let not_canonical = builder.ins().bnot(canonical);
    let noncanonical = builder.ins().band(is_nan, not_canonical);
    emit_interpreter_replay(builder, values, noncanonical, point, deopt_stack)
}

pub(super) fn extend_stack_roots(
    roots: &mut Vec<NativeRoot>,
    kinds: &[ScalarKind],
    values: &[NativeValue],
) -> Result<(), CompileError> {
    if kinds.len() != values.len() {
        return Err(CompileError::Backend);
    }
    for (kind, value) in kinds.iter().copied().zip(values.iter().copied()) {
        if is_root_kind(kind) {
            roots.push(NativeRoot {
                bits: value.bits,
                tag: value.tag,
                state: None,
            });
        }
    }
    Ok(())
}

pub(super) fn collect_native_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
) -> Result<Vec<NativeRoot>, CompileError> {
    let mut roots = Vec::new();
    for (slot, (kind, variable)) in local_kinds
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
    extend_stack_roots(&mut roots, stack_kinds, stack)?;
    Ok(roots)
}

pub(super) fn emit_graph_digest(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    ty: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
    exit: ReplayEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let ty = builder.ins().iconst(types::I32, i64::from(ty));
    let collection = builder.ins().iconst(types::I32, 1);
    let digest = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, digest_value),
    )?;
    let call = builder.ins().call_indirect(
        values.digest_signature,
        digest,
        &[
            values.runtime_context,
            reference,
            ty,
            environment,
            collection,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

pub(super) fn emit_heap_operation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    arguments: [ir::Value; 3],
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.heap_operation_signature,
        function,
        &[
            values.runtime_context,
            arguments[0],
            arguments[1],
            arguments[2],
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        exit.point,
        exit.fault_stack,
    )?;
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}
