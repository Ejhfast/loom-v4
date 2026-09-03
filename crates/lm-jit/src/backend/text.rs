//! Text and byte access emission.

use super::*;

pub(super) fn emit_bytes_len(
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
        JIT_OBJECT_BYTES,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let len = load_value(builder, values.pointer_type, entry, JIT_BYTES_LEN_OFFSET)?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

pub(super) fn emit_text_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    offset: usize,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_text_entry(
        builder,
        values,
        reference,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?
    .payload;
    let len = load_value(builder, values.pointer_type, entry, offset)?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

pub(super) fn emit_text_at_byte(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_text_entry(
        builder,
        values,
        reference,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?
    .payload;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_BYTE_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_DATA_OFFSET,
    )?;
    let address = builder.ins().iadd(data, native_index);
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let prefix = builder.ins().band_imm(first, 0xc0);
    let continuation = builder.ins().icmp_imm(IntCC::Equal, prefix, 0x80);
    emit_interpreter_replay(builder, values, continuation, point, deopt_stack)?;

    emit_utf8_at_address(builder, address)
}

pub(super) fn emit_text_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_text_entry(
        builder,
        values,
        reference,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?
    .payload;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_SCALAR_LEN_OFFSET,
    )?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, scalar_len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;

    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_DATA_OFFSET,
    )?;
    let byte_len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_BYTE_LEN_OFFSET,
    )?;
    let ascii = builder.ins().icmp(IntCC::Equal, byte_len, scalar_len);
    let ascii_block = builder.create_block();
    let scan = builder.create_block();
    let advance = builder.create_block();
    let found = builder.create_block();
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.ins().brif(
        ascii,
        ascii_block,
        &[],
        scan,
        &[native_index.into(), data.into()],
    );

    builder.switch_to_block(ascii_block);
    let address = builder.ins().iadd(data, native_index);
    builder.ins().jump(found, &[address.into()]);

    builder.switch_to_block(scan);
    let remaining = builder.block_params(scan)[0];
    let address = builder.block_params(scan)[1];
    let at_target = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    builder
        .ins()
        .brif(at_target, found, &[address.into()], advance, &[]);

    builder.switch_to_block(advance);
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let one = builder.ins().iconst(values.pointer_type, 1);
    let two = builder.ins().iconst(values.pointer_type, 2);
    let three = builder.ins().iconst(values.pointer_type, 3);
    let four = builder.ins().iconst(values.pointer_type, 4);
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0x80);
    let is_two = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xe0);
    let is_three = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xf0);
    let non_ascii_width = builder.ins().select(is_three, three, four);
    let multibyte_width = builder.ins().select(is_two, two, non_ascii_width);
    let width = builder.ins().select(is_ascii, one, multibyte_width);
    let next_address = builder.ins().iadd(address, width);
    let next_remaining = builder.ins().iadd_imm(remaining, -1);
    builder
        .ins()
        .jump(scan, &[next_remaining.into(), next_address.into()]);

    builder.switch_to_block(found);
    let address = builder.block_params(found)[0];
    emit_utf8_at_address(builder, address)
}

pub(super) fn emit_utf8_at_address(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
) -> Result<ir::Value, CompileError> {
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);

    let ascii = builder.create_block();
    let two = builder.create_block();
    let three = builder.create_block();
    let four = builder.create_block();
    let after_ascii = builder.create_block();
    let after_two = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0x80);
    builder.ins().brif(is_ascii, ascii, &[], after_ascii, &[]);

    builder.switch_to_block(ascii);
    let scalar = builder.ins().uextend(types::I64, first);
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(after_ascii);
    let is_two = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xe0);
    builder.ins().brif(is_two, two, &[], after_two, &[]);

    builder.switch_to_block(two);
    let scalar = emit_utf8_scalar(builder, address, first, 2)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(after_two);
    let is_three = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xf0);
    builder.ins().brif(is_three, three, &[], four, &[]);

    builder.switch_to_block(three);
    let scalar = emit_utf8_scalar(builder, address, first, 3)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(four);
    let scalar = emit_utf8_scalar(builder, address, first, 4)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_utf8_scalar(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
    first: ir::Value,
    length: u8,
) -> Result<ir::Value, CompileError> {
    let lead_mask = match length {
        2 => 0x1f,
        3 => 0x0f,
        4 => 0x07,
        _ => return Err(CompileError::Backend),
    };
    let first = builder.ins().uextend(types::I64, first);
    let mut scalar = builder.ins().band_imm(first, lead_mask);
    for offset in 1..length {
        let byte = builder
            .ins()
            .load(types::I8, MemFlags::trusted(), address, i32::from(offset));
        let byte = builder.ins().uextend(types::I64, byte);
        let byte = builder.ins().band_imm(byte, 0x3f);
        scalar = builder.ins().ishl_imm(scalar, 6);
        scalar = builder.ins().bor(scalar, byte);
    }
    Ok(scalar)
}

pub(super) fn emit_text_is_boundary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_text_entry(
        builder,
        values,
        reference,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?
    .payload;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    emit_interpreter_replay(builder, values, negative, point, deopt_stack)?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_BYTE_LEN_OFFSET,
    )?;
    let inside = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, native_index, len);
    let inspect = builder.create_block();
    let outside = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(inside, inspect, &[], outside, &[]);

    builder.switch_to_block(inspect);
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_PAYLOAD_DATA_OFFSET,
    )?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let prefix = builder.ins().band_imm(byte, 0xc0);
    let continuation = builder.ins().icmp_imm(IntCC::Equal, prefix, 0x80);
    let boundary = builder.ins().bxor_imm(continuation, 1);
    let boundary = builder.ins().uextend(types::I64, boundary);
    builder.ins().jump(done, &[boundary.into()]);

    builder.switch_to_block(outside);
    let boundary = builder.ins().icmp(IntCC::Equal, native_index, len);
    let boundary = builder.ins().uextend(types::I64, boundary);
    builder.ins().jump(done, &[boundary.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_bytes_at(
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
        JIT_OBJECT_BYTES,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(builder, values.pointer_type, entry, JIT_BYTES_LEN_OFFSET)?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let data = load_value(builder, values.pointer_type, entry, JIT_BYTES_DATA_OFFSET)?;
    let address = builder.ins().iadd(data, index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    Ok(builder.ins().uextend(types::I64, byte))
}

pub(super) fn emit_bytes_get(
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
        JIT_OBJECT_BYTES,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(builder, values.pointer_type, entry, JIT_BYTES_LEN_OFFSET)?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, len);
    let missing = builder.ins().bor(negative, outside);
    let found_block = builder.create_block();
    let missing_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder
        .ins()
        .brif(missing, missing_block, &[], found_block, &[]);

    builder.switch_to_block(found_block);
    let data = load_value(builder, values.pointer_type, entry, JIT_BYTES_DATA_OFFSET)?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let byte = builder.ins().uextend(types::I64, byte);
    builder.ins().jump(done, &[byte.into()]);

    builder.switch_to_block(missing_block);
    let minus_one = builder.ins().iconst(types::I64, -1);
    builder.ins().jump(done, &[minus_one.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

pub(super) fn emit_bytes_read_u32(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    offset: ir::Value,
    big_endian: bool,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_BYTES,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, offset, 0);
    let offset = if values.pointer_type == types::I64 {
        offset
    } else {
        builder.ins().ireduce(values.pointer_type, offset)
    };
    let len = load_value(builder, values.pointer_type, entry, JIT_BYTES_LEN_OFFSET)?;
    let too_short = builder.ins().icmp_imm(IntCC::UnsignedLessThan, len, 4);
    let four = builder.ins().iconst(values.pointer_type, 4);
    let last = builder.ins().isub(len, four);
    let outside = builder.ins().icmp(IntCC::UnsignedGreaterThan, offset, last);
    let invalid = builder.ins().bor(negative, too_short);
    let invalid = builder.ins().bor(invalid, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;

    let data = load_value(builder, values.pointer_type, entry, JIT_BYTES_DATA_OFFSET)?;
    let address = builder.ins().iadd(data, offset);
    let flags = MemFlags::new().with_notrap();
    let word = builder.ins().load(types::I32, flags, address, 0);
    let word = builder.ins().uextend(types::I64, word);
    let reverse = big_endian == cfg!(target_endian = "little");
    if !reverse {
        return Ok(word);
    }
    let byte_0 = builder.ins().band_imm(word, 0xff);
    let byte_0 = builder.ins().ishl_imm(byte_0, 24);
    let byte_1 = builder.ins().band_imm(word, 0xff00);
    let byte_1 = builder.ins().ishl_imm(byte_1, 8);
    let byte_2 = builder.ins().ushr_imm(word, 8);
    let byte_2 = builder.ins().band_imm(byte_2, 0xff00);
    let byte_3 = builder.ins().ushr_imm(word, 24);
    let high = builder.ins().bor(byte_0, byte_1);
    let low = builder.ins().bor(byte_2, byte_3);
    Ok(builder.ins().bor(high, low))
}
