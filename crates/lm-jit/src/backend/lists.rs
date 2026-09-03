//! List access and mutation emission.

use super::*;

pub(super) fn emit_list_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

pub(super) fn emit_list_capacity(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        capacity
    } else {
        builder.ins().uextend(types::I64, capacity)
    })
}

pub(super) fn emit_list_epoch(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let unobserved = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let observed = builder.ins().select(unobserved, one, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, observed)?;
    Ok(builder.ins().uextend(types::I64, observed))
}

pub(super) fn emit_list_iter_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    expected: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let expected_epoch = builder.ins().ireduce(types::I32, expected);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let changed = builder.ins().icmp(IntCC::NotEqual, epoch, expected_epoch);
    let invalid = builder.ins().bor(negative, changed);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

pub(super) fn emit_seal_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    allow_pending: bool,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let entry = if allow_pending {
        emit_instance_storage(
            builder,
            values,
            reference,
            Some(class),
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?
        .frozen
    } else {
        emit_instance_entry(
            builder,
            values,
            reference,
            class,
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?
        .0
    };
    let frozen = builder.ins().iconst(types::I8, 1);
    if allow_pending {
        store_i8_value(builder, entry, 0, frozen)
    } else {
        store_i8_value(builder, entry, JIT_ENTRY_FROZEN_OFFSET, frozen)
    }
}

pub(super) fn emit_list_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_list_get(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    family_type: u32,
    exit: HeapExitEmission<'_>,
    resolve: FaultPoint,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let array_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, array_index, len);
    let absent = builder.ins().bor(negative, outside);
    builder.ins().brif(absent, missing, &[], present, &[]);

    builder.switch_to_block(present);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, array_index)?;
    let value = emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(done, &[value.bits.into(), value.tag.into()]);

    builder.switch_to_block(missing);
    let family = emit_option_family(
        builder,
        values,
        function,
        family_type,
        resolve,
        exit.deopt_stack,
    )?;
    let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let payload = builder.ins().bor(family, arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(done, &[payload.into(), tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

pub(super) fn emit_list_pop(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    emission: ListOptionEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        emission.exit.point,
        ObjectGuard::Fault(emission.exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, emission.exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    let empty = builder.ins().icmp_imm(IntCC::Equal, len, 0);
    builder.ins().brif(empty, missing, &[], present, &[]);

    builder.switch_to_block(present);
    let last = builder.ins().iadd_imm(len, -1);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, last)?;
    let result = emit_loaded_value(
        builder,
        values,
        address,
        emission.result,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    emit_list_epoch_bump(builder, values, entry, emission.exit)?;
    store_list_len(builder, entry, last)?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    emit_list_shrink_charge(builder, values, entry, one)?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(missing);
    let family = emit_option_family(
        builder,
        values,
        emission.function,
        emission.family_type,
        emission.resolve,
        emission.exit.deopt_stack,
    )?;
    let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let payload = builder.ins().bor(family, arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(done, &[payload.into(), tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

pub(super) fn emit_list_insert(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: ListInsertEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        emission.stored.bits,
        emission.contract,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        emission.reference,
        JIT_OBJECT_LIST,
        emission.exit.point,
        ObjectGuard::Fault(emission.exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, emission.exit)?;
    let negative = builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, emission.index, 0);
    let native_index = native_size(builder, values, emission.index, emission.exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, native_index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(
        builder,
        values,
        invalid,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    emit_list_epoch_guard(builder, values, entry, emission.exit)?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    let has_capacity = builder.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), used_pointer, 0);
    let next_used = builder.ins().iadd_imm(used, VALUE_SIZE as i64);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let slow_charge = builder.ins().bor(charge_overflow, collection_due);
    let fast_charge = builder.ins().bxor_imm(slow_charge, 1);
    let fast = builder.ins().band(has_capacity, fast_charge);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let source = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, native_index)?;
    let destination = builder.ins().iadd_imm(source, VALUE_SIZE as i64);
    let moved = builder.ins().isub(len, native_index);
    let moved = builder.ins().imul_imm(moved, VALUE_SIZE as i64);
    builder.call_memmove(values.frontend_config, destination, source, moved);
    emit_store_value(builder, source, emission.stored, emission.contract.kind)?;
    let next_len = builder.ins().iadd_imm(len, 1);
    store_list_len(builder, entry, next_len)?;
    emit_list_epoch_bump(builder, values, entry, emission.exit)?;
    emit_list_growth_charge(builder, values, entry, next_used, used_pointer)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow_block);
    let status = emit_list_insert_call(
        builder,
        values,
        emission.reference,
        emission.index,
        emission.stored,
        emission.roots,
    )?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        emission.exit.point,
        emission.exit.fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(
        builder,
        values,
        replay,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_list_remove(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    swap: bool,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    let removed = emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )?;
    emit_list_epoch_bump(builder, values, entry, exit)?;
    let last = builder.ins().iadd_imm(len, -1);
    let source_index = if swap {
        last
    } else {
        builder.ins().iadd_imm(index, 1)
    };
    let source = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, source_index)?;
    let moved = if swap {
        builder.ins().iconst(values.pointer_type, VALUE_SIZE as i64)
    } else {
        let count = builder.ins().isub(last, index);
        builder.ins().imul_imm(count, VALUE_SIZE as i64)
    };
    builder.call_memmove(values.frontend_config, address, source, moved);
    store_list_len(builder, entry, last)?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    emit_list_shrink_charge(builder, values, entry, one)?;
    Ok(removed)
}

pub(super) fn emit_list_truncate(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    length: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, length, 0);
    emit_interpreter_replay(builder, values, negative, exit.point, exit.deopt_stack)?;
    let length = native_size(builder, values, length, exit)?;
    let current = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let changed = builder.ins().icmp(IntCC::UnsignedLessThan, length, current);
    let update = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(changed, update, &[], done, &[]);

    builder.switch_to_block(update);
    emit_list_epoch_bump(builder, values, entry, exit)?;
    store_list_len(builder, entry, length)?;
    let removed = builder.ins().isub(current, length);
    emit_list_shrink_charge(builder, values, entry, removed)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn native_size(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    if values.pointer_type == types::I64 {
        return Ok(value);
    }
    let too_large = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, too_large, exit.point, exit.deopt_stack)?;
    Ok(builder.ins().ireduce(values.pointer_type, value))
}

pub(super) fn emit_list_epoch_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, exhausted, exit.point, exit.deopt_stack)
}

pub(super) fn emit_list_epoch_bump(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, exhausted, exit.point, exit.deopt_stack)?;
    let tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let next = builder.ins().iadd_imm(epoch, 1);
    let next = builder.ins().select(tracked, next, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, next)
}

pub(super) fn store_list_len(
    builder: &mut FunctionBuilder<'_>,
    entry: ir::Value,
    len: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET)
        .map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), len, entry, offset);
    Ok(())
}

pub(super) fn emit_list_growth_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    next_used: ir::Value,
    used_pointer: ir::Value,
) -> Result<(), CompileError> {
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().iadd_imm(object_bytes, VALUE_SIZE as i64);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    builder
        .ins()
        .store(MemFlags::new(), next_used, used_pointer, 0);
    Ok(())
}

pub(super) fn emit_list_shrink_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    removed: ir::Value,
) -> Result<(), CompileError> {
    let bytes = builder.ins().imul_imm(removed, VALUE_SIZE as i64);
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().isub(object_bytes, bytes);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), used_pointer, 0);
    let used = builder.ins().isub(used, bytes);
    builder.ins().store(MemFlags::new(), used, used_pointer, 0);
    Ok(())
}

pub(super) fn emit_list_set(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    stored: NativeValue,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    emit_store_value(builder, address, stored, contract.kind)
}

pub(super) fn emit_list_swap(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    first: ir::Value,
    second: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let first = emit_checked_list_index(builder, values, entry, first, exit)?;
    let second = emit_checked_list_index(builder, values, entry, second, exit)?;
    let different = builder.ins().icmp(IntCC::NotEqual, first, second);
    let swap = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(different, swap, &[], done, &[]);

    builder.switch_to_block(swap);
    emit_list_epoch_bump(builder, values, entry, exit)?;
    let first_address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, first)?;
    let second_address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, second)?;
    let first_bits = load_heap_value(builder, types::I64, first_address, VALUE_PAYLOAD_OFFSET)?;
    let first_tag = load_heap_value(builder, types::I64, first_address, VALUE_TAG_OFFSET)?;
    let second_bits = load_heap_value(builder, types::I64, second_address, VALUE_PAYLOAD_OFFSET)?;
    let second_tag = load_heap_value(builder, types::I64, second_address, VALUE_TAG_OFFSET)?;
    store_heap_value(builder, first_address, VALUE_PAYLOAD_OFFSET, second_bits)?;
    store_heap_value(builder, first_address, VALUE_TAG_OFFSET, second_tag)?;
    store_heap_value(builder, second_address, VALUE_PAYLOAD_OFFSET, first_bits)?;
    store_heap_value(builder, second_address, VALUE_TAG_OFFSET, first_tag)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_list_push(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    contract: ValueContract,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let epoch_exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(
        builder,
        values,
        epoch_exhausted,
        exit.point,
        exit.deopt_stack,
    )?;

    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    let has_capacity = builder.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), used_pointer, 0);
    let next_used = builder.ins().iadd_imm(used, VALUE_SIZE as i64);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let slow_charge = builder.ins().bor(charge_overflow, collection_due);
    let fast_charge = builder.ins().bxor_imm(slow_charge, 1);
    let fast = builder.ins().band(has_capacity, fast_charge);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, len)?;
    emit_store_value(builder, address, stored, contract.kind)?;
    let next_len = builder.ins().iadd_imm(len, 1);
    let len_offset = i32::try_from(JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET)
        .map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), next_len, entry, len_offset);
    let tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let next_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder.ins().select(tracked, next_epoch, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, next_epoch)?;
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().iadd_imm(object_bytes, VALUE_SIZE as i64);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    builder
        .ins()
        .store(MemFlags::new(), next_used, used_pointer, 0);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow_block);
    let status = emit_list_growth_call(builder, values, reference, stored, roots)?;
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
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_list_reserve(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, additional, 0);
    emit_interpreter_replay(builder, values, negative, exit.point, exit.deopt_stack)?;
    let native_additional = if values.pointer_type == types::I64 {
        additional
    } else {
        let too_large =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, additional, i64::from(u32::MAX));
        emit_interpreter_replay(builder, values, too_large, exit.point, exit.deopt_stack)?;
        builder.ins().ireduce(values.pointer_type, additional)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    let spare = builder.ins().isub(capacity, len);
    let enough = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, native_additional, spare);
    let fast = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(enough, fast, &[], slow, &[]);

    builder.switch_to_block(fast);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow);
    let status = emit_list_reserve_call(builder, values, reference, additional, roots)?;
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
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

pub(super) fn emit_list_reorder(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, exhausted, exit.point, exit.deopt_stack)?;
    let tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let next = builder.ins().iadd_imm(epoch, 1);
    let next = builder.ins().select(tracked, next, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, next)
}

pub(super) fn emit_list_growth_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let grow_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, grow_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_growth_signature,
        grow_list,
        &[
            values.runtime_context,
            reference,
            stored.bits,
            stored.tag,
            root_count,
        ],
    );
    Ok(builder.inst_results(call)[0])
}

pub(super) fn emit_list_insert_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    stored: NativeValue,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let insert_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, insert_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_insert_signature,
        insert_list,
        &[
            values.runtime_context,
            reference,
            index,
            stored.bits,
            stored.tag,
            root_count,
        ],
    );
    Ok(builder.inst_results(call)[0])
}

pub(super) fn emit_list_reserve_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let reserve_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, reserve_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_reserve_signature,
        reserve_list,
        &[values.runtime_context, reference, additional, root_count],
    );
    Ok(builder.inst_results(call)[0])
}
