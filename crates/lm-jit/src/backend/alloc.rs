//! Allocation and scalar replacement emission.

use super::*;

pub(super) fn collect_capture_allocation_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
    capture_count: usize,
) -> Result<(Vec<NativeRoot>, usize), CompileError> {
    if stack_kinds.len() != stack.len() || capture_count > stack.len() {
        return Err(CompileError::Backend);
    }
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
    let stack_start = roots.len();
    roots.extend(stack.iter().copied().map(|value| NativeRoot {
        bits: value.bits,
        tag: value.tag,
        state: None,
    }));
    let capture_start = stack_start
        .checked_add(stack.len() - capture_count)
        .ok_or(CompileError::Backend)?;
    Ok((roots, capture_start))
}

pub(super) fn emit_capture_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: CaptureAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let CaptureAllocationEmission {
        function,
        environment,
        capture_start,
        capture_count,
        roots,
        callback,
        point,
        replay_stack,
        fault_stack,
    } = emission;
    let capture_end = capture_start
        .checked_add(capture_count)
        .ok_or(CompileError::Backend)?;
    let capture_roots = roots
        .get(capture_start..capture_end)
        .ok_or(CompileError::Backend)?;
    let fast_root_count = emit_runtime_roots(builder, values, capture_roots)?;
    let function = builder.ins().iconst(types::I32, i64::from(function));
    let fast_capture_start = builder.ins().iconst(types::I32, 0);
    let slow_capture_start = builder.ins().iconst(
        types::I32,
        i64::try_from(capture_start).map_err(|_| CompileError::Backend)?,
    );
    let capture_count = builder.ins().iconst(
        types::I32,
        i64::try_from(capture_count).map_err(|_| CompileError::Backend)?,
    );
    let function_offset = if callback {
        std_mem::offset_of!(RawNativeFunctions, allocate_callback)
    } else {
        std_mem::offset_of!(RawNativeFunctions, allocate_closure)
    };
    let allocation = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.capture_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            function,
            environment,
            no_collection,
            fast_capture_start,
            capture_count,
            fast_root_count,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let status = if callback {
        fast_status
    } else {
        let retry = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I32);
        let collection_required = builder.ins().icmp_imm(
            IntCC::Equal,
            fast_status,
            i64::from(RUNTIME_COLLECTION_REQUIRED),
        );
        builder
            .ins()
            .brif(collection_required, retry, &[], done, &[fast_status.into()]);

        builder.switch_to_block(retry);
        let root_count = emit_runtime_roots(builder, values, roots)?;
        let allow_collection = builder.ins().iconst(types::I32, 1);
        let slow_call = builder.ins().call_indirect(
            values.capture_allocation_signature,
            allocation,
            &[
                values.runtime_context,
                function,
                environment,
                allow_collection,
                slow_capture_start,
                capture_count,
                root_count,
                values.allocation_result_pointer,
            ],
        );
        let slow_status = builder.inst_results(slow_call)[0];
        builder.ins().jump(done, &[slow_status.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    };
    let limit_status = if callback {
        RUNTIME_STACK_LIMIT
    } else {
        RUNTIME_HEAP_LIMIT
    };
    let limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(limit_status));
    let limit_exit = if callback {
        EXIT_STACK_LIMIT
    } else {
        EXIT_HEAP_LIMIT
    };
    emit_fault_check(builder, values, limit, limit_exit, point, fault_stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

pub(super) fn emit_value_array_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: ValueArrayAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let ValueArrayAllocationEmission {
        kind,
        item_start,
        item_count,
        roots,
        point,
        replay_stack,
        fault_stack,
    } = emission;
    let item_end = item_start
        .checked_add(item_count)
        .ok_or(CompileError::Backend)?;
    let item_roots = roots
        .get(item_start..item_end)
        .ok_or(CompileError::Backend)?;
    let fast_root_count = emit_runtime_roots(builder, values, item_roots)?;
    let fast_item_start = builder.ins().iconst(types::I32, 0);
    let slow_item_start = builder.ins().iconst(
        types::I32,
        i64::try_from(item_start).map_err(|_| CompileError::Backend)?,
    );
    let item_count = builder.ins().iconst(
        types::I32,
        i64::try_from(item_count).map_err(|_| CompileError::Backend)?,
    );
    let function_offset = match kind {
        ValueArrayAllocationKind::Tuple => {
            std_mem::offset_of!(RawNativeFunctions, allocate_tuple)
        }
        ValueArrayAllocationKind::List => std_mem::offset_of!(RawNativeFunctions, allocate_list),
        ValueArrayAllocationKind::Map => std_mem::offset_of!(RawNativeFunctions, allocate_map),
    };
    let allocation = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.value_array_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            no_collection,
            fast_item_start,
            item_count,
            fast_root_count,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let retry = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    let collection_required = builder.ins().icmp_imm(
        IntCC::Equal,
        fast_status,
        i64::from(RUNTIME_COLLECTION_REQUIRED),
    );
    builder
        .ins()
        .brif(collection_required, retry, &[], done, &[fast_status.into()]);

    builder.switch_to_block(retry);
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let allow_collection = builder.ins().iconst(types::I32, 1);
    let slow_call = builder.ins().call_indirect(
        values.value_array_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            allow_collection,
            slow_item_start,
            item_count,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let slow_status = builder.inst_results(slow_call)[0];
    builder.ins().jump(done, &[slow_status.into()]);

    builder.switch_to_block(done);
    let status = builder.block_params(done)[0];
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        point,
        fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

pub(super) fn emit_allocate_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    emission: InstanceAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let InstanceAllocationEmission {
        roots,
        allow_pending,
        exit,
    } = emission;
    let (status, result) = if allow_pending {
        emit_requested_instance_allocation(builder, values, class, field_count, environment, roots)?
    } else {
        emit_instance_allocation(builder, values, class, field_count, environment, roots)?
    };
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        exit.point,
        exit.deopt_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    Ok(result)
}

pub(super) fn instance_field_count(input: &FunctionInput<'_>, class: u32) -> Option<u32> {
    let source_class = match input.root.class_relocation {
        Some(classes) => classes.iter().position(|relocated| *relocated == class)?,
        None => class as usize,
    };
    let count = input.root.source.classes.get(source_class)?.fields.len();
    u32::try_from(count).ok()
}

pub(super) fn emit_requested_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let request = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_request),
    )?;
    let zero = builder.ins().iconst(types::I32, 0);
    store_i32_value(
        builder,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_request),
        zero,
    )?;
    let requested = builder.ins().icmp_imm(IntCC::NotEqual, request, 0);
    let pending = builder.create_block();
    let canonical = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(requested, pending, &[], canonical, &[]);

    builder.switch_to_block(pending);
    let (status, result) = match field_count {
        Some(field_count) if field_count as usize <= VIRTUAL_INSTANCE_FIELDS => {
            emit_pending_instance_allocation(builder, values, class, field_count, environment)?
        }
        _ => {
            let status = builder
                .ins()
                .iconst(types::I32, i64::from(RUNTIME_COLLECTION_REQUIRED));
            let result = builder.ins().iconst(types::I64, 0);
            (status, result)
        }
    };
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(canonical);
    let (status, result) =
        emit_instance_allocation(builder, values, class, field_count, environment, roots)?;
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

pub(super) fn emit_pending_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: u32,
    environment: ir::Value,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let cost = (field_count as usize)
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    let cost = i64::try_from(cost).map_err(|_| CompileError::Backend)?;
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, cost);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let charge_blocked = builder.ins().bor(charge_overflow, collection_due);

    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let has_record = builder.ins().icmp_imm(IntCC::NotEqual, available, 0);
    let charge_ready = builder.ins().bxor_imm(charge_blocked, 1);
    let ready = builder.ins().band(has_record, charge_ready);

    let allocate = builder.create_block();
    let unavailable = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(ready, allocate, &[], unavailable, &[]);

    builder.switch_to_block(unavailable);
    let status = builder
        .ins()
        .iconst(types::I32, i64::from(RUNTIME_COLLECTION_REQUIRED));
    let result = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(allocate);
    let record_index = builder.ins().ctz(available);
    let next_available = builder.ins().iadd_imm(available, -1);
    let next_available = builder.ins().band(available, next_available);
    let available_offset =
        i32::try_from(std_mem::offset_of!(RawNativeActivation, virtual_available))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        next_available,
        values.activation_pointer,
        available_offset,
    );
    let instances = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let record_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(std_mem::size_of::<RawVirtualInstance>())
            .map_err(|_| CompileError::Backend)?,
    );
    let record = builder.ins().iadd(instances, record_offset);
    let fields = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    let field_record_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_FIELDS.saturating_mul(VALUE_SIZE))
            .map_err(|_| CompileError::Backend)?,
    );
    let field_data = builder.ins().iadd(fields, field_record_offset);
    let uninit = builder
        .ins()
        .iconst(types::I64, ValueTag::Uninit as u64 as i64);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    for field in 0..field_count as usize {
        let offset =
            i32::try_from(field.saturating_mul(VALUE_SIZE)).map_err(|_| CompileError::Backend)?;
        store_i64(
            builder,
            field_data,
            offset as usize + VALUE_TAG_OFFSET,
            uninit,
        )?;
        store_i64(
            builder,
            field_data,
            offset as usize + VALUE_PAYLOAD_OFFSET,
            zero_i64,
        )?;
    }
    let record_i32 = builder.ins().ireduce(types::I32, record_index);
    let maximum = builder.ins().iconst(types::I32, i64::from(u32::MAX));
    let token = builder.ins().isub(maximum, record_i32);
    let result = builder.ins().uextend(types::I64, token);
    let class_value = builder.ins().iconst(types::I32, i64::from(class));
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, next_live)?;
    let allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, inline_allocations),
    )?;
    let allocations = builder.ins().iadd_imm(allocations, 1);
    let allocations_offset =
        i32::try_from(std_mem::offset_of!(RawNativeActivation, inline_allocations))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        allocations,
        values.activation_pointer,
        allocations_offset,
    );
    let pending_allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, pending_instance_allocations),
    )?;
    let pending_allocations = builder.ins().iadd_imm(pending_allocations, 1);
    let pending_allocations_offset = i32::try_from(std_mem::offset_of!(
        RawNativeActivation,
        pending_instance_allocations
    ))
    .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        pending_allocations,
        values.activation_pointer,
        pending_allocations_offset,
    );

    let one_i32 = builder.ins().iconst(types::I32, 1);
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, active),
        one_i32,
    )?;
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, references),
        one_i32,
    )?;
    store_i64(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, object_bits),
        result,
    )?;
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, class),
        class_value,
    )?;
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, environment),
        environment,
    )?;
    let count = builder.ins().iconst(types::I32, i64::from(field_count));
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, field_count),
        count,
    )?;
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    store_i32_value(
        builder,
        record,
        std_mem::offset_of!(RawVirtualInstance, frozen),
        zero_i32,
    )?;
    let status = builder.ins().iconst(types::I32, i64::from(RUNTIME_OK));
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

pub(super) fn emit_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let Some(field_count) = field_count else {
        return emit_allocation_call(builder, values, class, environment, roots);
    };
    let cost = (field_count as usize)
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    let cost = i64::try_from(cost).map_err(|_| CompileError::Backend)?;

    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, cost);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let charge_blocked = builder.ins().bor(charge_overflow, collection_due);

    let free = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_free),
    )?;
    let free_len = load_heap_value(builder, values.pointer_type, free, OWNED_ARRAY_LEN_OFFSET)?;
    let has_free = builder.ins().icmp_imm(IntCC::NotEqual, free_len, 0);
    let slots_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_slots),
    )?;
    let slots = load_heap_value(builder, values.pointer_type, slots_pointer, 0)?;
    let page_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_page_count),
    )?;
    let page_capacity = builder
        .ins()
        .ishl_imm(page_count, i64::from(JIT_PAGE_SHIFT));
    let has_fresh = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, slots, page_capacity);
    let has_slot = builder.ins().bor(has_free, has_fresh);
    let charge_ready = builder.ins().bxor_imm(charge_blocked, 1);
    let fast = builder.ins().band(has_slot, charge_ready);

    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(slow_block);
    let (slow_status, slow_result) =
        emit_allocation_call(builder, values, class, environment, roots)?;
    builder
        .ins()
        .jump(done, &[slow_status.into(), slow_result.into()]);

    builder.switch_to_block(fast_block);
    let fields_ready = builder.create_block();
    builder.append_block_param(fields_ready, values.pointer_type);
    builder.append_block_param(fields_ready, values.pointer_type);
    builder.append_block_param(fields_ready, values.pointer_type);
    if field_count == 0 {
        let data = builder.ins().iconst(
            values.pointer_type,
            i64::try_from(VALUE_ARRAY_EMPTY_DATA).map_err(|_| CompileError::Backend)?,
        );
        let zero = builder.ins().iconst(values.pointer_type, 0);
        builder
            .ins()
            .jump(fields_ready, &[data.into(), zero.into(), zero.into()]);
    } else {
        let prepare = load_value(
            builder,
            values.pointer_type,
            values.runtime_functions,
            std_mem::offset_of!(RawNativeFunctions, prepare_instance_fields),
        )?;
        let count = builder.ins().iconst(types::I32, i64::from(field_count));
        let call = builder.ins().call_indirect(
            values.instance_fields_signature,
            prepare,
            &[count, values.allocation_result_pointer],
        );
        let status = builder.inst_results(call)[0];
        let data = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            0,
        );
        let len = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            8,
        );
        let capacity = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            16,
        );
        let prepared = builder
            .ins()
            .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
        let failed = builder.create_block();
        builder.ins().brif(
            prepared,
            fields_ready,
            &[data.into(), len.into(), capacity.into()],
            failed,
            &[],
        );
        builder.switch_to_block(failed);
        let zero = builder.ins().iconst(types::I64, 0);
        builder.ins().jump(done, &[status.into(), zero.into()]);
    }

    builder.switch_to_block(fields_ready);
    let fields_data = builder.block_params(fields_ready)[0];
    let fields_len = builder.block_params(fields_ready)[1];
    let fields_capacity = builder.block_params(fields_ready)[2];
    let recycled = builder.create_block();
    let fresh = builder.create_block();
    let slot_ready = builder.create_block();
    builder.append_block_param(slot_ready, types::I32);
    builder.ins().brif(has_free, recycled, &[], fresh, &[]);

    builder.switch_to_block(recycled);
    let free_data = load_heap_value(builder, values.pointer_type, free, OWNED_ARRAY_DATA_OFFSET)?;
    let next_free_len = builder.ins().iadd_imm(free_len, -1);
    let free_offset = builder.ins().imul_imm(next_free_len, 4);
    let free_slot = builder.ins().iadd(free_data, free_offset);
    let recycled_slot = builder
        .ins()
        .load(types::I32, heap_mem_flags(), free_slot, 0);
    store_heap_value(builder, free, OWNED_ARRAY_LEN_OFFSET, next_free_len)?;
    builder.ins().jump(slot_ready, &[recycled_slot.into()]);

    builder.switch_to_block(fresh);
    let fresh_slot = builder.ins().ireduce(types::I32, slots);
    let next_slots = builder.ins().iadd_imm(slots, 1);
    store_heap_value(builder, slots_pointer, 0, next_slots)?;
    let slot_count_offset =
        i32::try_from(std_mem::offset_of!(RawNativeActivation, heap_slot_count))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        next_slots,
        values.activation_pointer,
        slot_count_offset,
    );
    builder.ins().jump(slot_ready, &[fresh_slot.into()]);

    builder.switch_to_block(slot_ready);
    let slot = builder.block_params(slot_ready)[0];
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
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
    let generation = load_heap_value(builder, types::I32, entry, JIT_ENTRY_GENERATION_OFFSET)?;

    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let cost_value = builder.ins().iconst(values.pointer_type, cost);
    let object_tag = builder
        .ins()
        .iconst(types::I32, i64::from(JIT_OBJECT_INSTANCE));
    let class_value = builder.ins().iconst(types::I32, i64::from(class));
    store_heap_value(builder, entry, JIT_ENTRY_FROZEN_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_BYTES_OFFSET, cost_value)?;
    store_heap_value(builder, entry, JIT_ENTRY_SHARED_PRESENT_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_SHARED_KEY_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_OBJECT_TAG_OFFSET, object_tag)?;
    store_heap_value(builder, entry, JIT_INSTANCE_CLASS_OFFSET, class_value)?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
        fields_data,
    )?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
        fields_len,
    )?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
        fields_capacity,
    )?;
    store_heap_value(builder, entry, JIT_INSTANCE_ENV_OFFSET, environment)?;
    let live_tag = builder
        .ins()
        .iconst(types::I32, i64::from(JIT_ENTRY_LIVE_TAG));
    store_heap_value(builder, entry, JIT_ENTRY_LIVE_OFFSET, live_tag)?;
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, next_live)?;
    let allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, inline_allocations),
    )?;
    let allocations = builder.ins().iadd_imm(allocations, 1);
    let allocations_offset =
        i32::try_from(std_mem::offset_of!(RawNativeActivation, inline_allocations))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        allocations,
        values.activation_pointer,
        allocations_offset,
    );

    let generation = builder.ins().uextend(types::I64, generation);
    let generation = builder.ins().ishl_imm(generation, 32);
    let slot = builder.ins().uextend(types::I64, slot);
    let result = builder.ins().bor(generation, slot);
    let status = builder.ins().iconst(types::I32, i64::from(RUNTIME_OK));
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

pub(super) fn emit_allocation_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let class = builder.ins().iconst(types::I32, i64::from(class));
    let allocate_instance = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        std_mem::offset_of!(RawNativeFunctions, allocate_instance),
    )?;
    let no_roots = builder.ins().iconst(types::I32, 0);
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.allocation_signature,
        allocate_instance,
        &[
            values.runtime_context,
            class,
            environment,
            no_collection,
            no_roots,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let retry = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    let collection_required = builder.ins().icmp_imm(
        IntCC::Equal,
        fast_status,
        i64::from(RUNTIME_COLLECTION_REQUIRED),
    );
    builder
        .ins()
        .brif(collection_required, retry, &[], done, &[fast_status.into()]);

    builder.switch_to_block(retry);
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let allow_collection = builder.ins().iconst(types::I32, 1);
    let slow_call = builder.ins().call_indirect(
        values.allocation_signature,
        allocate_instance,
        &[
            values.runtime_context,
            class,
            environment,
            allow_collection,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let slow_status = builder.inst_results(slow_call)[0];
    builder.ins().jump(done, &[slow_status.into()]);

    builder.switch_to_block(done);
    let status = builder.block_params(done)[0];
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    Ok((status, result))
}

pub(super) fn emit_scalar_replacement_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    scalar: &ScalarReplacement,
) -> Result<ir::Value, CompileError> {
    let instance = values
        .scalar_instances
        .get(scalar.site as usize)
        .ok_or(CompileError::Backend)?;
    let active = builder.use_var(instance.active);
    let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let base_frames = load_activation_u32(builder, values, RawActivationField::BaseFrames)?;
    let frames = builder.ins().iadd(base_frames, frame_len);
    let frames = builder
        .ins()
        .iadd_imm(frames, i64::from(scalar.frame_count));
    let max_frames = load_activation_u32(builder, values, RawActivationField::MaxFrames)?;
    let frames_fit = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, frames, max_frames);

    let scalar_len = load_activation_u32(builder, values, RawActivationField::ScalarLen)?;
    let required_values = builder
        .ins()
        .iadd_imm(scalar_len, i64::from(scalar.stack_values));
    let max_values = load_activation_u32(builder, values, RawActivationField::MaxStackValues)?;
    let values_fit =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, required_values, max_values);

    let cost = scalar_instance_cost(scalar.fields.len())?;
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let cost_value = builder.ins().iconst(values.pointer_type, cost);
    let zero = builder.ins().iconst(values.pointer_type, 0);
    let additional = builder.ins().select(active, zero, cost_value);
    let next = builder.ins().iadd(used, additional);
    let no_overflow = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, next, used);
    let threshold = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let heap_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next, threshold);
    let heap_ready = builder.ins().bor(active, heap_fits);
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let record_bit = 1u64.checked_shl(scalar.site).ok_or(CompileError::Backend)?;
    let record = builder.ins().band_imm(available, record_bit as i64);
    let record = builder.ins().icmp_imm(IntCC::NotEqual, record, 0);
    let record_ready = builder.ins().bor(active, record);
    let limits_fit = builder.ins().band(frames_fit, values_fit);
    let heap_ready = builder.ins().band(no_overflow, heap_ready);
    let ready = builder.ins().band(heap_ready, record_ready);
    Ok(builder.ins().band(limits_fit, ready))
}

pub(super) fn emit_scalar_replacement(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    scalar: &ScalarReplacement,
    arguments: &[NativeValue],
    caller_stack: &[NativeValue],
    successor: ir::Block,
) -> Result<(), CompileError> {
    let instance = values
        .scalar_instances
        .get(scalar.site as usize)
        .ok_or(CompileError::Backend)?;
    if instance.fields.len() != scalar.fields.len() {
        return Err(CompileError::Backend);
    }
    let active = builder.use_var(instance.active);
    let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
    let ready = builder.create_block();
    let reserve = builder.create_block();
    builder.ins().brif(active, ready, &[], reserve, &[]);

    builder.switch_to_block(reserve);
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let record_bit = 1u64.checked_shl(scalar.site).ok_or(CompileError::Backend)?;
    let available = builder.ins().band_imm(available, !(record_bit as i64));
    store_i64(
        builder,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_available),
        available,
    )?;
    let cost = scalar_instance_cost(scalar.fields.len())?;
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let used = builder.ins().iadd_imm(used, cost);
    store_heap_value(builder, used_pointer, 0, used)?;
    let live_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, live)?;
    let one = builder.ins().iconst(types::I64, 1);
    builder.def_var(instance.active, one);
    builder.ins().jump(ready, &[]);

    builder.switch_to_block(ready);
    for (target, source) in instance.fields.iter().zip(&scalar.fields) {
        let value = match source {
            ScalarFieldSource::Parameter(parameter) => arguments
                .get(*parameter as usize)
                .copied()
                .ok_or(CompileError::Backend)?,
            ScalarFieldSource::Constant(value) => NativeValue {
                bits: builder.ins().iconst(types::I64, value.bits as i64),
                tag: builder.ins().iconst(types::I64, value.tag as i64),
            },
        };
        builder.def_var(target.bits, value.bits);
        builder.def_var(target.tag, value.tag);
    }
    let count = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, scalar_replaced_allocations),
    )?;
    let count = builder.ins().iadd_imm(count, 1);
    store_i64(
        builder,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, scalar_replaced_allocations),
        count,
    )?;
    emit_charge(
        builder,
        values,
        scalar
            .retired_cost
            .checked_add(1)
            .ok_or(CompileError::Backend)?,
    );
    let mut stack = caller_stack.to_vec();
    stack.push(NativeValue {
        bits: builder.ins().iconst(types::I64, instance.token as i64),
        tag: builder
            .ins()
            .iconst(types::I64, ValueTag::Obj as u64 as i64),
    });
    define_stack(builder, values, &stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

pub(super) fn scalar_instance_cost(field_count: usize) -> Result<i64, CompileError> {
    let cost = field_count
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    i64::try_from(cost).map_err(|_| CompileError::Backend)
}

pub(super) fn emit_scalar_deopt_records(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<(), CompileError> {
    if values.scalar_instances.is_empty() {
        return Ok(());
    }
    let records = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let field_values = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    for (site, (plan, instance)) in values
        .plan
        .scalar_instances
        .iter()
        .zip(values.scalar_instances)
        .enumerate()
    {
        if plan.field_count as usize != instance.fields.len() {
            return Err(CompileError::Backend);
        }
        let active = builder.use_var(instance.active);
        let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
        let write = builder.create_block();
        let next = builder.create_block();
        builder.ins().brif(active, write, &[], next, &[]);

        builder.switch_to_block(write);
        let record_offset = site
            .checked_mul(std_mem::size_of::<RawVirtualInstance>())
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(CompileError::Backend)?;
        let record = builder.ins().iadd_imm(records, record_offset);
        let values_offset = site
            .checked_mul(VIRTUAL_INSTANCE_FIELDS)
            .and_then(|offset| offset.checked_mul(VALUE_SIZE))
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(CompileError::Backend)?;
        let fields = builder.ins().iadd_imm(field_values, values_offset);
        for (field, value) in instance.fields.iter().enumerate() {
            let offset = field.checked_mul(VALUE_SIZE).ok_or(CompileError::Backend)?;
            let bits = builder.use_var(value.bits);
            let tag = builder.use_var(value.tag);
            store_i64(builder, fields, offset + VALUE_PAYLOAD_OFFSET, bits)?;
            store_i64(builder, fields, offset + VALUE_TAG_OFFSET, tag)?;
        }
        let one = builder.ins().iconst(types::I32, 1);
        let zero = builder.ins().iconst(types::I32, 0);
        let token = builder.ins().iconst(types::I64, instance.token as i64);
        let class = builder.ins().iconst(types::I32, i64::from(plan.class));
        let field_count = builder
            .ins()
            .iconst(types::I32, i64::from(plan.field_count));
        let frozen = if plan.frozen { one } else { zero };
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, references),
            one,
        )?;
        store_i64(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, object_bits),
            token,
        )?;
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, class),
            class,
        )?;
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, environment),
            zero,
        )?;
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, field_count),
            field_count,
        )?;
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, frozen),
            frozen,
        )?;
        store_i32_value(
            builder,
            record,
            std_mem::offset_of!(RawVirtualInstance, active),
            one,
        )?;
        builder.ins().jump(next, &[]);
        builder.switch_to_block(next);
    }
    Ok(())
}

pub(super) fn emit_release_scalar_charges(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<(), CompileError> {
    if values.scalar_instances.is_empty() {
        return Ok(());
    }
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let live_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        std_mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    for instance in values.scalar_instances {
        let active = builder.use_var(instance.active);
        let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
        let release = builder.create_block();
        let next = builder.create_block();
        builder.ins().brif(active, release, &[], next, &[]);

        builder.switch_to_block(release);
        let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
        let cost = scalar_instance_cost(instance.fields.len())?;
        let used = builder.ins().iadd_imm(used, -cost);
        store_heap_value(builder, used_pointer, 0, used)?;
        let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
        let live = builder.ins().iadd_imm(live, -1);
        store_heap_value(builder, live_pointer, 0, live)?;
        builder.ins().jump(next, &[]);
        builder.switch_to_block(next);
    }
    Ok(())
}
