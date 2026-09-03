//! Regular-expression instruction emission.

use super::*;

enum RegexResult {
    Static(ScalarKind),
    Optional {
        family: ir::Value,
        value: ValueContract,
    },
}

pub(super) fn emit(emission: &mut InstructionEmission<'_, '_, '_, '_>) -> Result<(), CompileError> {
    let instruction = emission.instruction;
    let builder = &mut *emission.builder;
    let values = emission.values;
    let plan = emission.plan;
    let input = emission.input;
    let segment = emission.segment;
    let stack = &mut *emission.stack;
    let position = segment.start + emission.within as u32;
    let deopt_stack = stack.clone();
    let roots = match segment
        .allocations
        .iter()
        .find(|site| site.instruction == position)
    {
        Some(site) => collect_native_roots(builder, values, &plan.local_kinds, &site.stack, stack)?,
        None => Vec::new(),
    };
    let zero = builder.ins().iconst(types::I64, 0);
    let optional = match instruction {
        Instr::Extended(
            ExtendedInstr::RegexCaptures { .. }
            | ExtendedInstr::RegexMatchGroup { .. }
            | ExtendedInstr::RegexMatchNamed { .. },
        ) => {
            let access = segment
                .option_accesses
                .iter()
                .find(|access| access.instruction == position)
                .copied()
                .ok_or(CompileError::Backend)?;
            let value = match (instruction, access.kind) {
                (
                    Instr::Extended(ExtendedInstr::RegexCaptures { .. }),
                    OptionAccessKind::RegexCaptures { value },
                )
                | (
                    Instr::Extended(ExtendedInstr::RegexMatchGroup { .. }),
                    OptionAccessKind::RegexMatchGroup { value },
                )
                | (
                    Instr::Extended(ExtendedInstr::RegexMatchNamed { .. }),
                    OptionAccessKind::RegexMatchNamed { value },
                ) => value,
                _ => return Err(CompileError::Backend),
            };
            let family = emit_option_family(
                builder,
                values,
                input.root.function,
                access.family_type,
                FaultPoint {
                    block: segment.block,
                    instruction: position,
                    prefix: emission.prior_prefix,
                },
                &deopt_stack,
            )?;
            Some((family, value))
        }
        _ => None,
    };
    let (arguments, function_offset, result) = match instruction {
        Instr::Native(NativeInstr::RegexCompileStatus) => {
            let pattern = pop_native(stack)?;
            (
                [pattern, zero, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_compile_status),
                RegexResult::Static(ScalarKind::Int),
            )
        }
        Instr::Native(NativeInstr::RegexCompileValue) => {
            let pattern = pop_native(stack)?;
            (
                [pattern, zero, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_compile_value),
                RegexResult::Static(ScalarKind::Object(0)),
            )
        }
        Instr::Native(NativeInstr::RegexSource) => {
            let regex = pop_native(stack)?;
            (
                [regex, zero, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_source),
                RegexResult::Static(ScalarKind::Object(0)),
            )
        }
        Instr::Native(NativeInstr::RegexIsMatch) => {
            let text = pop_native(stack)?;
            let regex = pop_native(stack)?;
            (
                [regex, text, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_is_match),
                RegexResult::Static(ScalarKind::Bool),
            )
        }
        Instr::Extended(ExtendedInstr::RegexCaptures { .. }) => {
            let text = pop_native(stack)?;
            let regex = pop_native(stack)?;
            (
                [regex, text, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_captures),
                RegexResult::Optional {
                    family: optional.ok_or(CompileError::Backend)?.0,
                    value: optional.ok_or(CompileError::Backend)?.1,
                },
            )
        }
        Instr::Native(NativeInstr::RegexCount) => {
            let text = pop_native(stack)?;
            let regex = pop_native(stack)?;
            (
                [regex, text, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_count),
                RegexResult::Static(ScalarKind::Int),
            )
        }
        Instr::Native(NativeInstr::RegexSplit) => {
            let text = pop_native(stack)?;
            let regex = pop_native(stack)?;
            (
                [regex, text, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_split),
                RegexResult::Static(ScalarKind::Object(0)),
            )
        }
        Instr::Native(NativeInstr::RegexReplaceAll) => {
            let replacement = pop_native(stack)?;
            let text = pop_native(stack)?;
            let regex = pop_native(stack)?;
            (
                [regex, text, replacement],
                std_mem::offset_of!(RawNativeFunctions, regex_replace_all),
                RegexResult::Static(ScalarKind::Object(0)),
            )
        }
        Instr::Native(NativeInstr::RegexMatchStart) => regex_match_unary(
            stack,
            zero,
            std_mem::offset_of!(RawNativeFunctions, regex_match_start),
            ScalarKind::Int,
        )?,
        Instr::Native(NativeInstr::RegexMatchEnd) => regex_match_unary(
            stack,
            zero,
            std_mem::offset_of!(RawNativeFunctions, regex_match_end),
            ScalarKind::Int,
        )?,
        Instr::Native(NativeInstr::RegexMatchText) => regex_match_unary(
            stack,
            zero,
            std_mem::offset_of!(RawNativeFunctions, regex_match_text),
            ScalarKind::Object(0),
        )?,
        Instr::Native(NativeInstr::RegexMatchGroupCount) => regex_match_unary(
            stack,
            zero,
            std_mem::offset_of!(RawNativeFunctions, regex_match_group_count),
            ScalarKind::Int,
        )?,
        Instr::Extended(ExtendedInstr::RegexMatchGroup { .. }) => {
            let index = pop_native(stack)?;
            let matched = pop_native(stack)?;
            (
                [matched, index, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_match_group),
                RegexResult::Optional {
                    family: optional.ok_or(CompileError::Backend)?.0,
                    value: optional.ok_or(CompileError::Backend)?.1,
                },
            )
        }
        Instr::Extended(ExtendedInstr::RegexMatchNamed { .. }) => {
            let name = pop_native(stack)?;
            let matched = pop_native(stack)?;
            (
                [matched, name, zero],
                std_mem::offset_of!(RawNativeFunctions, regex_match_named),
                RegexResult::Optional {
                    family: optional.ok_or(CompileError::Backend)?.0,
                    value: optional.ok_or(CompileError::Backend)?.1,
                },
            )
        }
        _ => return Err(CompileError::Backend),
    };
    let bits = emit_heap_operation(
        builder,
        values,
        function_offset,
        arguments,
        &roots,
        HeapExitEmission {
            point: FaultPoint {
                block: segment.block,
                instruction: position + 1,
                prefix: emission.fault_prefix,
            },
            fault_stack: stack,
            deopt_stack: &deopt_stack,
        },
    )?;
    match result {
        RegexResult::Static(kind) => push_static(builder, stack, kind, bits)?,
        RegexResult::Optional { family, value } => {
            let Some(value_tag) = value_tag(value.kind) else {
                return Err(CompileError::Backend);
            };
            let missing =
                builder
                    .ins()
                    .icmp_imm(IntCC::Equal, bits, crate::REGEX_OPTION_NONE as i64);
            let none_bits = builder.ins().bor_imm(family, 1_i64 << 32);
            let result_bits = builder.ins().select(missing, none_bits, bits);
            let object_tag = builder.ins().iconst(types::I64, value_tag as u64 as i64);
            let none_tag = builder
                .ins()
                .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
            let tag = builder.ins().select(missing, none_tag, object_tag);
            stack.push(NativeValue {
                bits: result_bits,
                tag,
            });
        }
    }
    Ok(())
}

fn regex_match_unary(
    stack: &mut Vec<NativeValue>,
    zero: ir::Value,
    function_offset: usize,
    result: ScalarKind,
) -> Result<([ir::Value; 3], usize, RegexResult), CompileError> {
    let matched = pop_native(stack)?;
    Ok((
        [matched, zero, zero],
        function_offset,
        RegexResult::Static(result),
    ))
}
