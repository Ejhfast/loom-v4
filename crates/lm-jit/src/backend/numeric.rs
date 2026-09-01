//! Numeric and character instruction emission.

use super::*;

pub(super) fn emit_numeric_instruction(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &mut Vec<NativeValue>,
    operation: NumericInstr,
    exit: NumericExitEmission<'_>,
) -> Result<(), CompileError> {
    match operation {
        NumericInstr::IntBitAnd
        | NumericInstr::IntBitOr
        | NumericInstr::IntBitXor
        | NumericInstr::IntWrappingAdd
        | NumericInstr::IntWrappingSub
        | NumericInstr::IntWrappingMul => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
            let value = match operation {
                NumericInstr::IntBitAnd => builder.ins().band(left, right),
                NumericInstr::IntBitOr => builder.ins().bor(left, right),
                NumericInstr::IntBitXor => builder.ins().bxor(left, right),
                NumericInstr::IntWrappingAdd => builder.ins().iadd(left, right),
                NumericInstr::IntWrappingSub => builder.ins().isub(left, right),
                NumericInstr::IntWrappingMul => builder.ins().imul(left, right),
                _ => unreachable!(),
            };
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NumericInstr::IntBitNot => {
            let value = pop_native(stack)?;
            let value = builder.ins().bnot(value);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NumericInstr::IntShl
        | NumericInstr::IntShr
        | NumericInstr::IntUshr
        | NumericInstr::IntRotateLeft
        | NumericInstr::IntRotateRight => {
            let amount = pop_native(stack)?;
            let value = pop_native(stack)?;
            let invalid = builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, amount, 63);
            emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
            let value = match operation {
                NumericInstr::IntShl => builder.ins().ishl(value, amount),
                NumericInstr::IntShr => builder.ins().sshr(value, amount),
                NumericInstr::IntUshr => builder.ins().ushr(value, amount),
                NumericInstr::IntRotateLeft => builder.ins().rotl(value, amount),
                NumericInstr::IntRotateRight => builder.ins().rotr(value, amount),
                _ => unreachable!(),
            };
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NumericInstr::IntToFloat => {
            let value = pop_native(stack)?;
            let value = builder.ins().fcvt_from_sint(types::F64, value);
            let value = canonical_float(builder, value);
            push_static(builder, stack, ScalarKind::Float, value)?;
        }
        NumericInstr::FloatNeg => {
            let value = float_value(builder, pop_native(stack)?);
            let value = builder.ins().fneg(value);
            let value = canonical_float(builder, value);
            push_static(builder, stack, ScalarKind::Float, value)?;
        }
        NumericInstr::FloatAdd
        | NumericInstr::FloatSub
        | NumericInstr::FloatMul
        | NumericInstr::FloatDiv => {
            let right_bits = pop_native(stack)?;
            let left_bits = pop_native(stack)?;
            let right = float_value(builder, right_bits);
            let left = float_value(builder, left_bits);
            let value = match operation {
                NumericInstr::FloatAdd => builder.ins().fadd(left, right),
                NumericInstr::FloatSub => builder.ins().fsub(left, right),
                NumericInstr::FloatMul => builder.ins().fmul(left, right),
                NumericInstr::FloatDiv => builder.ins().fdiv(left, right),
                _ => unreachable!(),
            };
            let value = canonical_float(builder, value);
            push_static(builder, stack, ScalarKind::Float, value)?;
        }
        NumericInstr::FloatEq
        | NumericInstr::FloatNe
        | NumericInstr::FloatLt
        | NumericInstr::FloatLe
        | NumericInstr::FloatGt
        | NumericInstr::FloatGe => {
            let right_bits = pop_native(stack)?;
            let left_bits = pop_native(stack)?;
            let right = float_value(builder, right_bits);
            let left = float_value(builder, left_bits);
            let compared = match operation {
                NumericInstr::FloatEq | NumericInstr::FloatNe => {
                    let equal = builder.ins().fcmp(FloatCC::Equal, left, right);
                    let left_nan = builder.ins().fcmp(FloatCC::Unordered, left, left);
                    let right_nan = builder.ins().fcmp(FloatCC::Unordered, right, right);
                    let both_nan = builder.ins().band(left_nan, right_nan);
                    let equal = builder.ins().bor(equal, both_nan);
                    if matches!(operation, NumericInstr::FloatNe) {
                        builder.ins().bxor_imm(equal, 1)
                    } else {
                        equal
                    }
                }
                NumericInstr::FloatLt => builder.ins().fcmp(FloatCC::LessThan, left, right),
                NumericInstr::FloatLe => builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right),
                NumericInstr::FloatGt => builder.ins().fcmp(FloatCC::GreaterThan, left, right),
                NumericInstr::FloatGe => {
                    builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right)
                }
                _ => unreachable!(),
            };
            let value = builder.ins().uextend(types::I64, compared);
            push_static(builder, stack, ScalarKind::Bool, value)?;
        }
        NumericInstr::FloatIsNan => {
            let value = float_value(builder, pop_native(stack)?);
            let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
            let value = builder.ins().uextend(types::I64, is_nan);
            push_static(builder, stack, ScalarKind::Bool, value)?;
        }
        NumericInstr::FloatHash => {
            let bits = pop_native(stack)?;
            let shifted = builder.ins().ishl_imm(bits, 1);
            let is_zero = builder.ins().icmp_imm(IntCC::Equal, shifted, 0);
            let zero = builder.ins().iconst(types::I64, 0);
            let value = builder.ins().select(is_zero, zero, bits);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NumericInstr::FloatBits => {
            let bits = pop_native(stack)?;
            push_static(builder, stack, ScalarKind::Int, bits)?;
        }
        NumericInstr::FloatFromBits => {
            let bits = pop_native(stack)?;
            let value = float_value(builder, bits);
            let value = canonical_float(builder, value);
            push_static(builder, stack, ScalarKind::Float, value)?;
        }
        NumericInstr::FloatToIntStatus => {
            let bits = pop_native(stack)?;
            let value = float_value(builder, bits);
            let finite = float_is_finite(builder, bits);
            let fits = float_fits_int(builder, value);
            let zero = builder.ins().iconst(types::I64, 0);
            let one = builder.ins().iconst(types::I64, 1);
            let two = builder.ins().iconst(types::I64, 2);
            let range_status = builder.ins().select(fits, zero, two);
            let value = builder.ins().select(finite, range_status, one);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NumericInstr::FloatToIntValue => {
            let bits = pop_native(stack)?;
            let value = float_value(builder, bits);
            let finite = float_is_finite(builder, bits);
            let fits = float_fits_int(builder, value);
            let valid = builder.ins().band(finite, fits);
            let invalid = builder.ins().bxor_imm(valid, 1);
            emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
            let value = builder.ins().fcvt_to_sint(types::I64, value);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        _ => {
            return Err(CompileError::Unsupported(
                UnsupportedReason::UnsupportedInstruction,
            ))
        }
    }
    Ok(())
}

pub(super) fn float_is_finite(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    let exponent = builder.ins().band_imm(bits, 0x7ff0_0000_0000_0000);
    builder
        .ins()
        .icmp_imm(IntCC::NotEqual, exponent, 0x7ff0_0000_0000_0000)
}

pub(super) fn float_fits_int(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let minimum_bits = builder
        .ins()
        .iconst(types::I64, (i64::MIN as f64).to_bits() as i64);
    let maximum_bits = builder
        .ins()
        .iconst(types::I64, 9_223_372_036_854_775_808.0_f64.to_bits() as i64);
    let minimum = float_value(builder, minimum_bits);
    let maximum = float_value(builder, maximum_bits);
    let at_least_minimum = builder
        .ins()
        .fcmp(FloatCC::GreaterThanOrEqual, value, minimum);
    let below_maximum = builder.ins().fcmp(FloatCC::LessThan, value, maximum);
    builder.ins().band(at_least_minimum, below_maximum)
}

pub(super) fn emit_char_instruction(
    builder: &mut FunctionBuilder<'_>,
    stack: &mut Vec<NativeValue>,
    operation: NativeInstr,
) -> Result<(), CompileError> {
    match operation {
        NativeInstr::CharCodepoint => {
            let value = pop_native(stack)?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NativeInstr::CharUtf8Len => {
            let value = pop_native(stack)?;
            let one = builder.ins().iconst(types::I64, 1);
            let two = builder.ins().iconst(types::I64, 2);
            let three = builder.ins().iconst(types::I64, 3);
            let four = builder.ins().iconst(types::I64, 4);
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
            let value = builder.ins().select(over_three, four, medium);
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        NativeInstr::EqChar
        | NativeInstr::NeChar
        | NativeInstr::LtChar
        | NativeInstr::LeChar
        | NativeInstr::GtChar
        | NativeInstr::GeChar => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
            let condition = match operation {
                NativeInstr::EqChar => IntCC::Equal,
                NativeInstr::NeChar => IntCC::NotEqual,
                NativeInstr::LtChar => IntCC::UnsignedLessThan,
                NativeInstr::LeChar => IntCC::UnsignedLessThanOrEqual,
                NativeInstr::GtChar => IntCC::UnsignedGreaterThan,
                NativeInstr::GeChar => IntCC::UnsignedGreaterThanOrEqual,
                _ => unreachable!(),
            };
            let compared = builder.ins().icmp(condition, left, right);
            let value = builder.ins().uextend(types::I64, compared);
            push_static(builder, stack, ScalarKind::Bool, value)?;
        }
        _ => {
            return Err(CompileError::Unsupported(
                UnsupportedReason::UnsupportedInstruction,
            ))
        }
    }
    Ok(())
}

pub(super) fn float_value(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    builder.ins().bitcast(types::F64, MemFlags::new(), bits)
}

pub(super) fn canonical_float(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
    let canonical = builder.ins().iconst(types::I64, CANONICAL_NAN_BITS as i64);
    builder.ins().select(is_nan, canonical, bits)
}

pub(super) fn emit_stable_hash_mix(
    builder: &mut FunctionBuilder<'_>,
    value: ir::Value,
) -> ir::Value {
    let shifted = builder.ins().ushr_imm(value, 30);
    let value = builder.ins().bxor(value, shifted);
    let value = builder
        .ins()
        .imul_imm(value, 0xbf58_476d_1ce4_e5b9_u64 as i64);
    let shifted = builder.ins().ushr_imm(value, 27);
    let value = builder.ins().bxor(value, shifted);
    let value = builder
        .ins()
        .imul_imm(value, 0x94d0_49bb_1331_11eb_u64 as i64);
    let shifted = builder.ins().ushr_imm(value, 31);
    builder.ins().bxor(value, shifted)
}
