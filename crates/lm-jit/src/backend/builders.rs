//! String and byte builder emission.

use super::*;

pub(super) fn emit_string_builder_append_text(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    source: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_entry = emit_text_entry(
        builder,
        values,
        source,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?
    .payload;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_len = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_PAYLOAD_BYTE_LEN_OFFSET,
    )?;
    let next_len = builder.ins().iadd(target_len, source_len);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let copy_block = builder.create_block();
    let copied_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let nonempty = builder.ins().icmp_imm(IntCC::NotEqual, source_len, 0);
    builder
        .ins()
        .brif(nonempty, copy_block, &[], copied_block, &[]);

    builder.switch_to_block(copy_block);
    let target_data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(target_data, target_len);
    let source_data = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_PAYLOAD_DATA_OFFSET,
    )?;
    builder.call_memmove(values.frontend_config, destination, source_data, source_len);
    builder.ins().jump(copied_block, &[]);

    builder.switch_to_block(copied_block);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let target_scalars = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let source_scalars = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_PAYLOAD_SCALAR_LEN_OFFSET,
    )?;
    let next_scalars = builder.ins().iadd(target_scalars, source_scalars);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        next_scalars,
    )?;
    let target_ascii = load_value(
        builder,
        types::I8,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
    )?;
    let source_ascii = builder.ins().icmp(IntCC::Equal, source_len, source_scalars);
    let next_ascii = builder.ins().band(target_ascii, source_ascii);
    store_i8_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
        next_ascii,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, string_builder_append_text),
        [target, source, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_string_builder_append_bool(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let truth = builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
    let true_len = builder.ins().iconst(values.pointer_type, 4);
    let false_len = builder.ins().iconst(values.pointer_type, 5);
    let added = builder.ins().select(truth, true_len, false_len);
    let next_len = builder.ins().iadd(target_len, added);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let true_block = builder.create_block();
    let false_block = builder.create_block();
    let written_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, target_len);
    builder.ins().brif(truth, true_block, &[], false_block, &[]);

    builder.switch_to_block(true_block);
    for (offset, byte) in b"true".iter().copied().enumerate() {
        let value = builder.ins().iconst(types::I8, i64::from(byte));
        store_i8_value(builder, destination, offset, value)?;
    }
    builder.ins().jump(written_block, &[]);

    builder.switch_to_block(false_block);
    for (offset, byte) in b"false".iter().copied().enumerate() {
        let value = builder.ins().iconst(types::I8, i64::from(byte));
        store_i8_value(builder, destination, offset, value)?;
    }
    builder.ins().jump(written_block, &[]);

    builder.switch_to_block(written_block);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd(scalar_len, added);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, string_builder_append_bool),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_string_builder_append_int(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;

    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, value, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let negated = builder.ins().isub(zero_i64, value);
    let magnitude = builder.ins().select(negative, negated, value);
    let count_digits = builder.create_block();
    let count_more = builder.create_block();
    let count_done = builder.create_block();
    builder.append_block_param(count_digits, types::I64);
    builder.append_block_param(count_digits, types::I64);
    builder.append_block_param(count_done, types::I64);
    let one_i64 = builder.ins().iconst(types::I64, 1);
    builder
        .ins()
        .jump(count_digits, &[magnitude.into(), one_i64.into()]);

    builder.switch_to_block(count_digits);
    let remaining = builder.block_params(count_digits)[0];
    let digits = builder.block_params(count_digits)[1];
    let has_more = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, remaining, 10);
    builder
        .ins()
        .brif(has_more, count_more, &[], count_done, &[digits.into()]);

    builder.switch_to_block(count_more);
    let remaining = builder.ins().udiv_imm(remaining, 10);
    let digits = builder.ins().iadd_imm(digits, 1);
    builder
        .ins()
        .jump(count_digits, &[remaining.into(), digits.into()]);

    builder.switch_to_block(count_done);
    let digits = builder.block_params(count_done)[0];
    let sign = builder.ins().uextend(types::I64, negative);
    let added_i64 = builder.ins().iadd(digits, sign);
    let added = if values.pointer_type == types::I64 {
        added_i64
    } else {
        builder.ins().ireduce(values.pointer_type, added_i64)
    };
    let next_len = builder.ins().iadd(target_len, added);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let sign_block = builder.create_block();
    let digits_block = builder.create_block();
    let digit_loop = builder.create_block();
    let written = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(digit_loop, types::I64);
    builder.append_block_param(digit_loop, values.pointer_type);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let cursor = builder.ins().iadd(data, next_len);
    builder
        .ins()
        .brif(negative, sign_block, &[], digits_block, &[]);

    builder.switch_to_block(sign_block);
    let sign_address = builder.ins().iadd(data, target_len);
    let minus = builder.ins().iconst(types::I8, i64::from(b'-'));
    store_i8_value(builder, sign_address, 0, minus)?;
    builder.ins().jump(digits_block, &[]);

    builder.switch_to_block(digits_block);
    builder
        .ins()
        .jump(digit_loop, &[magnitude.into(), cursor.into()]);

    builder.switch_to_block(digit_loop);
    let remaining = builder.block_params(digit_loop)[0];
    let cursor = builder.block_params(digit_loop)[1];
    let quotient = builder.ins().udiv_imm(remaining, 10);
    let digit = builder.ins().urem_imm(remaining, 10);
    let digit = builder.ins().iadd_imm(digit, i64::from(b'0'));
    let digit = builder.ins().ireduce(types::I8, digit);
    let cursor = builder.ins().iadd_imm(cursor, -1);
    store_i8_value(builder, cursor, 0, digit)?;
    let has_more = builder.ins().icmp_imm(IntCC::NotEqual, quotient, 0);
    builder.ins().brif(
        has_more,
        digit_loop,
        &[quotient.into(), cursor.into()],
        written,
        &[],
    );

    builder.switch_to_block(written);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd(scalar_len, added);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, string_builder_append_int),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_string_builder_append_char(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let above_unicode = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0x10ffff);
    let in_surrogate_tail =
        builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, value, 0xd800);
    let in_surrogate_head = builder
        .ins()
        .icmp_imm(IntCC::UnsignedLessThanOrEqual, value, 0xdfff);
    let surrogate = builder.ins().band(in_surrogate_tail, in_surrogate_head);
    let invalid = builder.ins().bor(above_unicode, surrogate);
    emit_fault_check(
        builder,
        values,
        invalid,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    let two = builder.ins().iconst(values.pointer_type, 2);
    let three = builder.ins().iconst(values.pointer_type, 3);
    let four = builder.ins().iconst(values.pointer_type, 4);
    let over_one = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0x7f);
    let over_two = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0x7ff);
    let over_three = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0xffff);
    let short = builder.ins().select(over_one, two, one);
    let medium = builder.ins().select(over_two, three, short);
    let width = builder.ins().select(over_three, four, medium);
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let next_len = builder.ins().iadd(target_len, width);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let one_block = builder.create_block();
    let after_one = builder.create_block();
    let two_block = builder.create_block();
    let after_two = builder.create_block();
    let three_block = builder.create_block();
    let four_block = builder.create_block();
    let written = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, target_len);
    let is_one = builder.ins().icmp_imm(IntCC::Equal, width, 1);
    builder.ins().brif(is_one, one_block, &[], after_one, &[]);

    builder.switch_to_block(one_block);
    let byte = builder.ins().ireduce(types::I8, value);
    store_i8_value(builder, destination, 0, byte)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(after_one);
    let is_two = builder.ins().icmp_imm(IntCC::Equal, width, 2);
    builder.ins().brif(is_two, two_block, &[], after_two, &[]);

    builder.switch_to_block(two_block);
    let first = builder.ins().ushr_imm(value, 6);
    let first = builder.ins().bor_imm(first, 0xc0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().band_imm(value, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(after_two);
    let is_three = builder.ins().icmp_imm(IntCC::Equal, width, 3);
    builder
        .ins()
        .brif(is_three, three_block, &[], four_block, &[]);

    builder.switch_to_block(three_block);
    let first = builder.ins().ushr_imm(value, 12);
    let first = builder.ins().bor_imm(first, 0xe0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().ushr_imm(value, 6);
    let second = builder.ins().band_imm(second, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    let third = builder.ins().band_imm(value, 0x3f);
    let third = builder.ins().bor_imm(third, 0x80);
    let third = builder.ins().ireduce(types::I8, third);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    store_i8_value(builder, destination, 2, third)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(four_block);
    let first = builder.ins().ushr_imm(value, 18);
    let first = builder.ins().bor_imm(first, 0xf0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().ushr_imm(value, 12);
    let second = builder.ins().band_imm(second, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    let third = builder.ins().ushr_imm(value, 6);
    let third = builder.ins().band_imm(third, 0x3f);
    let third = builder.ins().bor_imm(third, 0x80);
    let third = builder.ins().ireduce(types::I8, third);
    let fourth = builder.ins().band_imm(value, 0x3f);
    let fourth = builder.ins().bor_imm(fourth, 0x80);
    let fourth = builder.ins().ireduce(types::I8, fourth);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    store_i8_value(builder, destination, 2, third)?;
    store_i8_value(builder, destination, 3, fourth)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(written);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd_imm(scalar_len, 1);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    let target_ascii = load_value(
        builder,
        types::I8,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
    )?;
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, value, 0x80);
    let ascii = builder.ins().band(target_ascii, is_ascii);
    store_i8_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
        ascii,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, string_builder_append_char),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_byte_buffer_append(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, value, 0);
    let too_large = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, i64::from(u8::MAX));
    let invalid = builder.ins().bor(negative, too_large);
    emit_fault_check(
        builder,
        values,
        invalid,
        EXIT_INTEGER_OVERFLOW,
        exit.point,
        exit.fault_stack,
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let fast = builder.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, len);
    let byte = builder.ins().ireduce(types::I8, value);
    builder
        .ins()
        .store(MemFlags::trusted(), byte, destination, 0);
    let next_len = builder.ins().iadd_imm(len, 1);
    store_native_value(builder, target_entry, JIT_BYTE_BUFFER_LEN_OFFSET, next_len)?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, byte_buffer_append),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_byte_buffer_extend(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    source: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_entry = emit_object_entry(
        builder,
        values,
        source,
        JIT_OBJECT_BYTES,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_len = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_BYTES_LEN_OFFSET,
    )?;
    let next_len = builder.ins().iadd(target_len, source_len);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let copy_block = builder.create_block();
    let copied_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let nonempty = builder.ins().icmp_imm(IntCC::NotEqual, source_len, 0);
    builder
        .ins()
        .brif(nonempty, copy_block, &[], copied_block, &[]);

    builder.switch_to_block(copy_block);
    let target_data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(target_data, target_len);
    let source_data = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_BYTES_DATA_OFFSET,
    )?;
    builder.call_memmove(values.frontend_config, destination, source_data, source_len);
    builder.ins().jump(copied_block, &[]);

    builder.switch_to_block(copied_block);
    store_native_value(builder, target_entry, JIT_BYTE_BUFFER_LEN_OFFSET, next_len)?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, byte_buffer_extend),
        [target, source, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_byte_buffer_reserve(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let runtime_additional = additional;
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, additional, 0);
    emit_fault_check(
        builder,
        values,
        negative,
        EXIT_INTEGER_OVERFLOW,
        exit.point,
        exit.fault_stack,
    )?;
    let additional = if values.pointer_type == types::I64 {
        additional
    } else {
        let too_large =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, additional, i64::from(u32::MAX));
        emit_fault_check(
            builder,
            values,
            too_large,
            EXIT_INTEGER_OVERFLOW,
            exit.point,
            exit.fault_stack,
        )?;
        builder.ins().ireduce(values.pointer_type, additional)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder.ins().icmp(IntCC::UnsignedLessThan, capacity, len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let spare = builder.ins().isub(capacity, len);
    let fast = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, additional, spare);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        std_mem::offset_of!(RawNativeFunctions, byte_buffer_reserve),
        [target, runtime_additional, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_builder_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    object_tag: u32,
    offsets: (usize, usize),
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let (active_offset, length_offset) = offsets;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        object_tag,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    emit_active_guard(builder, values, entry, active_offset, point, deopt_stack)?;
    let length = load_value(builder, values.pointer_type, entry, length_offset)?;
    Ok(if values.pointer_type == types::I64 {
        length
    } else {
        builder.ins().uextend(types::I64, length)
    })
}

pub(super) fn emit_builder_clear(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    string_builder: bool,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let (object_tag, active_offset, length_offset) = if string_builder {
        (
            JIT_OBJECT_STRING_BUILDER,
            JIT_STRING_BUILDER_ACTIVE_OFFSET,
            JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        )
    } else {
        (
            JIT_OBJECT_BYTE_BUFFER,
            JIT_BYTE_BUFFER_ACTIVE_OFFSET,
            JIT_BYTE_BUFFER_LEN_OFFSET,
        )
    };
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        object_tag,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    emit_active_guard(
        builder,
        values,
        entry,
        active_offset,
        exit.point,
        exit.deopt_stack,
    )?;
    let zero = builder.ins().iconst(values.pointer_type, 0);
    store_native_value(builder, entry, length_offset, zero)?;
    if string_builder {
        store_native_value(builder, entry, JIT_STRING_BUILDER_SCALAR_LEN_OFFSET, zero)?;
        let ascii = builder.ins().iconst(types::I8, 1);
        store_i8_value(builder, entry, JIT_STRING_BUILDER_ASCII_OFFSET, ascii)?;
    }
    Ok(())
}

pub(super) fn emit_byte_buffer_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_BYTE_BUFFER,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    emit_active_guard(
        builder,
        values,
        entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        point,
        deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let length = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, length);
    let missing = builder.ins().bor(negative, outside);
    let load = builder.create_block();
    let absent = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(missing, absent, &[], load, &[]);

    builder.switch_to_block(load);
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let byte = builder.ins().uextend(types::I64, byte);
    builder.ins().jump(done, &[byte.into()]);

    builder.switch_to_block(absent);
    let missing = builder.ins().iconst(types::I64, -1);
    builder.ins().jump(done, &[missing.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_byte_buffer_set(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    value: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    emit_active_guard(
        builder,
        values,
        entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative_byte = builder.ins().icmp_imm(IntCC::SignedLessThan, value, 0);
    let large_byte = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, i64::from(u8::MAX));
    let invalid_byte = builder.ins().bor(negative_byte, large_byte);
    emit_fault_check(
        builder,
        values,
        invalid_byte,
        EXIT_INTEGER_OVERFLOW,
        exit.point,
        exit.fault_stack,
    )?;
    let negative_index = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = native_size(builder, values, index, exit)?;
    let length = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, length);
    let invalid_index = builder.ins().bor(negative_index, outside);
    emit_interpreter_replay(builder, values, invalid_index, exit.point, exit.deopt_stack)?;
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder.ins().ireduce(types::I8, value);
    builder.ins().store(MemFlags::trusted(), byte, address, 0);
    Ok(())
}

pub(super) fn emit_byte_buffer_truncate(
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
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    emit_active_guard(
        builder,
        values,
        entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, length, 0);
    emit_interpreter_replay(builder, values, negative, exit.point, exit.deopt_stack)?;
    let length = native_size(builder, values, length, exit)?;
    let current = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let changed = builder.ins().icmp(IntCC::UnsignedLessThan, length, current);
    let update = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(changed, update, &[], done, &[]);

    builder.switch_to_block(update);
    store_native_value(builder, entry, JIT_BYTE_BUFFER_LEN_OFFSET, length)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}
