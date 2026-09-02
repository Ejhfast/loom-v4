//! LMBC instruction dispatch.

use super::*;

mod allocation;
mod collections;
mod runtime;
mod scalar;
mod values;

pub(super) fn emit_segment_body(
    builder: &mut FunctionBuilder<'_>,
    emission: SegmentEmission<'_, '_>,
    mut stack: Vec<NativeValue>,
    mut virtual_stack: Vec<bool>,
) -> Result<(), CompileError> {
    let SegmentEmission {
        bytecode,
        segment,
        successor_blocks,
        values,
        plan,
        input,
        type_environment_sites,
    } = emission;
    {
        let mut translations = values.heap_translations.borrow_mut();
        translations.clear();
        translations.set_cached_list_data(true);
    }
    let reserved_prefix_cost = segment.reserved_prefix_cost;
    let fast_segment_cost = reserved_prefix_cost
        .checked_add(segment.cost)
        .ok_or(CompileError::Backend)?;
    let mut deferred_integer_overflow = if segment.defer_integer_overflow {
        Some(DeferredIntegerOverflow {
            flag: None,
            locals: capture_local_values(builder, values)?,
            stack: stack.clone(),
        })
    } else {
        None
    };
    // The entry guard initializes each live local in canonical state storage.
    // A store only initializes a slot that was dormant at this entry.
    let mut initialized_locals = segment.live_in.clone();
    let mut virtual_locals = segment.virtual_locals_in.clone();
    if virtual_stack.len() != stack.len() || virtual_locals.len() != plan.local_kinds.len() {
        return Err(CompileError::Backend);
    }
    let code =
        &bytecode.blocks[segment.block as usize][segment.start as usize..segment.end as usize];
    for (within, instruction) in code.iter().copied().enumerate() {
        let prefix = within as u32 + 1;
        let fault_prefix = reserved_prefix_cost
            .checked_add(prefix)
            .ok_or(CompileError::Backend)?;
        let prior_prefix = reserved_prefix_cost
            .checked_add(prefix - 1)
            .ok_or(CompileError::Backend)?;
        let position = segment.start + within as u32;
        let source_instruction = input
            .root
            .source
            .funcs
            .get(input.root.source_function as usize)
            .and_then(|function| function.blocks.get(segment.block as usize))
            .and_then(|block| block.get(position as usize))
            .copied()
            .ok_or(CompileError::Backend)?;
        if segment.virtual_barriers.binary_search(&position).is_ok() {
            emit_pending_instance_barrier(
                builder,
                values,
                FaultPoint {
                    block: segment.block,
                    instruction: position,
                    prefix: prior_prefix,
                },
                &stack,
            )?;
            virtual_locals.fill(false);
            virtual_stack.fill(false);
        }
        {
            let mut emission = InstructionEmission {
                builder,
                values,
                plan,
                input,
                segment,
                type_environment_sites,
                stack: &mut stack,
                virtual_stack: &mut virtual_stack,
                initialized_locals: &mut initialized_locals,
                virtual_locals: &mut virtual_locals,
                deferred_integer_overflow: &mut deferred_integer_overflow,
                instruction,
                within,
                prefix,
                fault_prefix,
                prior_prefix,
            };
            #[allow(unused_variables)]
            match instruction {
                Instr::ConstUnit => values::emit(&mut emission)?,
                Instr::MakeClosure { func, captures } => allocation::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MakeCallback { func, captures }) => {
                    allocation::emit(&mut emission)?
                }
                Instr::TupleNew { count, .. }
                | Instr::ListNew { count, .. }
                | Instr::MapNew { count, .. } => allocation::emit(&mut emission)?,
                Instr::New(class) | Instr::NewG { class, .. } => allocation::emit(&mut emission)?,
                Instr::ConstBool(value) => values::emit(&mut emission)?,
                Instr::ConstInt(value) => values::emit(&mut emission)?,
                Instr::ConstFloat(bits) => values::emit(&mut emission)?,
                Instr::ConstChar(value) => values::emit(&mut emission)?,
                Instr::ConstStr(index) => values::emit(&mut emission)?,
                Instr::ConstBytes(index) => values::emit(&mut emission)?,
                Instr::OpConst(operation) => values::emit(&mut emission)?,
                Instr::LoadLocal(slot) => values::emit(&mut emission)?,
                Instr::StoreLocal(slot) => values::emit(&mut emission)?,
                Instr::Pop => values::emit(&mut emission)?,
                Instr::LoadCapture(index) => values::emit(&mut emission)?,
                Instr::LoadField(field) => values::emit(&mut emission)?,
                Instr::StoreField(field) => values::emit(&mut emission)?,
                Instr::TupleGet(index) => values::emit(&mut emission)?,
                Instr::EqDigest | Instr::NeDigest => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::AsCallback) => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::OptionSome { .. }) => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::OptionNone { .. }) => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::OptionPayload { .. }) => {
                    values::emit(&mut emission)?
                }
                Instr::Extended(ExtendedInstr::ListGet { .. }) => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListPop { .. }) => values::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListContains) => values::emit(&mut emission)?,
                Instr::IsType(_) | Instr::CastType(_) => values::emit(&mut emission)?,
                Instr::ListLen => collections::emit(&mut emission)?,
                Instr::MapLen => collections::emit(&mut emission)?,
                Instr::MapHas | Instr::MapAt => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapGet { .. }) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapPutText { .. }) => {
                    collections::emit(&mut emission)?
                }
                Instr::MapPut { discard, .. } => collections::emit(&mut emission)?,
                Instr::ListAt => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListSet) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListInsert) => collections::emit(&mut emission)?,
                Instr::Extended(
                    operation @ (ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove),
                ) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListTruncate) => collections::emit(&mut emission)?,
                Instr::ListPush => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListReserve) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListReorder) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListCapacity) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListEpoch) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::ListIterLen) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapEpoch) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapIterLen) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapNextIndex) => collections::emit(&mut emission)?,
                Instr::Extended(
                    operation @ (ExtendedInstr::MapKeyAt | ExtendedInstr::MapValueAt),
                ) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapRemove { .. }) => {
                    collections::emit(&mut emission)?
                }
                Instr::Extended(ExtendedInstr::MapClear) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapReserve) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapProbe) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapProbeFound) => collections::emit(&mut emission)?,
                Instr::Extended(
                    operation @ (ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue),
                ) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapProbeSetValue) => {
                    collections::emit(&mut emission)?
                }
                Instr::Extended(ExtendedInstr::MapProbeRemove) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::MapInsertHashed) => {
                    collections::emit(&mut emission)?
                }
                Instr::Extended(ExtendedInstr::MapWriteGuard) => collections::emit(&mut emission)?,
                Instr::Extended(ExtendedInstr::SealInstance) => runtime::emit(&mut emission)?,
                Instr::Native(
                    NativeInstr::SbAppendStr
                    | NativeInstr::SbAppendInt
                    | NativeInstr::SbAppendBool
                    | NativeInstr::SbAppendChar
                    | NativeInstr::BbAppend
                    | NativeInstr::BbExtend
                    | NativeInstr::BbReserve,
                ) => runtime::emit(&mut emission)?,
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
                    | NativeInstr::BytesTextRange
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
                ) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::SbLen | NativeInstr::SbByteLen | NativeInstr::BbLen) => {
                    runtime::emit(&mut emission)?
                }
                Instr::Native(NativeInstr::SbClear | NativeInstr::BbClear) => {
                    runtime::emit(&mut emission)?
                }
                Instr::Native(NativeInstr::BbAt) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::BytesLen) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::BytesAt) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::BytesGet) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                    runtime::emit(&mut emission)?
                }
                Instr::Native(NativeInstr::TextAtByte) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::TextAt) => runtime::emit(&mut emission)?,
                Instr::Native(NativeInstr::TextIsBoundary) => runtime::emit(&mut emission)?,
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
                ) => runtime::emit(&mut emission)?,
                Instr::Add | Instr::Sub | Instr::Mul => scalar::emit(&mut emission)?,
                Instr::Div | Instr::Rem => scalar::emit(&mut emission)?,
                Instr::Neg => scalar::emit(&mut emission)?,
                Instr::Not => scalar::emit(&mut emission)?,
                Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
                    scalar::emit(&mut emission)?
                }
                Instr::LtInt
                | Instr::LeInt
                | Instr::GtInt
                | Instr::GeInt
                | Instr::EqInt
                | Instr::NeInt => scalar::emit(&mut emission)?,
                Instr::EqBool | Instr::NeBool => scalar::emit(&mut emission)?,
                Instr::EqValue | Instr::NeValue => scalar::emit(&mut emission)?,
                Instr::Freeze => scalar::emit(&mut emission)?,
                Instr::Digest { ty } => scalar::emit(&mut emission)?,
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
                ) => scalar::emit(&mut emission)?,
                Instr::Native(operation @ (NativeInstr::TextHash | NativeInstr::BytesHash)) => {
                    scalar::emit(&mut emission)?
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
                ) => scalar::emit(&mut emission)?,
                Instr::EqRef | Instr::NeRef => scalar::emit(&mut emission)?,
                Instr::Native(operation) => scalar::emit(&mut emission)?,
                Instr::Numeric(operation) => scalar::emit(&mut emission)?,
                Instr::Call(_)
                | Instr::CallG { .. }
                | Instr::CallVirtual { .. }
                | Instr::CallVirtualG { .. }
                | Instr::CallInterface { .. }
                | Instr::CallValue { .. }
                | Instr::Extended(ExtendedInstr::CallSlot { .. } | ExtendedInstr::NewSlot { .. })
                | Instr::Perform { .. }
                | Instr::PerformValue { .. }
                | Instr::TableEdit { .. }
                | Instr::AsCall { .. }
                | Instr::CallArgs
                | Instr::RequestOp
                | Instr::RaiseUserPanic
                | Instr::RaiseAssertionFailed
                | Instr::RaiseFault
                | Instr::Extended(ExtendedInstr::LoadSlot { .. })
                | Instr::Extended(ExtendedInstr::SendSlot { .. })
                | Instr::Extended(ExtendedInstr::PrepareWait { .. })
                | Instr::Extended(ExtendedInstr::DynRender)
                | Instr::Extended(ExtendedInstr::FunctionCode { .. })
                | Instr::Extended(ExtendedInstr::ClassCode { .. })
                | Instr::Extended(ExtendedInstr::CodeSource { .. })
                | Instr::Extended(ExtendedInstr::CodeDefinition)
                | Instr::Extended(ExtendedInstr::FaultSite { .. })
                | Instr::Extended(ExtendedInstr::FaultTrace { .. })
                | Instr::Jump(_)
                | Instr::JumpIfFalse(_)
                | Instr::JumpIfTrue(_)
                | Instr::Unreachable
                | Instr::Return => {}
            }
        }
        if matches!(
            crate::instruction_treatment(&instruction).class(),
            TreatmentClass::FastPath
                | TreatmentClass::Call
                | TreatmentClass::Helper
                | TreatmentClass::Exit
        ) {
            values.heap_translations.borrow_mut().clear();
        }
        let exit_handles_stack = within + 1 == code.len()
            && matches!(
                segment.exit,
                SegmentExit::Conditional { .. }
                    | SegmentExit::Call { .. }
                    | SegmentExit::VirtualCall { .. }
                    | SegmentExit::ValueCall { .. }
                    | SegmentExit::GenericVirtualCall { .. }
                    | SegmentExit::InterfaceCall { .. }
                    | SegmentExit::SlotCall { .. }
                    | SegmentExit::Effect { .. }
                    | SegmentExit::Boundary { .. }
                    | SegmentExit::Return
            );
        if !exit_handles_stack {
            let call = matches!(instruction, Instr::Call(_) | Instr::CallG { .. })
                .then_some(segment.call_contract.as_ref())
                .flatten();
            let virtual_new =
                plan.virtual_constructor
                    .is_some_and(|constructor| match instruction {
                        Instr::New(class) | Instr::NewG { class, .. } => class == constructor.class,
                        _ => false,
                    });
            transfer_virtual_instruction(
                input.root.source,
                source_instruction,
                instruction,
                call,
                virtual_new,
                &mut virtual_locals,
                &mut virtual_stack,
            )?;
        }
        debug_assert_eq!(virtual_stack.len(), stack.len());
    }

    if let Some(deferred) = deferred_integer_overflow {
        let overflow = deferred.flag.ok_or(CompileError::Backend)?;
        emit_deferred_integer_overflow_replay(
            builder,
            values,
            overflow,
            segment.block,
            segment.start,
            reserved_prefix_cost,
            &deferred.locals,
            &deferred.stack,
        )?;
    }

    if matches!(
        segment.exit,
        SegmentExit::Call { .. }
            | SegmentExit::VirtualCall { .. }
            | SegmentExit::ValueCall { .. }
            | SegmentExit::GenericVirtualCall { .. }
            | SegmentExit::InterfaceCall { .. }
            | SegmentExit::SlotCall { .. }
    ) {
        let call_instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let contract = segment
            .call_contract
            .as_ref()
            .ok_or(CompileError::Backend)?;
        if let SegmentExit::Call {
            target, app: None, ..
        } = segment.exit
        {
            if let Some(inline) = plan.inline_functions.get(&target) {
                let definition = input.definition(target).ok_or(CompileError::Backend)?;
                emit_inline_call(
                    builder,
                    values,
                    input,
                    &mut stack,
                    InlineCallEmission {
                        definition,
                        inline,
                        contract,
                        boundary_len: segment.boundary_stack.len(),
                        block: segment.block,
                        instruction: call_instruction,
                        successor: successor_blocks[0],
                    },
                )?;
                return Ok(());
            }
        }
        let capture = if matches!(segment.exit, SegmentExit::ValueCall { .. }) {
            let callable = stack
                .len()
                .checked_sub(
                    contract
                        .params
                        .len()
                        .checked_add(1)
                        .ok_or(CompileError::Backend)?,
                )
                .and_then(|index| stack.get(index))
                .copied()
                .ok_or(CompileError::Backend)?;
            Some(callable)
        } else {
            None
        };
        let target = match segment.exit {
            SegmentExit::Call {
                target,
                app: Some(application),
                ..
            } => {
                let site = type_environment_sites
                    .iter()
                    .find(|site| {
                        site.block == segment.block
                            && site.instruction == call_instruction
                            && site.application == application
                    })
                    .ok_or(CompileError::Backend)?;
                let environment = emit_type_environment_lookup(
                    builder,
                    values,
                    site,
                    FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix: 0,
                    },
                    &stack,
                )?;
                NativeCallTarget {
                    function: builder.ins().iconst(types::I32, i64::from(target)),
                    environment,
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: None,
                }
            }
            SegmentExit::Call {
                target, app: None, ..
            } => NativeCallTarget {
                function: builder.ins().iconst(types::I32, i64::from(target)),
                environment: builder.ins().iconst(types::I32, 0),
                capture_data: builder.ins().iconst(values.pointer_type, 0),
                capture_len: builder.ins().iconst(values.pointer_type, 0),
                fault: None,
            },
            SegmentExit::VirtualCall { selector, .. } => {
                let receiver = contract.receiver.ok_or(CompileError::Backend)?;
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let target = emit_virtual_target(
                    builder,
                    values,
                    receiver_value,
                    receiver,
                    selector,
                    FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix: 0,
                    },
                    &stack,
                )?;
                NativeCallTarget {
                    function: target,
                    environment: builder.ins().iconst(types::I32, 0),
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: None,
                }
            }
            SegmentExit::ValueCall { .. } => emit_call_value_target(
                builder,
                values,
                input.root.function,
                capture.ok_or(CompileError::Backend)?,
                contract.value_target.ok_or(CompileError::Backend)?,
                FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                },
                &stack,
            )?,
            SegmentExit::GenericVirtualCall { .. } => {
                let receiver = contract.receiver.ok_or(CompileError::Backend)?;
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let receiver = emit_generic_virtual_receiver_key(
                    builder,
                    values,
                    receiver_value,
                    receiver,
                    point,
                    &stack,
                )?;
                emit_resolved_call_lookup(
                    builder,
                    values,
                    input.root.function,
                    point,
                    receiver,
                    receiver_value,
                    EXIT_GENERIC_VIRTUAL_CALL,
                    &stack,
                )?
            }
            SegmentExit::InterfaceCall { .. } => {
                let receiver_value = stack
                    .get(
                        stack
                            .len()
                            .checked_sub(contract.params.len())
                            .ok_or(CompileError::Backend)?,
                    )
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let receiver =
                    emit_interface_receiver_key(builder, values, receiver_value, point, &stack)?;
                emit_resolved_call_lookup(
                    builder,
                    values,
                    input.root.function,
                    point,
                    receiver,
                    receiver_value,
                    EXIT_INTERFACE_CALL,
                    &stack,
                )?
            }
            SegmentExit::SlotCall {
                slot,
                application,
                constructor,
                ..
            } => {
                let point = FaultPoint {
                    block: segment.block,
                    instruction: call_instruction,
                    prefix: 0,
                };
                let (function, fault) =
                    emit_image_slot_call_target(builder, values, slot, constructor)?;
                let environment = if let Some(application) = application {
                    let site = type_environment_sites
                        .iter()
                        .find(|site| {
                            site.block == segment.block
                                && site.instruction == call_instruction
                                && site.application == application
                        })
                        .ok_or(CompileError::Backend)?;
                    emit_type_environment_lookup(builder, values, site, point, &stack)?
                } else {
                    builder.ins().iconst(types::I32, 0)
                };
                NativeCallTarget {
                    function,
                    environment,
                    capture_data: builder.ins().iconst(values.pointer_type, 0),
                    capture_len: builder.ins().iconst(values.pointer_type, 0),
                    fault: Some(fault),
                }
            }
            _ => return Err(CompileError::Backend),
        };
        emit_native_call(
            builder,
            values,
            &mut stack,
            NativeCallEmission {
                target,
                capture,
                fallback: if capture.is_some() {
                    NativeCallFallback::Replay
                } else {
                    NativeCallFallback::Direct
                },
                contract,
                local_kinds: &plan.local_kinds,
                boundary_kinds: &segment.boundary_stack,
                block: segment.block,
                instruction: call_instruction,
                successor_entry: u32::try_from(segment.successors[0])
                    .map_err(|_| CompileError::Backend)?,
                successor: successor_blocks[0],
            },
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Effect { .. }) {
        let effect_instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_EFFECT,
                block: segment.block,
                instruction: effect_instruction,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Boundary { .. }) {
        let instruction = segment.end - 1;
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_BOUNDARY,
                block: segment.block,
                instruction,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Unreachable) {
        emit_segment_charge(builder, values, fast_segment_cost);
        let retired = emit_retired(builder, values);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_UNREACHABLE,
                block: segment.block,
                instruction: segment.end,
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    match segment.exit {
        SegmentExit::Continue { .. } => {
            emit_segment_charge(builder, values, fast_segment_cost);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Jump { .. } => {
            let carries = segment
                .carry_reserved_cost
                .first()
                .copied()
                .ok_or(CompileError::Backend)?;
            if !carries {
                emit_charge(builder, values, fast_segment_cost);
            }
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Conditional { jump_on_true, .. } => {
            if segment.carry_reserved_cost.len() != 2 {
                return Err(CompileError::Backend);
            }
            let condition = pop_native(&mut stack)?;
            define_stack(builder, values, &stack)?;
            let condition = builder.ins().icmp_imm(IntCC::NotEqual, condition, 0);
            let mut target = successor_blocks[0];
            let mut fallthrough = successor_blocks[1];
            let mut charged_target = None;
            let mut charged_fallthrough = None;
            let carries_target = segment.carry_reserved_cost[0];
            let carries_fallthrough = segment.carry_reserved_cost[1];
            match (carries_target, carries_fallthrough) {
                (false, false) => emit_charge(builder, values, fast_segment_cost),
                (true, true) => {}
                (false, true) => {
                    let block = builder.create_block();
                    target = block;
                    charged_target = Some(block);
                }
                (true, false) => {
                    let block = builder.create_block();
                    fallthrough = block;
                    charged_fallthrough = Some(block);
                }
            }
            if jump_on_true {
                builder.ins().brif(condition, target, &[], fallthrough, &[]);
            } else {
                builder.ins().brif(condition, fallthrough, &[], target, &[]);
            }
            if let Some(block) = charged_target {
                builder.switch_to_block(block);
                emit_charge(builder, values, fast_segment_cost);
                builder.ins().jump(successor_blocks[0], &[]);
            }
            if let Some(block) = charged_fallthrough {
                builder.switch_to_block(block);
                emit_charge(builder, values, fast_segment_cost);
                builder.ins().jump(successor_blocks[1], &[]);
            }
        }
        SegmentExit::Call { .. } => unreachable!(),
        SegmentExit::VirtualCall { .. } => unreachable!(),
        SegmentExit::ValueCall { .. } => unreachable!(),
        SegmentExit::GenericVirtualCall { .. } => unreachable!(),
        SegmentExit::InterfaceCall { .. } => unreachable!(),
        SegmentExit::SlotCall { .. } => unreachable!(),
        SegmentExit::Allocation { .. } => {
            emit_segment_charge(builder, values, fast_segment_cost);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(successor_blocks[0], &[]);
        }
        SegmentExit::Effect { .. } => unreachable!(),
        SegmentExit::Boundary { .. } => unreachable!(),
        SegmentExit::Unreachable => unreachable!(),
        SegmentExit::Return => {
            emit_segment_charge(builder, values, fast_segment_cost);
            let result = pop_value(&mut stack)?;
            if let Some(target) = values.inline_return {
                if !stack.is_empty() {
                    return Err(CompileError::Backend);
                }
                builder
                    .ins()
                    .jump(target, &[result.bits.into(), result.tag.into()]);
                return Ok(());
            }
            for (slot, pending) in virtual_locals.iter().copied().enumerate() {
                if pending {
                    let local = builder.use_var(values.locals[slot]);
                    emit_release_pending_instance(builder, values, local)?;
                }
            }
            emit_function_return(builder, values, segment.block, segment.end, result, &stack)?;
        }
    }
    Ok(())
}
