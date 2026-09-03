//! Instruction emission for one dispatch domain.

use super::*;

pub(super) fn emit(emission: &mut InstructionEmission<'_, '_, '_, '_>) -> Result<(), CompileError> {
    let instruction = emission.instruction;
    let builder = &mut *emission.builder;
    let values = emission.values;
    let plan = emission.plan;
    let segment = emission.segment;
    let stack = &mut *emission.stack;
    let virtual_stack = &mut *emission.virtual_stack;
    let deferred_integer_overflow = &mut *emission.deferred_integer_overflow;
    let within = emission.within;
    let prefix = emission.prefix;
    let fault_prefix = emission.fault_prefix;
    match instruction {
        Instr::Add | Instr::Sub | Instr::Mul => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
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
                    stack,
                )?
            };
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::Div | Instr::Rem => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
            let point = FaultPoint {
                block: segment.block,
                instruction: segment.start + prefix,
                prefix: fault_prefix,
            };
            let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
            emit_fault_check(builder, values, zero, EXIT_DIVIDE_BY_ZERO, point, stack)?;
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
                stack,
            )?;
            let result = if matches!(instruction, Instr::Div) {
                builder.ins().sdiv(left, right)
            } else {
                builder.ins().srem(left, right)
            };
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::Neg => {
            let value = pop_native(stack)?;
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
                    stack,
                )?
            };
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::Not => {
            let value = pop_native(stack)?;
            let result = builder.ins().bxor_imm(value, 1);
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
            let value = pop_native(stack)?;
            let seed = pop_native(stack)?;
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
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::LtInt | Instr::LeInt | Instr::GtInt | Instr::GeInt | Instr::EqInt | Instr::NeInt => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
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
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::EqBool | Instr::NeBool => {
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
            let condition = if matches!(instruction, Instr::EqBool) {
                IntCC::Equal
            } else {
                IntCC::NotEqual
            };
            let compared = builder.ins().icmp(condition, left, right);
            let result = builder.ins().uextend(types::I64, compared);
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::EqValue | Instr::NeValue => {
            let deopt_stack = stack.clone();
            let right = pop_value(stack)?;
            let left = pop_value(stack)?;
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
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            let result = if matches!(instruction, Instr::EqValue) {
                equal
            } else {
                builder.ins().bxor_imm(equal, 1)
            };
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::Freeze => {
            let deopt_stack = stack.clone();
            let value = pop_value(stack)?;
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
                    fault_stack: stack,
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
                collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?;
            let reference = pop_native(stack)?;
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
            push_static(builder, stack, ScalarKind::Object(0), result)?;
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
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
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
                    fault_stack: stack,
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
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::Native(operation @ (NativeInstr::TextHash | NativeInstr::BytesHash)) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
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
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            push_static(builder, stack, ScalarKind::Int, result)?;
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
                Some(site) => {
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?
                }
                None => Vec::new(),
            };
            let zero = builder.ins().iconst(types::I64, 0);
            let (arguments, function_offset, result_kind) = match instruction {
                Instr::Native(NativeInstr::StrConcat) => {
                    let right = pop_native(stack)?;
                    let left = pop_native(stack)?;
                    (
                        [left, right, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_concat),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::StrStartsWith) => {
                    let prefix = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, prefix, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_starts_with),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::StrEndsWith) => {
                    let suffix = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, suffix, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_ends_with),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::StrContains) => {
                    let needle = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, needle, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_contains),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::StrFindIndex) => {
                    let needle = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, needle, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_find_scalar),
                        ScalarKind::Int,
                    )
                }
                Instr::Native(NativeInstr::TextFindByteIndex) => {
                    let needle = pop_native(stack)?;
                    let text = pop_native(stack)?;
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
                    let text = pop_native(stack)?;
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
                    let text = pop_native(stack)?;
                    let function_offset = if matches!(operation, NativeInstr::TextToLowerAscii) {
                        std_mem::offset_of!(RawNativeFunctions, text_lower_ascii)
                    } else {
                        std_mem::offset_of!(RawNativeFunctions, text_upper_ascii)
                    };
                    ([text, zero, zero], function_offset, ScalarKind::Object(0))
                }
                Instr::Native(NativeInstr::TextReplace) => {
                    let replacement = pop_native(stack)?;
                    let needle = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, needle, replacement],
                        std_mem::offset_of!(RawNativeFunctions, text_replace),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(
                    operation @ (NativeInstr::TextParseIntStatus | NativeInstr::TextParseIntValue),
                ) => {
                    let radix = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    let function_offset = if matches!(operation, NativeInstr::TextParseIntStatus) {
                        std_mem::offset_of!(RawNativeFunctions, text_parse_int_status)
                    } else {
                        std_mem::offset_of!(RawNativeFunctions, text_parse_int_value)
                    };
                    ([text, radix, zero], function_offset, ScalarKind::Int)
                }
                Instr::Native(
                    operation @ (NativeInstr::TextPadStart | NativeInstr::TextPadEnd),
                ) => {
                    let width = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    let function_offset = if matches!(operation, NativeInstr::TextPadStart) {
                        std_mem::offset_of!(RawNativeFunctions, text_pad_start)
                    } else {
                        std_mem::offset_of!(RawNativeFunctions, text_pad_end)
                    };
                    ([text, width, zero], function_offset, ScalarKind::Object(0))
                }
                Instr::Native(NativeInstr::BytesEndsWith) => {
                    let suffix = pop_native(stack)?;
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, suffix, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_ends_with),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::BytesContains) => {
                    let needle = pop_native(stack)?;
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, needle, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_contains),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::TextSplit) => {
                    let separator = pop_native(stack)?;
                    let text = pop_native(stack)?;
                    (
                        [text, separator, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_split),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::TextLines) => {
                    let text = pop_native(stack)?;
                    (
                        [text, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_lines),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(
                    operation @ (NativeInstr::TextSlice | NativeInstr::TextSliceBytes),
                ) => {
                    let length = pop_native(stack)?;
                    let start = pop_native(stack)?;
                    let text = pop_native(stack)?;
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
                    let text = pop_native(stack)?;
                    (
                        [text, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_bytes),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::TextToString) => {
                    let text = pop_native(stack)?;
                    (
                        [text, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, text_to_string),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::BytesText) => {
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_text),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::BbFindFrom) => {
                    let start = pop_native(stack)?;
                    let needle = pop_native(stack)?;
                    let buffer = pop_native(stack)?;
                    (
                        [buffer, needle, start],
                        std_mem::offset_of!(RawNativeFunctions, byte_buffer_find_from),
                        ScalarKind::Int,
                    )
                }
                Instr::Native(NativeInstr::BytesStartsWith) => {
                    let prefix = pop_native(stack)?;
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, prefix, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_starts_with),
                        ScalarKind::Bool,
                    )
                }
                Instr::Native(NativeInstr::BytesFindIndex) => {
                    let needle = pop_native(stack)?;
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, needle, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_find_index),
                        ScalarKind::Int,
                    )
                }
                Instr::Native(NativeInstr::BytesHex) => {
                    let bytes = pop_native(stack)?;
                    (
                        [bytes, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_hex),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Native(NativeInstr::BytesIsUtf8) => {
                    let bytes = pop_native(stack)?;
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
                    let text = pop_native(stack)?;
                    let (function_offset, result_kind) =
                        if matches!(operation, NumericInstr::TextParseFloatStatus) {
                            (
                                std_mem::offset_of!(RawNativeFunctions, text_parse_float_status),
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
                    let digits = pop_native(stack)?;
                    let value = pop_native(stack)?;
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
                    fault_stack: stack,
                    deopt_stack: &deopt_stack,
                },
            )?;
            push_static(builder, stack, result_kind, result)?;
        }
        Instr::EqRef | Instr::NeRef => {
            let release_right = virtual_stack.last().copied().unwrap_or(false);
            let release_left = virtual_stack
                .len()
                .checked_sub(2)
                .and_then(|index| virtual_stack.get(index))
                .copied()
                .unwrap_or(false);
            let right = pop_native(stack)?;
            let left = pop_native(stack)?;
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
            push_static(builder, stack, ScalarKind::Bool, result)?;
        }
        Instr::Native(operation) => {
            emit_char_instruction(builder, stack, operation)?;
        }
        Instr::Numeric(operation) => {
            let deopt_stack = stack.clone();
            emit_numeric_instruction(
                builder,
                values,
                stack,
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
        _ => return Err(CompileError::Backend),
    }
    Ok(())
}
