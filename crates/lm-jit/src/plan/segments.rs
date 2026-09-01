//! Segment construction and instruction analysis.

use super::*;

pub(crate) fn split_segments(func: &Func) -> Result<Vec<Segment>, UnsupportedReason> {
    let mut segments = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        let mut start = 0usize;
        for (instruction_index, instruction) in block.iter().enumerate() {
            if requires_retry_entry(instruction) && start < instruction_index {
                segments.push(empty_segment(
                    block_index,
                    start,
                    instruction_index,
                    SegmentExit::Continue {
                        fallthrough_ip: instruction_index as u32,
                    },
                ));
                start = instruction_index;
            }
            let exit = segment_exit(instruction, instruction_index, block.len())?;
            let exit = match exit {
                Some(exit) => exit,
                None if crate::instruction_treatment(instruction).is_replay_barrier() => {
                    SegmentExit::Continue {
                        fallthrough_ip: instruction_index as u32 + 1,
                    }
                }
                None => continue,
            };
            segments.push(empty_segment(
                block_index,
                start,
                instruction_index + 1,
                exit,
            ));
            start = instruction_index + 1;
        }
        if start != block.len() {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
    }
    for segment in &mut segments {
        segment.retry_entry = func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize))
            .is_some_and(requires_retry_entry);
    }
    Ok(segments)
}

pub(super) fn empty_segment(block: usize, start: usize, end: usize, exit: SegmentExit) -> Segment {
    Segment {
        block: block as u32,
        start: start as u32,
        end: end as u32,
        cost: end as u32 - start as u32,
        fuel_reserve: 0,
        reserved_prefix_cost: 0,
        carry_reserved_cost: Vec::new(),
        carries_reserved_prefix: false,
        retry_entry: false,
        defer_integer_overflow: false,
        exit,
        uses: Vec::new(),
        definitions: Vec::new(),
        successors: Vec::new(),
        live_in: Vec::new(),
        dirty_locals: Vec::new(),
        entry_stack: Vec::new(),
        virtual_locals_in: Vec::new(),
        virtual_stack_in: Vec::new(),
        virtual_barriers: Vec::new(),
        call_contract: None,
        exit_stack: Vec::new(),
        boundary_stack: Vec::new(),
        heap_accesses: Vec::new(),
        option_accesses: Vec::new(),
        fuel_stacks: Vec::new(),
        replay_stacks: Vec::new(),
        fault_stacks: Vec::new(),
        allocations: Vec::new(),
    }
}

pub(super) fn requires_retry_entry(instruction: &Instr) -> bool {
    matches!(
        crate::instruction_treatment(instruction).exit(),
        crate::ExitBehavior::Call
    ) || matches!(
        instruction,
        Instr::NewG { .. }
            | Instr::IsType(_)
            | Instr::CastType(_)
            | Instr::MapPut { .. }
            | Instr::Extended(
                ExtendedInstr::OptionNone { .. }
                    | ExtendedInstr::OptionPayload { .. }
                    | ExtendedInstr::ListGet { .. }
                    | ExtendedInstr::ListPop { .. }
                    | ExtendedInstr::MapGet { .. }
                    | ExtendedInstr::MapRemove { .. }
            )
    )
}

pub(super) fn can_defer_integer_overflow(func: &Func, segment: &Segment) -> bool {
    if segment
        .definitions
        .iter()
        .copied()
        .zip(segment.live_in.iter().copied())
        .any(|(defined, live)| defined && !live)
    {
        return false;
    }
    let Some(code) = func
        .blocks
        .get(segment.block as usize)
        .and_then(|block| block.get(segment.start as usize..segment.end as usize))
    else {
        return false;
    };
    let mut checked = 0usize;
    for instruction in code {
        match instruction {
            Instr::Add | Instr::Sub | Instr::Mul | Instr::Neg => checked += 1,
            Instr::ConstUnit
            | Instr::ConstBool(_)
            | Instr::ConstInt(_)
            | Instr::ConstFloat(_)
            | Instr::ConstChar(_)
            | Instr::LoadLocal(_)
            | Instr::StoreLocal(_)
            | Instr::Pop
            | Instr::Not
            | Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt
            | Instr::EqBool
            | Instr::NeBool
            | Instr::EqRef
            | Instr::NeRef
            | Instr::OpConst(_)
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Return => {}
            _ => return false,
        }
    }
    checked > 1
}

pub(crate) fn bypasses_fuel_check(segments: &[Segment], index: usize, successor: usize) -> bool {
    let segment = &segments[index];
    let scalar_call = segment
        .call_contract
        .as_ref()
        .is_some_and(|contract| contract.scalar_result.is_some())
        && segment.successors.first() == Some(&successor);
    !segments[successor].retry_entry
        && ((successor > index
            && matches!(
                segment.exit,
                SegmentExit::Jump { .. } | SegmentExit::Conditional { .. }
            ))
            || scalar_call)
}

pub(super) fn compute_fuel_reserves(segments: &mut [Segment]) -> Result<(), UnsupportedReason> {
    for index in (0..segments.len()).rev() {
        let tail = segments[index]
            .successors
            .iter()
            .copied()
            .filter(|successor| bypasses_fuel_check(segments, index, *successor))
            .map(|successor| segments[successor].fuel_reserve)
            .max()
            .unwrap_or(0);
        let scalar_cost = segments[index]
            .call_contract
            .as_ref()
            .and_then(|contract| contract.scalar_result.as_ref())
            .map_or(0, |scalar| scalar.retired_cost.saturating_add(1));
        segments[index].fuel_reserve = segments[index]
            .cost
            .checked_add(scalar_cost)
            .ok_or(UnsupportedReason::RegionLimit)?
            .checked_add(tail)
            .ok_or(UnsupportedReason::RegionLimit)?;
    }
    Ok(())
}

pub(super) fn compute_reserved_costs(segments: &mut [Segment]) -> Result<(), UnsupportedReason> {
    let mut bypass_predecessors = vec![0usize; segments.len()];
    let mut other_predecessors = vec![false; segments.len()];
    for (index, segment) in segments.iter().enumerate() {
        for successor in segment.successors.iter().copied() {
            let count = bypass_predecessors
                .get_mut(successor)
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            if bypasses_fuel_check(segments, index, successor) {
                *count = count.checked_add(1).ok_or(UnsupportedReason::RegionLimit)?;
            } else {
                other_predecessors[successor] = true;
            }
        }
    }

    let carries_into: Vec<bool> = (0..segments.len())
        .map(|index| index != 0 && bypass_predecessors[index] == 1 && !other_predecessors[index])
        .collect();
    for (index, segment) in segments.iter_mut().enumerate() {
        segment.carries_reserved_prefix = carries_into[index];
        segment.carry_reserved_cost = vec![false; segment.successors.len()];
    }

    for index in 0..segments.len() {
        let pending = segments[index]
            .reserved_prefix_cost
            .checked_add(segments[index].cost)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let successors = segments[index].successors.clone();
        for (edge, successor) in successors.into_iter().enumerate() {
            let carries =
                bypasses_fuel_check(segments, index, successor) && carries_into[successor];
            segments[index].carry_reserved_cost[edge] = carries;
            if carries {
                segments[successor].reserved_prefix_cost = pending;
            }
        }
    }
    Ok(())
}

pub(super) fn segment_exit(
    instruction: &Instr,
    instruction_index: usize,
    block_len: usize,
) -> Result<Option<SegmentExit>, UnsupportedReason> {
    let next = instruction_index as u32 + 1;
    let treatment = crate::instruction_treatment(instruction);
    Ok(match treatment.exit() {
        crate::ExitBehavior::Continue => None,
        crate::ExitBehavior::Branch => Some(match instruction {
            Instr::Jump(target) => SegmentExit::Jump {
                target_block: *target,
            },
            Instr::JumpIfFalse(target) => SegmentExit::Conditional {
                target_block: *target,
                jump_on_true: false,
                fallthrough_ip: next,
            },
            Instr::JumpIfTrue(target) => SegmentExit::Conditional {
                target_block: *target,
                jump_on_true: true,
                fallthrough_ip: next,
            },
            _ => return Err(UnsupportedReason::InvalidControlFlow),
        }),
        crate::ExitBehavior::Call => Some(match instruction {
            Instr::Call(target) => SegmentExit::Call {
                target: *target,
                app: None,
                fallthrough_ip: next,
            },
            Instr::CallG { func, app } => SegmentExit::Call {
                target: *func,
                app: Some(*app),
                fallthrough_ip: next,
            },
            Instr::CallVirtual { selector, .. } => SegmentExit::VirtualCall {
                selector: *selector,
                fallthrough_ip: next,
            },
            Instr::CallValue { .. } => SegmentExit::ValueCall {
                fallthrough_ip: next,
            },
            Instr::CallVirtualG { selector, app, .. } => SegmentExit::GenericVirtualCall {
                selector: *selector,
                application: *app,
                fallthrough_ip: next,
            },
            Instr::CallInterface { site, recv_ty, app } => {
                let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
                SegmentExit::InterfaceCall {
                    interface,
                    method,
                    recv_ty: *recv_ty,
                    app: *app,
                    fallthrough_ip: next,
                }
            }
            Instr::Extended(ExtendedInstr::CallSlot { slot, app }) => SegmentExit::SlotCall {
                slot: *slot,
                application: (*app != lm_bytecode::NO_APP).then_some(*app),
                constructor: false,
                fallthrough_ip: next,
            },
            Instr::Extended(ExtendedInstr::NewSlot { slot, app }) => SegmentExit::SlotCall {
                slot: *slot,
                application: (*app != lm_bytecode::NO_APP).then_some(*app),
                constructor: true,
                fallthrough_ip: next,
            },
            _ => return Err(UnsupportedReason::InvalidControlFlow),
        }),
        crate::ExitBehavior::Allocation => Some(SegmentExit::Allocation {
            fallthrough_ip: next,
        }),
        crate::ExitBehavior::Effect => Some(SegmentExit::Effect {
            fallthrough_ip: next,
        }),
        crate::ExitBehavior::Boundary => Some(SegmentExit::Boundary {
            fallthrough_ip: (instruction_index + 1 < block_len).then_some(next),
        }),
        crate::ExitBehavior::Return => Some(SegmentExit::Return),
        crate::ExitBehavior::Fault => Some(match instruction {
            Instr::Unreachable => SegmentExit::Unreachable,
            _ => SegmentExit::Boundary {
                fallthrough_ip: None,
            },
        }),
    })
}

pub(super) fn resolve_successors(
    segments: &mut [Segment],
    entries: &std::collections::HashMap<(u32, u32), usize>,
) -> Result<(), UnsupportedReason> {
    for segment in segments {
        segment.successors = match segment.exit {
            SegmentExit::Continue { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Jump { target_block } => vec![entry(entries, target_block, 0)?],
            SegmentExit::Conditional {
                target_block,
                fallthrough_ip,
                ..
            } => vec![
                entry(entries, target_block, 0)?,
                entry(entries, segment.block, fallthrough_ip)?,
            ],
            SegmentExit::Call { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::VirtualCall { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::ValueCall { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::GenericVirtualCall { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::InterfaceCall { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::SlotCall { fallthrough_ip, .. } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Allocation { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Effect { fallthrough_ip } => {
                vec![entry(entries, segment.block, fallthrough_ip)?]
            }
            SegmentExit::Boundary {
                fallthrough_ip: Some(fallthrough_ip),
            } => vec![entry(entries, segment.block, fallthrough_ip)?],
            SegmentExit::Boundary {
                fallthrough_ip: None,
            } => Vec::new(),
            SegmentExit::Return | SegmentExit::Unreachable => Vec::new(),
        };
    }
    Ok(())
}

pub(super) fn entry(
    entries: &std::collections::HashMap<(u32, u32), usize>,
    block: u32,
    instruction: u32,
) -> Result<usize, UnsupportedReason> {
    entries
        .get(&(block, instruction))
        .copied()
        .ok_or(UnsupportedReason::InvalidControlFlow)
}

pub(super) fn analyze_segment(
    context: &SegmentAnalysisContext<'_>,
    segment: &Segment,
    verified_points: &HashMap<(u32, u32), VerifiedPoint>,
) -> Result<SegmentAnalysis, UnsupportedReason> {
    let mut max_stack = 0;
    let mut max_stack_values = 0;
    let mut boundary_stack = Vec::new();
    let mut heap_accesses = Vec::new();
    let mut option_accesses = Vec::new();
    let mut fuel_stacks = Vec::new();
    let mut replay_stacks = Vec::new();
    let mut fault_stacks = Vec::new();
    let mut allocations = Vec::new();
    let mut call_contract = None;
    let mut uses = vec![false; context.locals.len()];
    let mut definitions = vec![false; context.locals.len()];
    for (offset, instruction) in context.func.blocks[segment.block as usize]
        [segment.start as usize..segment.end as usize]
        .iter()
        .enumerate()
    {
        let position = segment
            .start
            .checked_add(u32::try_from(offset).map_err(|_| UnsupportedReason::RegionLimit)?)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let next = position
            .checked_add(1)
            .ok_or(UnsupportedReason::RegionLimit)?;
        let before = verified_points
            .get(&(segment.block, position))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        let after = verified_points
            .get(&(segment.block, next))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        fuel_stacks.push((position, before.stack.clone()));

        let treatment = crate::instruction_treatment(instruction);
        if treatment.replays() {
            replay_stacks.push((position, before.stack.clone()));
        }
        match treatment.fault_stack() {
            crate::FaultStack::None => {}
            crate::FaultStack::Before => {
                fault_stacks.push((next, before.stack.clone()));
            }
            crate::FaultStack::Pop(count) => {
                let length = before
                    .stack
                    .len()
                    .checked_sub(usize::from(count))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                fault_stacks.push((next, before.stack[..length].to_vec()));
            }
        }

        let source_instruction = context
            .source_func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(position as usize))
            .copied()
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        match *instruction {
            Instr::LoadLocal(slot) => {
                let at = slot as usize;
                if context.locals.get(at).is_none() {
                    return Err(UnsupportedReason::InvalidControlFlow);
                }
                if !before.initialized.get(at).copied().unwrap_or(false) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                if !definitions[at] {
                    uses[at] = true;
                }
            }
            Instr::StoreLocal(slot) => {
                let at = slot as usize;
                context
                    .locals
                    .get(at)
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                definitions[at] = true;
            }
            Instr::LoadCapture(index) => {
                let ty = context
                    .source_func
                    .captures
                    .get(index as usize)
                    .copied()
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::LoadCapture {
                        value: value_contract(context, ty)?,
                    },
                });
            }
            Instr::LoadField(field) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let (receiver_class, value) = field_contract(context, receiver, field)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::LoadField {
                        receiver_class,
                        value,
                    },
                });
            }
            Instr::StoreField(field) => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 1)?;
                let (receiver_class, contract) = field_contract(context, receiver, field)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::StoreField {
                        receiver_class,
                        value: contract,
                    },
                });
            }
            Instr::TupleGet(index) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let value = tuple_element_contract(context, receiver, index)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::TupleGet { value },
                });
            }
            Instr::EqDigest | Instr::NeDigest => {
                let right = stack_from_end(&before.stack, 0)?;
                let left = stack_from_end(&before.stack, 1)?;
                digest_type(context, left)?;
                digest_type(context, right)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::DigestCompare,
                });
            }
            Instr::Extended(ExtendedInstr::AsCallback) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                function_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::AsCallback,
                });
            }
            Instr::Extended(ExtendedInstr::OptionNone { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionNone { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                option_argument_type(context, source_ty)?;
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::None,
                });
            }
            Instr::Extended(ExtendedInstr::OptionPayload { ty }) => {
                let Instr::Extended(ExtendedInstr::OptionPayload { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let payload = option_argument_type(context, source_ty)?;
                let value = value_contract(context, payload)?;
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::Payload { value },
                });
            }
            Instr::Extended(ExtendedInstr::ListGet { ty }) => {
                let Instr::Extended(ExtendedInstr::ListGet { ty: source_ty }) = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context, receiver)?;
                let option_element = option_argument_type(context, source_ty)?;
                let value = value_contract(context, element)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_element)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::ListGet { value },
                });
            }
            Instr::Extended(ExtendedInstr::ListPop { ty }) => {
                let Instr::Extended(ExtendedInstr::ListPop { ty: source_ty }) = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let receiver = stack_from_end(&before.stack, 0)?;
                let element = list_element_type(context, receiver)?;
                let option_element = option_argument_type(context, source_ty)?;
                let value = value_contract(context, element)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_element)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::ListPop { value },
                });
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let source_ty = match source_instruction {
                    Instr::IsType(ty) | Instr::CastType(ty) => ty,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                if let Some(target) = option_test_target(context.module, source_ty)? {
                    if !matches!(receiver, ScalarKind::Tagged(_)) {
                        return Err(UnsupportedReason::InvalidStack);
                    }
                    let kind = if matches!(instruction, Instr::IsType(_)) {
                        OptionAccessKind::IsType { target }
                    } else {
                        OptionAccessKind::CastType { target }
                    };
                    let family_type = match instruction {
                        Instr::IsType(ty) | Instr::CastType(ty) => ty,
                        _ => return Err(UnsupportedReason::InvalidControlFlow),
                    };
                    option_accesses.push(OptionAccess {
                        instruction: position,
                        family_type: *family_type,
                        kind,
                    });
                } else {
                    if !matches!(receiver, ScalarKind::Object(_)) {
                        return Err(UnsupportedReason::InvalidStack);
                    }
                    let target_class = class_test_target(context, source_ty)?;
                    let kind = if matches!(instruction, Instr::IsType(_)) {
                        HeapAccessKind::IsType { target_class }
                    } else {
                        HeapAccessKind::CastType { target_class }
                    };
                    heap_accesses.push(HeapAccess {
                        instruction: position,
                        kind,
                    });
                }
            }
            Instr::ListLen => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListLen,
                });
            }
            Instr::MapLen => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapLen,
                });
            }
            Instr::MapHas => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key, _) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapHas {
                        key: value_contract(context, key)?,
                    },
                });
            }
            Instr::MapAt => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key, value) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapAt {
                        key: value_contract(context, key)?,
                        value: value_contract(context, value)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapGet { ty }) => {
                let Instr::Extended(ExtendedInstr::MapGet { ty: source_ty }) = source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key_type, value_type) = map_type(context, receiver)?;
                let option_value = option_argument_type(context, source_ty)?;
                let value = value_contract(context, value_type)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_value)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::MapGet { value },
                });
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapGet {
                        key: value_contract(context, key_type)?,
                    },
                });
            }
            Instr::MapPut { ty, discard } => {
                let receiver = stack_from_end(&before.stack, 2)?;
                let (key_type, value_type) = map_type(context, receiver)?;
                let value = value_contract(context, value_type)?;
                let source_type = match source_instruction {
                    Instr::MapPut { ty, .. } => ty,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                let option_value = option_argument_type(context, source_type)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_value)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::MapPut { value, discard },
                });
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapPut {
                        key: value_contract(context, key_type)?,
                    },
                });
            }
            Instr::ListAt => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context, receiver)?;
                let value = value_contract(context, element)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListAt { value },
                });
            }
            Instr::Extended(ExtendedInstr::ListSet) => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 2)?;
                let element = list_element_type(context, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListSet { value: contract },
                });
            }
            Instr::Extended(ExtendedInstr::ListInsert) => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 2)?;
                let element = list_element_type(context, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListInsert { value: contract },
                });
            }
            Instr::Extended(
                operation @ (ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove),
            ) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListRemove {
                        value: value_contract(context, element)?,
                        swap: matches!(operation, ExtendedInstr::ListSwapRemove),
                    },
                });
            }
            Instr::Extended(ExtendedInstr::ListTruncate) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListTruncate,
                });
            }
            Instr::Extended(ExtendedInstr::ListCapacity) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListCapacity,
                });
            }
            Instr::Extended(ExtendedInstr::ListReserve) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListReserve,
                });
            }
            Instr::Extended(ExtendedInstr::ListReorder) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListReorder,
                });
            }
            Instr::Extended(ExtendedInstr::ListEpoch) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListEpoch,
                });
            }
            Instr::Extended(ExtendedInstr::ListIterLen) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                list_element_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListIterLen,
                });
            }
            Instr::Extended(ExtendedInstr::MapEpoch) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapEpoch,
                });
            }
            Instr::Extended(ExtendedInstr::MapIterLen) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapIterLen,
                });
            }
            Instr::Extended(ExtendedInstr::MapNextIndex) => {
                let receiver = stack_from_end(&before.stack, 2)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapNextIndex,
                });
            }
            Instr::Extended(ExtendedInstr::MapKeyAt) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key, _) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapKeyAt {
                        value: value_contract(context, key)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapValueAt) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (_, value) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapValueAt {
                        value: value_contract(context, value)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapRemove { ty }) => {
                let Instr::Extended(ExtendedInstr::MapRemove { ty: source_ty }) =
                    source_instruction
                else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key_type, value_type) = map_type(context, receiver)?;
                let option_value = option_argument_type(context, source_ty)?;
                let value = value_contract(context, value_type)?;
                if !uses_equal_representation(
                    value.kind,
                    scalar_kind(context.module, option_value)?,
                ) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                option_accesses.push(OptionAccess {
                    instruction: position,
                    family_type: ty,
                    kind: OptionAccessKind::MapRemove { value },
                });
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapRemove {
                        key: value_contract(context, key_type)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapClear) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapClear,
                });
            }
            Instr::Extended(ExtendedInstr::MapReserve) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapReserve,
                });
            }
            Instr::Extended(ExtendedInstr::MapProbe) => {
                let receiver = stack_from_end(&before.stack, 2)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapProbe,
                });
            }
            Instr::Extended(ExtendedInstr::MapProbeFound) => {}
            Instr::Extended(ExtendedInstr::MapProbeKey) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (key, _) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapProbeKey {
                        value: value_contract(context, key)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapProbeValue) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (_, value) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapProbeValue {
                        value: value_contract(context, value)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapProbeSetValue) => {
                let stored = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 2)?;
                let (_, value_type) = map_type(context, receiver)?;
                let value = value_contract(context, value_type)?;
                if !uses_equal_representation(stored, value.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapProbeSetValue { value },
                });
            }
            Instr::Extended(ExtendedInstr::MapProbeRemove) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                let (_, value) = map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapProbeRemove {
                        value: value_contract(context, value)?,
                    },
                });
            }
            Instr::Extended(ExtendedInstr::MapInsertHashed) => {
                let stored_key = stack_from_end(&before.stack, 3)?;
                let stored_value = stack_from_end(&before.stack, 2)?;
                let receiver = stack_from_end(&before.stack, 4)?;
                let (key_type, value_type) = map_type(context, receiver)?;
                let key = value_contract(context, key_type)?;
                let value = value_contract(context, value_type)?;
                if !uses_equal_representation(stored_key, key.kind)
                    || !uses_equal_representation(stored_value, value.kind)
                {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapInsertHashed { key, value },
                });
            }
            Instr::Extended(ExtendedInstr::MapWriteGuard) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                map_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::MapWriteGuard,
                });
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                let ScalarKind::Object(ty) = receiver else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                let contract = value_contract(context, ty)?;
                let Some(ObjectContract::Instance(class)) = contract.object else {
                    return Err(UnsupportedReason::InvalidStack);
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::SealInstance { class },
                });
            }
            Instr::Native(NativeInstr::BytesLen) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                bytes_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesLen,
                });
            }
            Instr::Native(NativeInstr::BytesAt) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                bytes_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesAt,
                });
            }
            Instr::Native(NativeInstr::BytesGet) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                bytes_type(context, receiver)?;
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::BytesGet,
                });
            }
            Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                text_type(context, receiver)?;
                let kind = if matches!(*instruction, Instr::Native(NativeInstr::StrByteLen)) {
                    HeapAccessKind::TextByteLen
                } else {
                    HeapAccessKind::TextScalarLen
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind,
                });
            }
            Instr::Native(
                NativeInstr::TextAtByte | NativeInstr::TextAt | NativeInstr::TextIsBoundary,
            ) => {
                let receiver = stack_from_end(&before.stack, 1)?;
                text_type(context, receiver)?;
                let kind = match instruction {
                    Instr::Native(NativeInstr::TextAtByte) => HeapAccessKind::TextAtByte,
                    Instr::Native(NativeInstr::TextAt) => HeapAccessKind::TextAt,
                    Instr::Native(NativeInstr::TextIsBoundary) => HeapAccessKind::TextIsBoundary,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind,
                });
            }
            Instr::Native(NativeInstr::SbNew | NativeInstr::BbNew) => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::SbBuild | NativeInstr::SbFinish | NativeInstr::BytesNew) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                if matches!(*instruction, Instr::Native(NativeInstr::BytesNew)) {
                    let ScalarKind::Object(ty) = receiver else {
                        return Err(UnsupportedReason::InvalidStack);
                    };
                    if !matches!(
                        value_contract(context, ty)?.object,
                        Some(ObjectContract::Str)
                    ) {
                        return Err(UnsupportedReason::InvalidStack);
                    }
                } else {
                    string_builder_type(context, receiver)?;
                }
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(
                NativeInstr::BbBuild
                | NativeInstr::BbFinish
                | NativeInstr::BytesCompact
                | NativeInstr::BytesTextView,
            )
            | Instr::Numeric(NumericInstr::BytesBitNot) => {
                let receiver = stack_from_end(&before.stack, 0)?;
                if matches!(
                    *instruction,
                    Instr::Native(NativeInstr::BbBuild | NativeInstr::BbFinish)
                ) {
                    byte_buffer_type(context, receiver)?;
                } else {
                    bytes_type(context, receiver)?;
                }
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::SbAppendStr) => {
                string_builder_type(context, stack_from_end(&before.stack, 1)?)?;
                text_type(context, stack_from_end(&before.stack, 0)?)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::SbAppendInt)
            | Instr::Native(NativeInstr::SbAppendBool)
            | Instr::Native(NativeInstr::SbAppendChar)
            | Instr::Numeric(NumericInstr::SbAppendFloat) => {
                string_builder_type(context, stack_from_end(&before.stack, 1)?)?;
                let expected = match instruction {
                    Instr::Native(NativeInstr::SbAppendInt) => ScalarKind::Int,
                    Instr::Native(NativeInstr::SbAppendBool) => ScalarKind::Bool,
                    Instr::Native(NativeInstr::SbAppendChar) => ScalarKind::Char,
                    Instr::Numeric(NumericInstr::SbAppendFloat) => ScalarKind::Float,
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                expect_scalar(stack_from_end(&before.stack, 0)?, expected)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::SbLen | NativeInstr::SbByteLen | NativeInstr::SbClear) => {
                string_builder_type(context, stack_from_end(&before.stack, 0)?)?;
            }
            Instr::Native(NativeInstr::BbAppend | NativeInstr::BbReserve) => {
                byte_buffer_type(context, stack_from_end(&before.stack, 1)?)?;
                expect_scalar(stack_from_end(&before.stack, 0)?, ScalarKind::Int)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::BbExtend) => {
                byte_buffer_type(context, stack_from_end(&before.stack, 1)?)?;
                bytes_type(context, stack_from_end(&before.stack, 0)?)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::BbLen | NativeInstr::BbClear) => {
                byte_buffer_type(context, stack_from_end(&before.stack, 0)?)?;
            }
            Instr::Native(NativeInstr::BbAt) => {
                byte_buffer_type(context, stack_from_end(&before.stack, 1)?)?;
                expect_scalar(stack_from_end(&before.stack, 0)?, ScalarKind::Int)?;
            }
            Instr::Native(NativeInstr::BytesSlice) => {
                bytes_type(context, stack_from_end(&before.stack, 2)?)?;
                expect_scalar(stack_from_end(&before.stack, 1)?, ScalarKind::Int)?;
                expect_scalar(stack_from_end(&before.stack, 0)?, ScalarKind::Int)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(NativeInstr::BytesConcat)
            | Instr::Numeric(
                NumericInstr::BytesBitAnd | NumericInstr::BytesBitOr | NumericInstr::BytesBitXor,
            ) => {
                bytes_type(context, stack_from_end(&before.stack, 1)?)?;
                bytes_type(context, stack_from_end(&before.stack, 0)?)?;
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Native(
                NativeInstr::StrConcat
                | NativeInstr::TextTrim
                | NativeInstr::TextTrimStart
                | NativeInstr::TextTrimEnd
                | NativeInstr::TextToLowerAscii
                | NativeInstr::TextToUpperAscii
                | NativeInstr::TextReplace
                | NativeInstr::TextPadStart
                | NativeInstr::TextPadEnd
                | NativeInstr::TextSplit
                | NativeInstr::TextLines
                | NativeInstr::TextSlice
                | NativeInstr::TextSliceBytes
                | NativeInstr::TextBytes
                | NativeInstr::TextToString
                | NativeInstr::BytesText
                | NativeInstr::BytesHex,
            )
            | Instr::Numeric(NumericInstr::FloatFixed) => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::ListPush => {
                let value = stack_from_end(&before.stack, 0)?;
                let receiver = stack_from_end(&before.stack, 1)?;
                let element = list_element_type(context, receiver)?;
                let contract = value_contract(context, element)?;
                if !uses_equal_representation(value, contract.kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                heap_accesses.push(HeapAccess {
                    instruction: position,
                    kind: HeapAccessKind::ListPush { value: contract },
                });
            }
            Instr::New(_)
            | Instr::NewG { .. }
            | Instr::MakeClosure { .. }
            | Instr::TupleNew { .. }
            | Instr::ListNew { .. }
            | Instr::MapNew { .. }
            | Instr::Digest { .. }
            | Instr::Extended(ExtendedInstr::MakeCallback { .. }) => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
                let captures = match instruction {
                    Instr::MakeClosure { captures, .. }
                    | Instr::Extended(ExtendedInstr::MakeCallback { captures, .. }) => {
                        Some(*captures)
                    }
                    Instr::TupleNew { count, .. } | Instr::ListNew { count, .. } => Some(*count),
                    Instr::MapNew { count, .. } => {
                        Some(count.checked_mul(2).ok_or(UnsupportedReason::RegionLimit)?)
                    }
                    Instr::Digest { .. } => Some(1),
                    _ => None,
                };
                if let Some(captures) = captures {
                    let length = before
                        .stack
                        .len()
                        .checked_sub(captures as usize)
                        .ok_or(UnsupportedReason::InvalidStack)?;
                    fault_stacks.push((next, before.stack[..length].to_vec()));
                }
            }
            Instr::FaultCode | Instr::FaultDenied => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Extended(ExtendedInstr::DynPack { .. }) => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Extended(
                ExtendedInstr::SyntaxTreeRoot
                | ExtendedInstr::SyntaxText
                | ExtendedInstr::SyntaxChildren
                | ExtendedInstr::SyntaxDetach
                | ExtendedInstr::SyntaxBuildToken
                | ExtendedInstr::SyntaxBuildTrivia
                | ExtendedInstr::SyntaxBuildNode
                | ExtendedInstr::SyntaxToTree,
            ) => {
                allocations.push(AllocationSite {
                    instruction: position,
                    stack: before.stack.clone(),
                });
            }
            Instr::Call(target) => {
                let signature = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                let contract = instantiate_call(signature, context.module, None)?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                if !uses_equal_representation(result, contract.result) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                boundary_stack = before.stack.clone();
                call_contract = Some(contract);
            }
            Instr::CallG { func: target, .. } => {
                let Instr::CallG { app, .. } = source_instruction else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let signature = context
                    .calls
                    .get(&target)
                    .ok_or(UnsupportedReason::MissingSource)?;
                let contract = instantiate_call(signature, context.module, Some(app))?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                if !uses_equal_representation(result, contract.result) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                boundary_stack = before.stack.clone();
                call_contract = Some(contract);
            }
            Instr::CallValue { argc } => {
                let parameter_count =
                    usize::try_from(argc).map_err(|_| UnsupportedReason::RegionLimit)?;
                let callee = before
                    .stack
                    .len()
                    .checked_sub(parameter_count.saturating_add(1))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let callee_type = before
                    .stack_types
                    .get(callee)
                    .and_then(|ty| context.verified_types.get(*ty as usize))
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let (params, result_type, value_target) = match callee_type {
                    BcType::Fn(params, _, result, _) => (params, *result, ValueCallTarget::Closure),
                    BcType::Callback(params, _, result, _) => {
                        (params, *result, ValueCallTarget::Callback)
                    }
                    _ => return Err(UnsupportedReason::InvalidStack),
                };
                if params.len() != parameter_count {
                    return Err(UnsupportedReason::InvalidStack);
                }
                let params = params
                    .iter()
                    .map(|ty| scalar_kind_in(context.module, context.verified_types, *ty))
                    .collect::<Result<Vec<_>, _>>()?;
                let result_kind =
                    scalar_kind_in(context.module, context.verified_types, result_type)?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                if !uses_equal_representation(result, result_kind) {
                    return Err(UnsupportedReason::InvalidStack);
                }
                boundary_stack = before.stack.clone();
                let mut stack_limit = before.stack.clone();
                stack_limit.remove(callee);
                fault_stacks.push((next, stack_limit));
                call_contract = Some(CallContract {
                    virtual_params: vec![false; params.len()],
                    params,
                    local_count: None,
                    result: result_kind,
                    receiver: None,
                    value_target: Some(value_target),
                    virtual_result: false,
                    scalar_result: None,
                });
            }
            Instr::CallVirtual { argc, .. } | Instr::CallVirtualG { argc, .. } => {
                let parameter_count = usize::try_from(argc)
                    .ok()
                    .and_then(|count| count.checked_add(1))
                    .ok_or(UnsupportedReason::RegionLimit)?;
                let parameter_start = before
                    .stack
                    .len()
                    .checked_sub(parameter_count)
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let params = before.stack[parameter_start..].to_vec();
                let receiver = params
                    .first()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                boundary_stack = before.stack.clone();
                call_contract = Some(CallContract {
                    virtual_params: vec![false; params.len()],
                    params,
                    local_count: None,
                    result,
                    receiver: Some(virtual_receiver(context, receiver)?),
                    value_target: None,
                    virtual_result: false,
                    scalar_result: None,
                });
            }
            Instr::CallInterface { .. } => {
                let Instr::CallInterface { site, .. } = source_instruction else {
                    return Err(UnsupportedReason::InvalidControlFlow);
                };
                let (interface, method) = lm_bytecode::unpack_interface_call_site(site);
                let parameter_count = context
                    .module
                    .interfaces
                    .get(interface as usize)
                    .and_then(|contract| contract.methods.get(method as usize))
                    .and_then(|requirement| requirement.params.len().checked_add(1))
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                let parameter_start = before
                    .stack
                    .len()
                    .checked_sub(parameter_count)
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let params = before.stack[parameter_start..].to_vec();
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                boundary_stack = before.stack.clone();
                call_contract = Some(CallContract {
                    virtual_params: vec![false; params.len()],
                    params,
                    local_count: None,
                    result,
                    receiver: None,
                    value_target: None,
                    virtual_result: false,
                    scalar_result: None,
                });
            }
            Instr::Extended(ExtendedInstr::CallSlot { .. } | ExtendedInstr::NewSlot { .. }) => {
                let (slot, constructor) = match source_instruction {
                    Instr::Extended(ExtendedInstr::CallSlot { slot, .. }) => (slot, false),
                    Instr::Extended(ExtendedInstr::NewSlot { slot, .. }) => (slot, true),
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                let spec = context
                    .module
                    .slots
                    .get(slot as usize)
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                let parameter_count = match (&spec.contract, constructor) {
                    (
                        lm_bytecode::SlotContract::Function(contract)
                        | lm_bytecode::SlotContract::Method(contract),
                        false,
                    ) => contract.params.len(),
                    (lm_bytecode::SlotContract::Class { constructor, .. }, true) => {
                        constructor.params.len()
                    }
                    _ => return Err(UnsupportedReason::InvalidControlFlow),
                };
                let parameter_start = before
                    .stack
                    .len()
                    .checked_sub(parameter_count)
                    .ok_or(UnsupportedReason::InvalidStack)?;
                let result = after
                    .stack
                    .last()
                    .copied()
                    .ok_or(UnsupportedReason::InvalidStack)?;
                boundary_stack = before.stack.clone();
                call_contract = Some(CallContract {
                    params: before.stack[parameter_start..].to_vec(),
                    virtual_params: vec![false; before.stack.len() - parameter_start],
                    local_count: None,
                    result,
                    receiver: None,
                    value_target: None,
                    virtual_result: false,
                    scalar_result: None,
                });
            }
            Instr::Perform { .. } | Instr::PerformValue { .. } => {
                boundary_stack = before.stack.clone();
            }
            Instr::TableEdit { .. }
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
            | Instr::Extended(ExtendedInstr::FaultTrace { .. }) => {
                boundary_stack = before.stack.clone();
            }
            _ => {}
        }
        max_stack = max_stack.max(before.stack.len()).max(after.stack.len());
        max_stack_values = max_stack_values
            .max(before.stack.len())
            .max(after.stack.len());
    }
    let exit_stack = verified_points
        .get(&(segment.block, segment.end))
        .ok_or(UnsupportedReason::InvalidControlFlow)?
        .stack
        .clone();
    Ok(SegmentAnalysis {
        uses,
        definitions,
        exit_stack,
        max_stack,
        max_stack_values,
        boundary_stack,
        heap_accesses,
        option_accesses,
        fuel_stacks,
        replay_stacks,
        fault_stacks,
        allocations,
        call_contract,
    })
}
