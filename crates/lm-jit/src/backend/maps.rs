//! Map, equality, and graph helper emission.

use super::*;

pub(super) fn emit_map_reserve_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let reserve_map = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, map_reserve),
    )?;
    let call = builder.ins().call_indirect(
        values.list_reserve_signature,
        reserve_map,
        &[values.runtime_context, reference, additional, root_count],
    );
    Ok(builder.inst_results(call)[0])
}

pub(super) fn emit_raw_map_value_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    first: ir::Value,
    second: ir::Value,
) -> Result<(ir::Value, NativeValue), CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        function,
        &[
            values.runtime_context,
            reference,
            first,
            second,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    Ok((status, NativeValue { bits, tag }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_optional_map_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    key: NativeValue,
    family: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let (status, found_value) = emit_raw_map_value_call(
        builder,
        values,
        function_offset,
        reference,
        key.bits,
        key.tag,
    )?;
    emit_runtime_fault_status(builder, values, status, exit.point, exit.fault_stack)?;
    let found = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
    let missing = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_MAP_VACANT));
    let valid = builder.ins().bor(found, missing);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;

    let found_block = builder.create_block();
    let missing_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder
        .ins()
        .brif(found, found_block, &[], missing_block, &[]);

    builder.switch_to_block(found_block);
    emit_native_value_contract(
        builder,
        values,
        found_value,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(done, &[found_value.bits.into(), found_value.tag.into()]);

    builder.switch_to_block(missing_block);
    let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let bits = builder.ins().bor(family, arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(done, &[bits.into(), tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_map_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    first: ir::Value,
    second: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let (status, result) =
        emit_raw_map_value_call(builder, values, function_offset, reference, first, second)?;
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

pub(super) fn emit_object_binary_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    argument: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_binary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            argument,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = NativeValue {
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
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

pub(super) fn emit_object_unary_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_unary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = NativeValue {
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
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

pub(super) fn emit_map_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: MapLookupEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let Some(key_kind) = direct_map_key_kind(emission.key_contract) else {
        return emit_map_lookup_slow(builder, values, emission);
    };

    let entry = emit_object_entry(
        builder,
        values,
        emission.reference,
        JIT_OBJECT_MAP,
        emission.exit.point,
        ObjectGuard::Replay(emission.exit.deopt_stack),
    )?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let key = emit_direct_map_key(builder, values, emission.key, key_kind, emission.exit)?;
    let probe_start = builder.create_block();
    builder.ins().brif(key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        entry,
        entry_count,
        emission.key,
        key,
        emission.exit,
    )?;
    match emission.result {
        MapLookupResult::Has => {
            let found = builder.ins().uextend(types::I64, probe.found);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::Bool as u64 as i64);
            builder.ins().jump(done, &[found.into(), tag.into()]);
        }
        MapLookupResult::At => {
            let hit = builder.create_block();
            builder.ins().brif(probe.found, hit, &[], slow, &[]);
            builder.switch_to_block(hit);
            builder
                .ins()
                .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);
        }
        MapLookupResult::Get { family, value } => {
            let hit = builder.create_block();
            let missing = builder.create_block();
            builder.ins().brif(probe.found, hit, &[], missing, &[]);

            builder.switch_to_block(hit);
            emit_native_value_contract(
                builder,
                values,
                probe.value,
                value,
                emission.exit.point,
                emission.exit.deopt_stack,
            )?;
            builder
                .ins()
                .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);

            builder.switch_to_block(missing);
            let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
            let bits = builder.ins().bor(family, arm);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
            builder.ins().jump(done, &[bits.into(), tag.into()]);
        }
    }

    builder.switch_to_block(slow);
    let result = emit_map_lookup_slow(builder, values, emission)?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_map_remove(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    key_contract: ValueContract,
    option_family: ir::Value,
    value_contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let Some(key_kind) = direct_map_key_kind(key_contract) else {
        return emit_optional_map_value(
            builder,
            values,
            std_mem::offset_of!(RawNativeFunctions, map_remove),
            reference,
            key,
            option_family,
            value_contract,
            exit,
        );
    };
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, map_entry, exit)?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, map_entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let index_ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(index_ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let direct_key = emit_direct_map_key(builder, values, key, key_kind, exit)?;
    let probe_start = builder.create_block();
    builder
        .ins()
        .brif(direct_key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        map_entry,
        entry_count,
        key,
        direct_key,
        exit,
    )?;
    let hit = builder.create_block();
    let missing = builder.create_block();
    builder.ins().brif(probe.found, hit, &[], missing, &[]);

    builder.switch_to_block(missing);
    let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let none_bits = builder.ins().bor(option_family, none_arm);
    let none_tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder
        .ins()
        .jump(done, &[none_bits.into(), none_tag.into()]);

    builder.switch_to_block(hit);
    emit_native_value_contract(
        builder,
        values,
        probe.value,
        value_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let live = load_heap_value(builder, types::I32, map_entry, JIT_MAP_LIVE_OFFSET)?;
    let has_live_entry = builder.ins().icmp_imm(IntCC::NotEqual, live, 0);
    let next_live = builder.ins().iadd_imm(live, -1);
    let next_live_native = builder.ins().uextend(values.pointer_type, next_live);
    let tombstones = builder.ins().isub(entry_count, next_live_native);
    let compaction_floor = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, tombstones, 8);
    let weighted_tombstones = builder.ins().imul_imm(tombstones, 3);
    let compaction_ratio =
        builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, weighted_tombstones, entry_count);
    let needs_compaction = builder.ins().band(compaction_floor, compaction_ratio);
    let no_compaction = builder.ins().bxor_imm(needs_compaction, 1);
    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch_ready = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, epoch, i64::from(u32::MAX));
    let fast = builder.ins().band(has_live_entry, no_compaction);
    let fast = builder.ins().band(fast, epoch_ready);
    let commit = builder.create_block();
    builder.ins().brif(fast, commit, &[], slow, &[]);

    builder.switch_to_block(commit);
    let zero = builder.ins().iconst(types::I64, 0);
    let uninit = builder
        .ins()
        .iconst(types::I64, ValueTag::Uninit as u64 as i64);
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
        zero,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
        uninit,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_VALUE_OFFSET + VALUE_PAYLOAD_OFFSET,
        zero,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_VALUE_OFFSET + VALUE_TAG_OFFSET,
        uninit,
    )?;
    store_heap_value(builder, probe.entry, MAP_ENTRY_SEMANTIC_HASH_OFFSET, zero)?;
    let epoch_tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let incremented_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder
        .ins()
        .select(epoch_tracked, incremented_epoch, epoch);
    store_heap_value(builder, map_entry, JIT_MAP_LIVE_OFFSET, next_live)?;
    store_heap_value(builder, map_entry, JIT_MAP_EPOCH_OFFSET, next_epoch)?;
    builder
        .ins()
        .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);

    builder.switch_to_block(slow);
    let result = emit_optional_map_value(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, map_remove),
        reference,
        key,
        option_family,
        value_contract,
        exit,
    )?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

pub(super) fn emit_map_next_index(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    cursor: ir::Value,
    expected: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch = builder.ins().uextend(types::I64, epoch);
    let negative_epoch = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let wrong_epoch = builder.ins().icmp(IntCC::NotEqual, epoch, expected);
    let invalid_epoch = builder.ins().bor(negative_epoch, wrong_epoch);
    emit_interpreter_replay(builder, values, invalid_epoch, exit.point, exit.deopt_stack)?;
    let negative_cursor = builder.ins().icmp_imm(IntCC::SignedLessThan, cursor, 0);
    emit_interpreter_replay(
        builder,
        values,
        negative_cursor,
        exit.point,
        exit.deopt_stack,
    )?;

    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let scan = builder.create_block();
    let found = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.append_block_param(done, types::I64);
    let in_range = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, cursor, count_i64);
    let cursor_native = if values.pointer_type == types::I64 {
        cursor
    } else {
        builder.ins().ireduce(values.pointer_type, cursor)
    };
    builder
        .ins()
        .brif(in_range, scan, &[cursor_native.into()], missing, &[]);

    builder.switch_to_block(scan);
    let position = builder.block_params(scan)[0];
    let byte_offset = builder.ins().imul_imm(
        position,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, byte_offset);
    let tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let live = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, ValueTag::Uninit as u64 as i64);
    let next = builder.ins().iadd_imm(position, 1);
    let more = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next, entry_count);
    let tombstone = builder.ins().bxor_imm(live, 1);
    let continue_scan = builder.ins().band(tombstone, more);
    let next_or_missing = builder.create_block();
    builder
        .ins()
        .brif(live, found, &[position.into()], next_or_missing, &[]);

    builder.switch_to_block(next_or_missing);
    builder
        .ins()
        .brif(continue_scan, scan, &[next.into()], missing, &[]);

    builder.switch_to_block(found);
    let position = builder.block_params(found)[0];
    let position = if values.pointer_type == types::I64 {
        position
    } else {
        builder.ins().uextend(types::I64, position)
    };
    builder.ins().jump(done, &[position.into()]);

    builder.switch_to_block(missing);
    let none = builder.ins().iconst(types::I64, -1);
    builder.ins().jump(done, &[none.into()]);

    builder.switch_to_block(done);
    let result = builder.block_params(done)[0];
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::Int as u64 as i64);
    Ok(NativeValue { bits: result, tag })
}

pub(super) fn emit_map_entry_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    load_stored_value: bool,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, count_i64);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let byte_offset = builder.ins().imul_imm(
        native_index,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, byte_offset);
    let key_tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let tombstone = builder
        .ins()
        .icmp_imm(IntCC::Equal, key_tag, ValueTag::Uninit as u64 as i64);
    emit_interpreter_replay(builder, values, tombstone, exit.point, exit.deopt_stack)?;
    let offset = if load_stored_value {
        MAP_ENTRY_VALUE_OFFSET
    } else {
        MAP_ENTRY_KEY_OFFSET
    };
    let result = NativeValue {
        bits: load_heap_value(builder, types::I64, entry, offset + VALUE_PAYLOAD_OFFSET)?,
        tag: load_heap_value(builder, types::I64, entry, offset + VALUE_TAG_OFFSET)?,
    };
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

pub(super) fn emit_map_lookup_slow(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: MapLookupEmission<'_>,
) -> Result<NativeValue, CompileError> {
    match emission.result {
        MapLookupResult::Has => emit_runtime_value_lookup(
            builder,
            values,
            std_mem::offset_of!(RawNativeFunctions, map_has),
            emission.reference,
            emission.key,
            emission.exit,
        ),
        MapLookupResult::At => emit_runtime_value_lookup(
            builder,
            values,
            std_mem::offset_of!(RawNativeFunctions, map_at),
            emission.reference,
            emission.key,
            emission.exit,
        ),
        MapLookupResult::Get { family, value } => emit_optional_map_value(
            builder,
            values,
            std_mem::offset_of!(RawNativeFunctions, map_get),
            emission.reference,
            emission.key,
            family,
            value,
            emission.exit,
        ),
    }
}

#[derive(Clone, Copy)]
pub(super) struct DirectMapProbe {
    found: ir::Value,
    value: NativeValue,
    entry: ir::Value,
    vacant_slot: ir::Value,
}

#[derive(Clone, Copy)]
pub(super) enum DirectMapKeyKind {
    Scalar(ScalarKind),
    Str,
    Text,
    Bytes,
}

#[derive(Clone, Copy)]
pub(super) struct DirectMapKey {
    kind: DirectMapKeyKind,
    semantic_hash: ir::Value,
    lookup_hash: ir::Value,
    object_entry: Option<ir::Value>,
    ready: ir::Value,
}

pub(super) fn direct_map_key_kind(contract: ValueContract) -> Option<DirectMapKeyKind> {
    match (contract.kind, contract.object) {
        (
            kind @ (ScalarKind::Unit
            | ScalarKind::Bool
            | ScalarKind::Int
            | ScalarKind::Float
            | ScalarKind::Char),
            None,
        ) => Some(DirectMapKeyKind::Scalar(kind)),
        (ScalarKind::Object(_), Some(ObjectContract::Str)) => Some(DirectMapKeyKind::Str),
        (ScalarKind::Object(_), Some(ObjectContract::Text)) => Some(DirectMapKeyKind::Text),
        (ScalarKind::Object(_), Some(ObjectContract::Bytes)) => Some(DirectMapKeyKind::Bytes),
        _ => None,
    }
}

pub(super) fn emit_direct_map_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    key: NativeValue,
    kind: DirectMapKeyKind,
    exit: HeapExitEmission<'_>,
) -> Result<DirectMapKey, CompileError> {
    let (semantic_hash, lookup_hash, object_entry, ready) = match kind {
        DirectMapKeyKind::Scalar(kind) => {
            let semantic_hash = emit_scalar_map_semantic_hash(builder, key.bits, kind);
            let lookup_key = load_vmctx_value(
                builder,
                types::I64,
                values.activation_pointer,
                std_mem::offset_of!(RawNativeActivation, lookup_hash_key),
            )?;
            let lookup_hash = builder.ins().bxor(semantic_hash, lookup_key);
            let lookup_hash = emit_stable_hash_mix(builder, lookup_hash);
            let ready = builder.ins().iconst(types::I8, 1);
            (semantic_hash, lookup_hash, None, ready)
        }
        DirectMapKeyKind::Str | DirectMapKeyKind::Text | DirectMapKeyKind::Bytes => {
            let entry = match kind {
                DirectMapKeyKind::Str => emit_object_entry(
                    builder,
                    values,
                    key.bits,
                    JIT_OBJECT_STR,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Text => emit_text_entry(
                    builder,
                    values,
                    key.bits,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Bytes => emit_object_entry(
                    builder,
                    values,
                    key.bits,
                    JIT_OBJECT_BYTES,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let offset = match kind {
                DirectMapKeyKind::Str | DirectMapKeyKind::Text => JIT_TEXT_LOOKUP_HASH_OFFSET,
                DirectMapKeyKind::Bytes => JIT_BYTES_LOOKUP_HASH_OFFSET,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let semantic_offset = match kind {
                DirectMapKeyKind::Str | DirectMapKeyKind::Text => JIT_TEXT_SEMANTIC_HASH_OFFSET,
                DirectMapKeyKind::Bytes => JIT_BYTES_SEMANTIC_HASH_OFFSET,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let semantic_hash = load_heap_value(builder, types::I64, entry, semantic_offset)?;
            let lookup_hash = load_heap_value(builder, types::I64, entry, offset)?;
            let ready = builder.ins().icmp_imm(IntCC::NotEqual, lookup_hash, 0);
            (semantic_hash, lookup_hash, Some(entry), ready)
        }
    };
    Ok(DirectMapKey {
        kind,
        semantic_hash,
        lookup_hash,
        object_entry,
        ready,
    })
}

pub(super) fn emit_direct_map_probe(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    map_entry: ir::Value,
    entry_count: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<DirectMapProbe, CompileError> {
    let lookup_hash = direct_key.lookup_hash;
    let slots = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_DATA_OFFSET,
    )?;
    let slot_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_LEN_OFFSET,
    )?;
    let empty = builder.create_block();
    let start = builder.create_block();
    let probe = builder.create_block();
    let candidate = builder.create_block();
    let advance = builder.create_block();
    let found = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(empty, values.pointer_type);
    builder.append_block_param(probe, values.pointer_type);
    builder.append_block_param(probe, values.pointer_type);
    builder.append_block_param(candidate, values.pointer_type);
    builder.append_block_param(candidate, values.pointer_type);
    builder.append_block_param(advance, values.pointer_type);
    builder.append_block_param(advance, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.append_block_param(done, types::I8);
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);

    let has_slots = builder.ins().icmp_imm(IntCC::NotEqual, slot_count, 0);
    let zero_pointer = builder.ins().iconst(values.pointer_type, 0);
    builder
        .ins()
        .brif(has_slots, start, &[], empty, &[zero_pointer.into()]);

    builder.switch_to_block(empty);
    let vacant_slot = builder.block_params(empty)[0];
    let zero_i8 = builder.ins().iconst(types::I8, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(
        done,
        &[
            zero_i8.into(),
            zero_i64.into(),
            zero_i64.into(),
            zero_pointer.into(),
            vacant_slot.into(),
        ],
    );

    builder.switch_to_block(start);
    let right = builder.ins().rotr_imm(lookup_hash, 25);
    let left = builder.ins().rotl_imm(lookup_hash, 17);
    let mixed = builder.ins().bxor(lookup_hash, right);
    let mixed = builder.ins().bxor(mixed, left);
    let mask = builder.ins().iadd_imm(slot_count, -1);
    let first = builder.ins().band(mixed, mask);
    builder
        .ins()
        .jump(probe, &[first.into(), slot_count.into()]);

    builder.switch_to_block(probe);
    let slot = builder.block_params(probe)[0];
    let remaining = builder.block_params(probe)[1];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let entry_index = load_heap_value(builder, types::I32, slot_address, MAP_SLOT_ENTRY_OFFSET)?;
    let occupied = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, entry_index, u32::MAX as i64);
    builder.ins().brif(
        occupied,
        candidate,
        &[slot.into(), remaining.into()],
        empty,
        &[slot_address.into()],
    );

    builder.switch_to_block(candidate);
    let slot = builder.block_params(candidate)[0];
    let remaining = builder.block_params(candidate)[1];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let stored_hash = load_heap_value(builder, types::I64, slot_address, MAP_SLOT_HASH_OFFSET)?;
    let same_hash = builder.ins().icmp(IntCC::Equal, stored_hash, lookup_hash);
    builder.ins().brif(
        same_hash,
        found,
        &[slot.into()],
        advance,
        &[slot.into(), remaining.into()],
    );

    builder.switch_to_block(found);
    let slot = builder.block_params(found)[0];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let entry_index = load_heap_value(builder, types::I32, slot_address, MAP_SLOT_ENTRY_OFFSET)?;
    let entry_index = builder.ins().uextend(values.pointer_type, entry_index);
    let invalid = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, entry_index, entry_count);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    let entry_offset = builder.ins().imul_imm(
        entry_index,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let entry = builder.ins().iadd(entries, entry_offset);
    let equal = emit_direct_map_key_equal(builder, values, entry, key, direct_key, exit)?;
    let equal_block = builder.create_block();
    builder.ins().brif(
        equal,
        equal_block,
        &[],
        advance,
        &[slot.into(), remaining.into()],
    );

    builder.switch_to_block(equal_block);
    let value = NativeValue {
        bits: load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_VALUE_OFFSET + VALUE_PAYLOAD_OFFSET,
        )?,
        tag: load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_VALUE_OFFSET + VALUE_TAG_OFFSET,
        )?,
    };
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(
        done,
        &[
            one.into(),
            value.bits.into(),
            value.tag.into(),
            entry.into(),
            zero_pointer.into(),
        ],
    );

    builder.switch_to_block(advance);
    let slot = builder.block_params(advance)[0];
    let remaining = builder.block_params(advance)[1];
    let next = builder.ins().iadd_imm(slot, 1);
    let next = builder.ins().band(next, mask);
    let remaining = builder.ins().iadd_imm(remaining, -1);
    let continue_probe = builder.ins().icmp_imm(IntCC::NotEqual, remaining, 0);
    builder.ins().brif(
        continue_probe,
        probe,
        &[next.into(), remaining.into()],
        empty,
        &[zero_pointer.into()],
    );

    builder.switch_to_block(done);
    Ok(DirectMapProbe {
        found: builder.block_params(done)[0],
        value: NativeValue {
            bits: builder.block_params(done)[1],
            tag: builder.block_params(done)[2],
        },
        entry: builder.block_params(done)[3],
        vacant_slot: builder.block_params(done)[4],
    })
}

pub(super) fn emit_scalar_map_semantic_hash(
    builder: &mut FunctionBuilder<'_>,
    bits: ir::Value,
    kind: ScalarKind,
) -> ir::Value {
    match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool | ScalarKind::Int | ScalarKind::Char => bits,
        ScalarKind::Float => {
            let shifted = builder.ins().ishl_imm(bits, 1);
            let zero = builder.ins().icmp_imm(IntCC::Equal, shifted, 0);
            let zero_bits = builder.ins().iconst(types::I64, 0);
            builder.ins().select(zero, zero_bits, bits)
        }
        _ => bits,
    }
}

pub(super) fn emit_direct_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    match direct_key.kind {
        DirectMapKeyKind::Scalar(kind) => emit_scalar_map_key_equal(builder, entry, key, kind),
        DirectMapKeyKind::Str | DirectMapKeyKind::Text | DirectMapKeyKind::Bytes => {
            emit_object_map_key_equal(builder, values, entry, key, direct_key, exit)
        }
    }
}

pub(super) fn emit_object_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    map_entry: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let key_entry = direct_key.object_entry.ok_or(CompileError::Backend)?;
    let stored_tag = load_heap_value(
        builder,
        types::I64,
        map_entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let stored_bits = load_heap_value(
        builder,
        types::I64,
        map_entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
    )?;
    let matching_tag =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, stored_tag, ValueTag::Obj as u64 as i64);
    let identical = builder.ins().icmp(IntCC::Equal, stored_bits, key.bits);
    let identical = builder.ins().band(matching_tag, identical);
    let matched = builder.create_block();
    let inspect = builder.create_block();
    let compare = builder.create_block();
    let missed = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I8);
    builder.ins().brif(identical, matched, &[], inspect, &[]);

    builder.switch_to_block(inspect);
    builder.ins().brif(matching_tag, compare, &[], missed, &[]);

    builder.switch_to_block(compare);
    let stored_entry = match direct_key.kind {
        DirectMapKeyKind::Str => emit_object_entry(
            builder,
            values,
            stored_bits,
            JIT_OBJECT_STR,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Text => emit_text_entry(
            builder,
            values,
            stored_bits,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Bytes => emit_object_entry(
            builder,
            values,
            stored_bits,
            JIT_OBJECT_BYTES,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
    };
    let (data_offset, length_offset) = match direct_key.kind {
        DirectMapKeyKind::Str | DirectMapKeyKind::Text => {
            (JIT_TEXT_DATA_OFFSET, JIT_TEXT_BYTE_LEN_OFFSET)
        }
        DirectMapKeyKind::Bytes => (JIT_BYTES_DATA_OFFSET, JIT_BYTES_LEN_OFFSET),
        DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
    };
    let key_length = load_heap_value(builder, values.pointer_type, key_entry, length_offset)?;
    let stored_length = load_heap_value(builder, values.pointer_type, stored_entry, length_offset)?;
    let same_length = builder.ins().icmp(IntCC::Equal, key_length, stored_length);
    let compare_bytes = builder.create_block();
    builder
        .ins()
        .brif(same_length, compare_bytes, &[], missed, &[]);

    builder.switch_to_block(compare_bytes);
    let key_data = load_heap_value(builder, values.pointer_type, key_entry, data_offset)?;
    let stored_data = load_heap_value(builder, values.pointer_type, stored_entry, data_offset)?;
    let bytes_equal = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, bytes_equal),
    )?;
    let call = builder.ins().call_indirect(
        values.bytes_equal_signature,
        bytes_equal,
        &[key_data, stored_data, key_length],
    );
    let equal = builder.inst_results(call)[0];
    let equal = builder.ins().icmp_imm(IntCC::NotEqual, equal, 0);
    builder.ins().brif(equal, matched, &[], missed, &[]);

    builder.switch_to_block(matched);
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(done, &[one.into()]);

    builder.switch_to_block(missed);
    let zero = builder.ins().iconst(types::I8, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_scalar_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    entry: ir::Value,
    key: NativeValue,
    kind: ScalarKind,
) -> Result<ir::Value, CompileError> {
    let expected_tag = value_tag(kind).ok_or(CompileError::Backend)?;
    let stored_tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let valid = builder
        .ins()
        .icmp_imm(IntCC::Equal, stored_tag, expected_tag as u64 as i64);
    let stored_bits = match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool => {
            let bits = load_heap_value(
                builder,
                types::I8,
                entry,
                MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
            )?;
            builder.ins().uextend(types::I64, bits)
        }
        ScalarKind::Char => {
            let bits = load_heap_value(
                builder,
                types::I32,
                entry,
                MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
            )?;
            builder.ins().uextend(types::I64, bits)
        }
        ScalarKind::Int | ScalarKind::Float => load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
        )?,
        _ => return Err(CompileError::Backend),
    };
    let equal = if kind == ScalarKind::Float {
        let left = float_value(builder, stored_bits);
        let right = float_value(builder, key.bits);
        let equal = builder.ins().fcmp(FloatCC::Equal, left, right);
        let left_nan = builder.ins().fcmp(FloatCC::Unordered, left, left);
        let right_nan = builder.ins().fcmp(FloatCC::Unordered, right, right);
        let both_nan = builder.ins().band(left_nan, right_nan);
        builder.ins().bor(equal, both_nan)
    } else {
        builder.ins().icmp(IntCC::Equal, stored_bits, key.bits)
    };
    Ok(builder.ins().band(valid, equal))
}

pub(super) fn emit_runtime_value_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    argument: NativeValue,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let lookup = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        lookup,
        &[
            values.runtime_context,
            reference,
            argument.bits,
            argument.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    Ok(NativeValue { bits, tag })
}

pub(super) fn emit_value_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    left: NativeValue,
    right: NativeValue,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let matching_tags = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let same_tag = builder.ins().icmp(IntCC::Equal, left.tag, right.tag);
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .brif(same_tag, matching_tags, &[], done, &[zero.into()]);

    builder.switch_to_block(matching_tags);
    let is_object = builder
        .ins()
        .icmp_imm(IntCC::Equal, left.tag, ValueTag::Obj as u64 as i64);
    let mut is_simple =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, left.tag, ValueTag::Unit as u64 as i64);
    for tag in [
        ValueTag::Bool,
        ValueTag::Int,
        ValueTag::Char,
        ValueTag::Op,
        ValueTag::EmptyCase,
    ] {
        let matches = builder
            .ins()
            .icmp_imm(IntCC::Equal, left.tag, tag as u64 as i64);
        is_simple = builder.ins().bor(is_simple, matches);
    }
    let same_bits = builder.ins().icmp(IntCC::Equal, left.bits, right.bits);
    let same_object = builder.ins().band(is_object, same_bits);
    let fast = builder.ins().bor(same_object, is_simple);
    let fast_result = builder.ins().uextend(types::I64, same_bits);
    builder
        .ins()
        .brif(fast, done, &[fast_result.into()], slow, &[]);

    builder.switch_to_block(slow);
    let equal = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, value_equal),
    )?;
    let call = builder.ins().call_indirect(
        values.value_equal_signature,
        equal,
        &[
            values.runtime_context,
            left.bits,
            left.tag,
            right.bits,
            right.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_typed_object_binary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    left: ir::Value,
    right: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_binary_signature,
        function,
        &[
            values.runtime_context,
            left,
            right,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
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

pub(super) fn emit_typed_object_unary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_unary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
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

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_map_put(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    key_contract: ValueContract,
    stored: NativeValue,
    option_family: Option<ir::Value>,
    previous_contract: ValueContract,
    roots: &[NativeRoot],
    own_text_key: bool,
    exit: HeapExitEmission<'_>,
) -> Result<Option<NativeValue>, CompileError> {
    let Some(key_kind) = direct_map_key_kind(key_contract) else {
        return emit_map_put_slow(
            builder,
            values,
            reference,
            key,
            stored,
            option_family,
            previous_contract,
            roots,
            own_text_key,
            exit,
        );
    };
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, map_entry, exit)?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, map_entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let index_ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    if option_family.is_some() {
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I64);
    }
    builder.ins().brif(index_ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let direct_key = emit_direct_map_key(builder, values, key, key_kind, exit)?;
    let probe_start = builder.create_block();
    builder
        .ins()
        .brif(direct_key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        map_entry,
        entry_count,
        key,
        direct_key,
        exit,
    )?;
    let replace = builder.create_block();
    let insert = builder.create_block();
    builder.ins().brif(probe.found, replace, &[], insert, &[]);

    builder.switch_to_block(replace);
    emit_native_value_contract(
        builder,
        values,
        probe.value,
        previous_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let value_address = builder.ins().iadd_imm(
        probe.entry,
        i64::try_from(MAP_ENTRY_VALUE_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, value_address, stored, previous_contract.kind)?;
    if option_family.is_some() {
        builder
            .ins()
            .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(insert);
    let entry_capacity = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_CAPACITY_OFFSET,
    )?;
    let has_entry_capacity =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, entry_count, entry_capacity);
    let max_entry_count = builder
        .ins()
        .iconst(values.pointer_type, i64::from(u32::MAX));
    let count_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, entry_count, max_entry_count);
    let next_count = builder.ins().iadd_imm(entry_count, 1);

    let live = load_heap_value(builder, types::I32, map_entry, JIT_MAP_LIVE_OFFSET)?;
    let live_native = builder.ins().uextend(values.pointer_type, live);
    let live_valid = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, live_native, entry_count);
    let live_fits = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, live, i64::from(u32::MAX));
    let next_live = builder.ins().iadd_imm(live, 1);

    let slot_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_LEN_OFFSET,
    )?;
    let has_vacant_slot = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, probe.vacant_slot, 0);
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let slots_i64 = if values.pointer_type == types::I64 {
        slot_count
    } else {
        builder.ins().uextend(types::I64, slot_count)
    };
    let required_slots = builder.ins().iadd_imm(count_i64, 1);
    let required_slots = builder.ins().imul_imm(required_slots, 3);
    let available_slots = builder.ins().imul_imm(slots_i64, 2);
    let load_factor_ready = builder.ins().icmp(
        IntCC::UnsignedLessThanOrEqual,
        required_slots,
        available_slots,
    );

    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch_ready = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, epoch, i64::from(u32::MAX));
    let epoch_tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let incremented_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder
        .ins()
        .select(epoch_tracked, incremented_epoch, epoch);

    let object_bytes = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_ENTRY_BYTES_OFFSET,
    )?;
    let next_object_bytes = builder
        .ins()
        .iadd_imm(object_bytes, JIT_MAP_ENTRY_COST as i64);
    let object_charge_ready = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        next_object_bytes,
        object_bytes,
    );
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, JIT_MAP_ENTRY_COST as i64);
    let heap_charge_ready = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let below_threshold = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_used, threshold);

    let mut fast = builder.ins().band(has_entry_capacity, count_fits);
    fast = builder.ins().band(fast, live_valid);
    fast = builder.ins().band(fast, live_fits);
    fast = builder.ins().band(fast, has_vacant_slot);
    fast = builder.ins().band(fast, load_factor_ready);
    fast = builder.ins().band(fast, epoch_ready);
    fast = builder.ins().band(fast, object_charge_ready);
    fast = builder.ins().band(fast, heap_charge_ready);
    fast = builder.ins().band(fast, below_threshold);
    if own_text_key {
        fast = builder.ins().iconst(types::I8, 0);
    }
    let commit = builder.create_block();
    builder.ins().brif(fast, commit, &[], slow, &[]);

    builder.switch_to_block(commit);
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let entry_offset = builder.ins().imul_imm(
        entry_count,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, entry_offset);
    let key_address = builder.ins().iadd_imm(
        entry,
        i64::try_from(MAP_ENTRY_KEY_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, key_address, key, key_contract.kind)?;
    let value_address = builder.ins().iadd_imm(
        entry,
        i64::try_from(MAP_ENTRY_VALUE_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, value_address, stored, previous_contract.kind)?;
    store_heap_value(
        builder,
        entry,
        MAP_ENTRY_SEMANTIC_HASH_OFFSET,
        direct_key.semantic_hash,
    )?;
    store_heap_value(
        builder,
        probe.vacant_slot,
        MAP_SLOT_HASH_OFFSET,
        direct_key.lookup_hash,
    )?;
    let entry_index = builder.ins().ireduce(types::I32, entry_count);
    store_heap_value(
        builder,
        probe.vacant_slot,
        MAP_SLOT_ENTRY_OFFSET,
        entry_index,
    )?;
    store_heap_value(builder, map_entry, JIT_MAP_EPOCH_OFFSET, next_epoch)?;
    store_heap_value(
        builder,
        map_entry,
        JIT_ENTRY_BYTES_OFFSET,
        next_object_bytes,
    )?;
    store_heap_value(builder, used_pointer, 0, next_used)?;
    store_heap_value(builder, map_entry, JIT_MAP_ENTRIES_LEN_OFFSET, next_count)?;
    store_heap_value(builder, map_entry, JIT_MAP_LIVE_OFFSET, next_live)?;
    let next_built = builder.ins().ireduce(types::I32, next_count);
    store_heap_value(builder, map_entry, JIT_MAP_INDEX_BUILT_OFFSET, next_built)?;
    if let Some(option_family) = option_family {
        let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
        let bits = builder.ins().bor(option_family, none_arm);
        let tag = builder
            .ins()
            .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
        builder.ins().jump(done, &[bits.into(), tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(slow);
    let result = emit_map_put_slow(
        builder,
        values,
        reference,
        key,
        stored,
        option_family,
        previous_contract,
        roots,
        own_text_key,
        exit,
    )?;
    if let Some(result) = result {
        builder
            .ins()
            .jump(done, &[result.bits.into(), result.tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(done);
    Ok(option_family.map(|_| NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_map_put_slow(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    stored: NativeValue,
    option_family: Option<ir::Value>,
    previous_contract: ValueContract,
    roots: &[NativeRoot],
    own_text_key: bool,
    exit: HeapExitEmission<'_>,
) -> Result<Option<NativeValue>, CompileError> {
    let Some(option_family) = option_family else {
        let root_count = emit_runtime_roots(builder, values, roots)?;
        let own_text_key = builder
            .ins()
            .iconst(types::I32, i64::from(u8::from(own_text_key)));
        let discard = load_value(
            builder,
            values.pointer_type,
            values.runtime_functions,
            std_mem::offset_of!(RawNativeFunctions, map_put_discard),
        )?;
        let call = builder.ins().call_indirect(
            values.map_put_discard_signature,
            discard,
            &[
                values.runtime_context,
                reference,
                key.bits,
                key.tag,
                stored.bits,
                stored.tag,
                own_text_key,
                root_count,
            ],
        );
        let status = builder.inst_results(call)[0];
        emit_runtime_status(
            builder,
            values,
            status,
            exit.point,
            exit.fault_stack,
            exit.deopt_stack,
        )?;
        return Ok(None);
    };

    let probe = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, map_put_probe),
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        probe,
        &[
            values.runtime_context,
            reference,
            key.bits,
            key.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_fault_status(builder, values, status, exit.point, exit.fault_stack)?;
    let existing = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
    let vacant = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_MAP_VACANT));
    let valid = builder.ins().bor(existing, vacant);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;

    let token = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        16,
    );
    let entry_count = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        24,
    );
    let existing_block = builder.create_block();
    let vacant_block = builder.create_block();
    let ready = builder.create_block();
    builder.append_block_param(ready, types::I64);
    builder.append_block_param(ready, types::I64);
    builder
        .ins()
        .brif(vacant, vacant_block, &[], existing_block, &[]);

    builder.switch_to_block(existing_block);
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    let previous = NativeValue { bits, tag };
    emit_native_value_contract(
        builder,
        values,
        previous,
        previous_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(ready, &[previous.bits.into(), previous.tag.into()]);

    builder.switch_to_block(vacant_block);
    let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let bits = builder.ins().bor(option_family, none_arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(ready, &[bits.into(), tag.into()]);

    builder.switch_to_block(ready);
    let result = NativeValue {
        bits: builder.block_params(ready)[0],
        tag: builder.block_params(ready)[1],
    };

    let root_count = emit_runtime_roots(builder, values, roots)?;
    let own_text_key = builder
        .ins()
        .iconst(types::I32, i64::from(u8::from(own_text_key)));
    let commit = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, map_put_commit),
    )?;
    let zero = builder.ins().iconst(types::I32, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let vacant = builder.ins().select(vacant, one, zero);
    let call = builder.ins().call_indirect(
        values.map_put_commit_signature,
        commit,
        &[
            values.runtime_context,
            reference,
            key.bits,
            key.tag,
            stored.bits,
            stored.tag,
            token,
            entry_count,
            vacant,
            own_text_key,
            root_count,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(Some(result))
}

pub(super) fn emit_map_len(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let len = load_value(builder, types::I32, entry, JIT_MAP_LIVE_OFFSET)?;
    Ok(builder.ins().uextend(types::I64, len))
}

pub(super) fn emit_map_epoch(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_MAP_EPOCH_OFFSET)?;
    let unobserved = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let observed = builder.ins().select(unobserved, one, epoch);
    store_i32_value(builder, entry, JIT_MAP_EPOCH_OFFSET, observed)?;
    Ok(builder.ins().uextend(types::I64, observed))
}

pub(super) fn emit_map_iter_len(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_MAP_EPOCH_OFFSET)?;
    let expected_epoch = builder.ins().ireduce(types::I32, expected);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let changed = builder.ins().icmp(IntCC::NotEqual, epoch, expected_epoch);
    let invalid = builder.ins().bor(negative, changed);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let len = load_value(builder, types::I32, entry, JIT_MAP_LIVE_OFFSET)?;
    Ok(builder.ins().uextend(types::I64, len))
}

pub(super) fn emit_digest_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    left: ir::Value,
    right: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let left = emit_object_entry(
        builder,
        values,
        left,
        JIT_OBJECT_DIGEST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let right = emit_object_entry(
        builder,
        values,
        right,
        JIT_OBJECT_DIGEST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let mut equal = builder.ins().iconst(types::I8, 1);
    for word in 0..4 {
        let offset = JIT_DIGEST_BYTES_OFFSET
            .checked_add(word * std_mem::size_of::<u64>())
            .ok_or(CompileError::Backend)?;
        let left_word = load_value(builder, types::I64, left, offset)?;
        let right_word = load_value(builder, types::I64, right, offset)?;
        let word_equal = builder.ins().icmp(IntCC::Equal, left_word, right_word);
        equal = builder.ins().band(equal, word_equal);
    }
    Ok(equal)
}
