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
    let within = emission.within;
    let fault_prefix = emission.fault_prefix;
    match instruction {
        Instr::Extended(ExtendedInstr::SealInstance) => {
            let deopt_stack = stack.clone();
            let allow_pending = virtual_stack.last().copied().unwrap_or(false);
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            let HeapAccessKind::SealInstance { class } = access.kind else {
                return Err(CompileError::Backend);
            };
            emit_seal_instance(
                builder,
                values,
                reference,
                class,
                allow_pending,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Object(0), reference)?;
        }
        Instr::Native(
            NativeInstr::SbAppendStr
            | NativeInstr::SbAppendInt
            | NativeInstr::SbAppendBool
            | NativeInstr::SbAppendChar
            | NativeInstr::BbAppend
            | NativeInstr::BbExtend
            | NativeInstr::BbReserve,
        ) => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let deopt_stack = stack.clone();
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?;
            let point = FaultPoint {
                block: segment.block,
                instruction: position + 1,
                prefix: fault_prefix,
            };
            let result = match instruction {
                Instr::Native(NativeInstr::SbAppendStr) => {
                    let source = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_string_builder_append_text(
                        builder,
                        values,
                        target,
                        source,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::SbAppendBool) => {
                    let value = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_string_builder_append_bool(
                        builder,
                        values,
                        target,
                        value,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::SbAppendInt) => {
                    let value = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_string_builder_append_int(
                        builder,
                        values,
                        target,
                        value,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::SbAppendChar) => {
                    let value = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_string_builder_append_char(
                        builder,
                        values,
                        target,
                        value,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::BbAppend) => {
                    let value = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_byte_buffer_append(
                        builder,
                        values,
                        target,
                        value,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::BbExtend) => {
                    let source = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_byte_buffer_extend(
                        builder,
                        values,
                        target,
                        source,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                Instr::Native(NativeInstr::BbReserve) => {
                    let additional = pop_native(stack)?;
                    let target = pop_native(stack)?;
                    emit_byte_buffer_reserve(
                        builder,
                        values,
                        target,
                        additional,
                        &roots,
                        HeapExitEmission {
                            point,
                            fault_stack: stack,
                            deopt_stack: &deopt_stack,
                        },
                    )?
                }
                _ => return Err(CompileError::Backend),
            };
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::FaultCode
        | Instr::FaultDenied
        | Instr::Extended(ExtendedInstr::DynPack { .. })
        | Instr::Native(
            NativeInstr::SbNew
            | NativeInstr::SbBuild
            | NativeInstr::SbFinish
            | NativeInstr::BbNew
            | NativeInstr::BbBuild
            | NativeInstr::BbFinish
            | NativeInstr::BytesNew
            | NativeInstr::BytesSlice
            | NativeInstr::BytesConcat
            | NativeInstr::BytesCompact
            | NativeInstr::BytesTextView,
        )
        | Instr::Numeric(
            NumericInstr::SbAppendFloat
            | NumericInstr::BytesBitAnd
            | NumericInstr::BytesBitOr
            | NumericInstr::BytesBitXor
            | NumericInstr::BytesBitNot,
        ) => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let deopt_stack = stack.clone();
            let roots =
                collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?;
            let zero = builder.ins().iconst(types::I64, 0);
            let (arguments, function_offset) = match instruction {
                Instr::FaultCode => {
                    let fault = pop_native(stack)?;
                    (
                        [fault, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, fault_code),
                    )
                }
                Instr::FaultDenied => {
                    let reason = pop_native(stack)?;
                    (
                        [reason, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, fault_denied),
                    )
                }
                Instr::Extended(ExtendedInstr::DynPack { ty }) => {
                    let value = pop_value(stack)?;
                    let frame = emit_current_frame_pointer(builder, values)?;
                    let environment = load_cell_u32(
                        builder,
                        frame,
                        std_mem::offset_of!(RawNativeFrame, environment),
                    )?;
                    let environment = builder.ins().uextend(types::I64, environment);
                    let environment = builder.ins().ishl_imm(environment, 32);
                    let ty = builder.ins().iconst(types::I64, i64::from(ty));
                    let packed = builder.ins().bor(ty, environment);
                    (
                        [value.bits, value.tag, packed],
                        std_mem::offset_of!(RawNativeFunctions, dyn_pack),
                    )
                }
                Instr::Native(NativeInstr::SbNew) => (
                    [zero, zero, zero],
                    std_mem::offset_of!(RawNativeFunctions, string_builder_new),
                ),
                Instr::Native(NativeInstr::BbNew) => (
                    [zero, zero, zero],
                    std_mem::offset_of!(RawNativeFunctions, byte_buffer_new),
                ),
                Instr::Numeric(NumericInstr::SbAppendFloat) => {
                    let value = pop_native(stack)?;
                    let builder_value = pop_native(stack)?;
                    (
                        [builder_value, value, zero],
                        std_mem::offset_of!(RawNativeFunctions, string_builder_append_float),
                    )
                }
                Instr::Native(NativeInstr::SbBuild) => {
                    let builder_value = pop_native(stack)?;
                    (
                        [builder_value, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, string_builder_build),
                    )
                }
                Instr::Native(NativeInstr::SbFinish) => {
                    let builder_value = pop_native(stack)?;
                    (
                        [builder_value, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, string_builder_finish),
                    )
                }
                Instr::Native(NativeInstr::BbBuild) => {
                    let buffer = pop_native(stack)?;
                    (
                        [buffer, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, byte_buffer_build),
                    )
                }
                Instr::Native(NativeInstr::BbFinish) => {
                    let buffer = pop_native(stack)?;
                    (
                        [buffer, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, byte_buffer_finish),
                    )
                }
                Instr::Native(NativeInstr::BytesNew) => {
                    let source = pop_native(stack)?;
                    (
                        [source, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_from_text),
                    )
                }
                Instr::Native(NativeInstr::BytesSlice) => {
                    let length = pop_native(stack)?;
                    let start = pop_native(stack)?;
                    let source = pop_native(stack)?;
                    (
                        [source, start, length],
                        std_mem::offset_of!(RawNativeFunctions, bytes_slice),
                    )
                }
                Instr::Native(NativeInstr::BytesConcat) => {
                    let right = pop_native(stack)?;
                    let left = pop_native(stack)?;
                    (
                        [left, right, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_concat),
                    )
                }
                Instr::Native(NativeInstr::BytesCompact) => {
                    let source = pop_native(stack)?;
                    (
                        [source, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_compact),
                    )
                }
                Instr::Native(NativeInstr::BytesTextView) => {
                    let source = pop_native(stack)?;
                    (
                        [source, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_text_view),
                    )
                }
                Instr::Numeric(NumericInstr::BytesBitAnd) => {
                    let right = pop_native(stack)?;
                    let left = pop_native(stack)?;
                    (
                        [left, right, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_bit_and),
                    )
                }
                Instr::Numeric(NumericInstr::BytesBitOr) => {
                    let right = pop_native(stack)?;
                    let left = pop_native(stack)?;
                    (
                        [left, right, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_bit_or),
                    )
                }
                Instr::Numeric(NumericInstr::BytesBitXor) => {
                    let right = pop_native(stack)?;
                    let left = pop_native(stack)?;
                    (
                        [left, right, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_bit_xor),
                    )
                }
                Instr::Numeric(NumericInstr::BytesBitNot) => {
                    let source = pop_native(stack)?;
                    (
                        [source, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, bytes_bit_not),
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
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::Native(NativeInstr::SbLen | NativeInstr::SbByteLen | NativeInstr::BbLen) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let position = segment.start + within as u32;
            let (tag, active, length) = match instruction {
                Instr::Native(NativeInstr::SbLen) => (
                    JIT_OBJECT_STRING_BUILDER,
                    JIT_STRING_BUILDER_ACTIVE_OFFSET,
                    JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
                ),
                Instr::Native(NativeInstr::SbByteLen) => (
                    JIT_OBJECT_STRING_BUILDER,
                    JIT_STRING_BUILDER_ACTIVE_OFFSET,
                    JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
                ),
                Instr::Native(NativeInstr::BbLen) => (
                    JIT_OBJECT_BYTE_BUFFER,
                    JIT_BYTE_BUFFER_ACTIVE_OFFSET,
                    JIT_BYTE_BUFFER_LEN_OFFSET,
                ),
                _ => return Err(CompileError::Backend),
            };
            let result = emit_builder_len(
                builder,
                values,
                reference,
                tag,
                (active, length),
                FaultPoint {
                    block: segment.block,
                    instruction: position + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::Native(NativeInstr::SbClear | NativeInstr::BbClear) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let position = segment.start + within as u32;
            emit_builder_clear(
                builder,
                values,
                reference,
                matches!(instruction, Instr::Native(NativeInstr::SbClear)),
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
            push_static(builder, stack, ScalarKind::Object(0), reference)?;
        }
        Instr::Native(NativeInstr::BbAt) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let position = segment.start + within as u32;
            let result = emit_byte_buffer_at(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: position + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, result)?;
        }
        Instr::Native(NativeInstr::BytesLen) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::BytesLen) {
                return Err(CompileError::Backend);
            }
            let value = emit_bytes_len(
                builder,
                values,
                reference,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Native(NativeInstr::BytesAt) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::BytesAt) {
                return Err(CompileError::Backend);
            }
            let value = emit_bytes_at(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Native(NativeInstr::BytesGet) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::BytesGet) {
                return Err(CompileError::Backend);
            }
            let value = emit_bytes_get(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
            let deopt_stack = stack.clone();
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            let offset = match access.kind {
                HeapAccessKind::TextByteLen => JIT_TEXT_BYTE_LEN_OFFSET,
                HeapAccessKind::TextScalarLen => JIT_TEXT_SCALAR_LEN_OFFSET,
                _ => return Err(CompileError::Backend),
            };
            let value = emit_text_len(
                builder,
                values,
                reference,
                offset,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Int, value)?;
        }
        Instr::Native(NativeInstr::TextAtByte) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::TextAtByte) {
                return Err(CompileError::Backend);
            }
            let value = emit_text_at_byte(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Char, value)?;
        }
        Instr::Native(NativeInstr::TextAt) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::TextAt) {
                return Err(CompileError::Backend);
            }
            let value = emit_text_at(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Char, value)?;
        }
        Instr::Native(NativeInstr::TextIsBoundary) => {
            let deopt_stack = stack.clone();
            let index = pop_native(stack)?;
            let reference = pop_native(stack)?;
            let instruction_index = segment.start + within as u32;
            let access = segment
                .heap_accesses
                .iter()
                .find(|access| access.instruction == instruction_index)
                .copied()
                .ok_or(CompileError::Backend)?;
            if !matches!(access.kind, HeapAccessKind::TextIsBoundary) {
                return Err(CompileError::Backend);
            }
            let value = emit_text_is_boundary(
                builder,
                values,
                reference,
                index,
                FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                },
                &deopt_stack,
            )?;
            push_static(builder, stack, ScalarKind::Bool, value)?;
        }
        Instr::Extended(
            ExtendedInstr::SyntaxTreeRoot
            | ExtendedInstr::SyntaxKind
            | ExtendedInstr::SyntaxCategory
            | ExtendedInstr::SyntaxRangeStart
            | ExtendedInstr::SyntaxRangeEnd
            | ExtendedInstr::SyntaxText
            | ExtendedInstr::SyntaxChildren
            | ExtendedInstr::SyntaxDetach
            | ExtendedInstr::SyntaxBuildToken
            | ExtendedInstr::SyntaxBuildTrivia
            | ExtendedInstr::SyntaxBuildNode
            | ExtendedInstr::SyntaxToTree,
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
                Instr::Extended(ExtendedInstr::SyntaxTreeRoot) => {
                    let tree = pop_native(stack)?;
                    (
                        [tree, zero, zero],
                        std_mem::offset_of!(RawNativeFunctions, syntax_tree_root),
                        ScalarKind::Object(0),
                    )
                }
                Instr::Extended(
                    operation @ (ExtendedInstr::SyntaxKind
                    | ExtendedInstr::SyntaxCategory
                    | ExtendedInstr::SyntaxRangeStart
                    | ExtendedInstr::SyntaxRangeEnd),
                ) => {
                    let element = pop_native(stack)?;
                    let function_offset = match operation {
                        ExtendedInstr::SyntaxKind => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_kind)
                        }
                        ExtendedInstr::SyntaxCategory => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_category)
                        }
                        ExtendedInstr::SyntaxRangeStart => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_range_start)
                        }
                        ExtendedInstr::SyntaxRangeEnd => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_range_end)
                        }
                        _ => return Err(CompileError::Backend),
                    };
                    ([element, zero, zero], function_offset, ScalarKind::Int)
                }
                Instr::Extended(
                    operation @ (ExtendedInstr::SyntaxText
                    | ExtendedInstr::SyntaxChildren
                    | ExtendedInstr::SyntaxDetach
                    | ExtendedInstr::SyntaxToTree),
                ) => {
                    let element = pop_native(stack)?;
                    let function_offset = match operation {
                        ExtendedInstr::SyntaxText => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_text)
                        }
                        ExtendedInstr::SyntaxChildren => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_children)
                        }
                        ExtendedInstr::SyntaxDetach => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_detach)
                        }
                        ExtendedInstr::SyntaxToTree => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_to_tree)
                        }
                        _ => return Err(CompileError::Backend),
                    };
                    (
                        [element, zero, zero],
                        function_offset,
                        ScalarKind::Object(0),
                    )
                }
                Instr::Extended(
                    operation @ (ExtendedInstr::SyntaxBuildToken
                    | ExtendedInstr::SyntaxBuildTrivia
                    | ExtendedInstr::SyntaxBuildNode),
                ) => {
                    let value = pop_native(stack)?;
                    let kind = pop_native(stack)?;
                    let builder_value = pop_native(stack)?;
                    let function_offset = match operation {
                        ExtendedInstr::SyntaxBuildToken => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_build_token)
                        }
                        ExtendedInstr::SyntaxBuildTrivia => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_build_trivia)
                        }
                        ExtendedInstr::SyntaxBuildNode => {
                            std_mem::offset_of!(RawNativeFunctions, syntax_build_node)
                        }
                        _ => return Err(CompileError::Backend),
                    };
                    (
                        [builder_value, kind, value],
                        function_offset,
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
        _ => return Err(CompileError::Backend),
    }
    Ok(())
}
