//! Virtual-object, liveness, and dirty-state analysis.

use super::*;

pub(super) fn stacks_use_equal_representations(left: &[ScalarKind], right: &[ScalarKind]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .copied()
            .zip(right.iter().copied())
            .all(|(left, right)| uses_equal_representation(left, right))
}

pub(super) fn uses_equal_representation(left: ScalarKind, right: ScalarKind) -> bool {
    left == right
        || matches!(
            (left, right),
            (ScalarKind::Object(_), ScalarKind::Object(_))
                | (ScalarKind::Tagged(_), ScalarKind::Tagged(_))
                | (ScalarKind::Callback(_), ScalarKind::Callback(_))
        )
}

#[derive(Clone)]
pub(super) struct VirtualFlowState {
    locals: Vec<bool>,
    stack: Vec<bool>,
}

pub(super) fn select_virtual_results(
    input: &FunctionInput<'_>,
    runtime: &Func,
    source: &Module,
    source_func: &Func,
    segments: &mut [Segment],
    local_count: usize,
    constructor: Option<VirtualConstructor>,
) -> Result<Vec<ScalarInstance>, UnsupportedReason> {
    let candidates: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter_map(|(index, segment)| {
            segment
                .call_contract
                .as_ref()
                .is_some_and(|contract| contract.virtual_result)
                .then_some(index)
        })
        .collect();
    let mut selected = Vec::new();
    selected
        .try_reserve_exact(candidates.len())
        .map_err(|_| UnsupportedReason::RegionLimit)?;
    for candidate in candidates {
        if virtual_result_dies_locally(
            runtime,
            source,
            source_func,
            segments,
            local_count,
            constructor,
            candidate,
        )? {
            selected.push(candidate);
        }
    }
    for segment in segments.iter_mut() {
        if let Some(contract) = &mut segment.call_contract {
            contract.virtual_result = false;
            contract.scalar_result = None;
        }
    }
    let mut scalar_instances = Vec::new();
    for candidate in selected {
        let scalar = match segments[candidate].exit {
            SegmentExit::Call {
                target, app: None, ..
            } if scalar_result_is_local(runtime, segments, candidate) => {
                scalar_constructor_summary(input, target)?
            }
            _ => None,
        };
        let contract = segments[candidate]
            .call_contract
            .as_mut()
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        contract.virtual_result = true;
        if let Some(mut scalar) = scalar {
            let site = u32::try_from(scalar_instances.len())
                .map_err(|_| UnsupportedReason::RegionLimit)?;
            if site < crate::activation::SCALAR_INSTANCE_COUNT as u32 {
                scalar.site = site;
                scalar_instances.push(ScalarInstance {
                    class: scalar.class,
                    field_count: u32::try_from(scalar.fields.len())
                        .map_err(|_| UnsupportedReason::RegionLimit)?,
                    frozen: scalar.frozen,
                });
                contract.scalar_result = Some(scalar);
            }
        }
    }
    Ok(scalar_instances)
}

pub(super) fn scalar_result_is_local(
    runtime: &Func,
    segments: &[Segment],
    candidate: usize,
) -> bool {
    let Some(successor) = segments[candidate].successors.first().copied() else {
        return false;
    };
    let Some(segment) = segments.get(successor) else {
        return false;
    };
    let Some(code) = runtime
        .blocks
        .get(segment.block as usize)
        .and_then(|block| block.get(segment.start as usize..segment.end as usize))
    else {
        return false;
    };
    let Some(Instr::StoreLocal(local)) = code.first().copied() else {
        return false;
    };
    if segment.successors.iter().any(|successor| {
        segments
            .get(*successor)
            .and_then(|next| next.live_in.get(local as usize))
            .copied()
            .unwrap_or(true)
    }) {
        return false;
    }
    let mut field_reads = 0usize;
    for (index, instruction) in code.iter().copied().enumerate().skip(1) {
        match instruction {
            Instr::LoadLocal(slot) if slot == local => {
                if !matches!(code.get(index + 1), Some(Instr::LoadField(_))) {
                    return false;
                }
                field_reads += 1;
            }
            Instr::StoreLocal(slot) if slot == local => return false,
            _ => {}
        }
    }
    if field_reads == 0 {
        return false;
    }
    code.iter().all(scalar_projection_instruction)
}

pub(super) fn scalar_projection_instruction(instruction: &Instr) -> bool {
    matches!(
        crate::instruction_treatment(instruction).class(),
        crate::TreatmentClass::Inline | crate::TreatmentClass::Guarded
    ) && !matches!(
        instruction,
        Instr::StoreField(_) | Instr::New(_) | Instr::NewG { .. }
    )
}

pub(super) fn virtual_result_dies_locally(
    runtime: &Func,
    source: &Module,
    source_func: &Func,
    segments: &[Segment],
    local_count: usize,
    constructor: Option<VirtualConstructor>,
    candidate: usize,
) -> Result<bool, UnsupportedReason> {
    let Some(successor) = segments[candidate].successors.first().copied() else {
        return Ok(false);
    };
    let mut entries: Vec<Option<VirtualFlowState>> = segments.iter().map(|_| None).collect();
    let mut initial = VirtualFlowState {
        locals: vec![false; local_count],
        stack: vec![false; segments[successor].entry_stack.len()],
    };
    let Some(result) = initial.stack.last_mut() else {
        return Ok(false);
    };
    *result = true;
    entries[successor] = Some(initial);
    let mut work = VecDeque::from([successor]);
    let mut queued = vec![false; segments.len()];
    queued[successor] = true;
    while let Some(index) = work.pop_front() {
        queued[index] = false;
        let mut state = entries[index]
            .clone()
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        let segment = &segments[index];
        let runtime_code = runtime
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize..segment.end as usize))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        let source_code = source_func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize..segment.end as usize))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        if runtime_code.len() != source_code.len() {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        for (instruction, source_instruction) in runtime_code
            .iter()
            .copied()
            .zip(source_code.iter().copied())
        {
            let call = matches!(instruction, Instr::Call(_) | Instr::CallG { .. })
                .then_some(segment.call_contract.as_ref())
                .flatten();
            if virtual_barrier_required(instruction, call, false, constructor, &state, true) {
                return Ok(false);
            }
            if matches!(instruction, Instr::Call(_) | Instr::CallG { .. }) {
                let contract = call.ok_or(UnsupportedReason::InvalidControlFlow)?;
                if state.stack.len() < contract.params.len() {
                    return Err(UnsupportedReason::InvalidStack);
                }
                state
                    .stack
                    .truncate(state.stack.len() - contract.params.len());
                state.stack.push(false);
            } else {
                transfer_virtual_instruction(
                    source,
                    source_instruction,
                    instruction,
                    call,
                    false,
                    &mut state.locals,
                    &mut state.stack,
                )?;
            }
        }
        for successor in segment.successors.iter().copied() {
            let changed = merge_virtual_state(&mut entries[successor], &state)?;
            if changed && !queued[successor] {
                queued[successor] = true;
                work.push_back(successor);
            }
        }
    }
    Ok(true)
}

pub(super) fn compute_virtual_flow(
    runtime: &Func,
    source: &Module,
    source_func: &Func,
    segments: &mut [Segment],
    local_count: usize,
    constructor: Option<VirtualConstructor>,
    virtual_parameters: &[bool],
) -> Result<(), UnsupportedReason> {
    for segment in segments.iter_mut() {
        segment.virtual_locals_in = vec![false; local_count];
        segment.virtual_stack_in = vec![false; segment.entry_stack.len()];
        segment.virtual_barriers.clear();
    }
    let entry = segments
        .iter_mut()
        .find(|segment| segment.block == 0 && segment.start == 0)
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    if virtual_parameters.len() > entry.virtual_locals_in.len() {
        return Err(UnsupportedReason::InvalidControlFlow);
    }
    entry.virtual_locals_in[..virtual_parameters.len()].copy_from_slice(virtual_parameters);
    let mut work: VecDeque<usize> = (0..segments.len()).collect();
    let mut queued = vec![true; segments.len()];
    while let Some(index) = work.pop_front() {
        queued[index] = false;
        let segment = &segments[index];
        let mut state = VirtualFlowState {
            locals: segment.virtual_locals_in.clone(),
            stack: segment.virtual_stack_in.clone(),
        };
        let mut barriers = Vec::new();
        let runtime_code = runtime
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize..segment.end as usize))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        let source_code = source_func
            .blocks
            .get(segment.block as usize)
            .and_then(|block| block.get(segment.start as usize..segment.end as usize))
            .ok_or(UnsupportedReason::InvalidControlFlow)?;
        if runtime_code.len() != source_code.len() {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        for (offset, (instruction, source_instruction)) in runtime_code
            .iter()
            .copied()
            .zip(source_code.iter().copied())
            .enumerate()
        {
            let position = segment
                .start
                .checked_add(u32::try_from(offset).map_err(|_| UnsupportedReason::RegionLimit)?)
                .ok_or(UnsupportedReason::RegionLimit)?;
            let call = matches!(instruction, Instr::Call(_) | Instr::CallG { .. })
                .then_some(segment.call_contract.as_ref())
                .flatten();
            let virtual_new = constructor.is_some_and(|constructor| match instruction {
                Instr::New(class) | Instr::NewG { class, .. } => class == constructor.class,
                _ => false,
            });
            if virtual_barrier_required(instruction, call, virtual_new, constructor, &state, false)
            {
                barriers.push(position);
                state.locals.fill(false);
                state.stack.fill(false);
            }
            transfer_virtual_instruction(
                source,
                source_instruction,
                instruction,
                call,
                virtual_new,
                &mut state.locals,
                &mut state.stack,
            )?;
        }
        if barriers.iter().any(|barrier| {
            !segments[index]
                .virtual_barriers
                .iter()
                .any(|known| known == barrier)
        }) {
            segments[index].virtual_barriers.extend(barriers);
            segments[index].virtual_barriers.sort_unstable();
            segments[index].virtual_barriers.dedup();
        }
        let successors = segments[index].successors.clone();
        for successor in successors {
            let mut known = Some(VirtualFlowState {
                locals: std::mem::take(&mut segments[successor].virtual_locals_in),
                stack: std::mem::take(&mut segments[successor].virtual_stack_in),
            });
            let changed = merge_virtual_state(&mut known, &state)?;
            let known = known.ok_or(UnsupportedReason::InvalidControlFlow)?;
            segments[successor].virtual_locals_in = known.locals;
            segments[successor].virtual_stack_in = known.stack;
            if changed && !queued[successor] {
                queued[successor] = true;
                work.push_back(successor);
            }
        }
    }
    Ok(())
}

pub(super) fn virtual_barrier_required(
    instruction: Instr,
    call: Option<&CallContract>,
    virtual_new: bool,
    constructor: Option<VirtualConstructor>,
    state: &VirtualFlowState,
    exit_materializes: bool,
) -> bool {
    let any_pending = state.locals.iter().chain(&state.stack).any(|value| *value);
    let blocked_argument = call.is_some_and(|contract| {
        state
            .stack
            .get(state.stack.len().saturating_sub(contract.params.len())..)
            .is_none_or(|arguments| {
                arguments
                    .iter()
                    .copied()
                    .zip(contract.virtual_params.iter().copied())
                    .any(|(pending, accepted)| pending && !accepted)
            })
    });
    let stored_pending =
        matches!(instruction, Instr::StoreField(_)) && state.stack.last().copied().unwrap_or(false);
    let wrapped_pending = matches!(
        instruction,
        Instr::Extended(ExtendedInstr::OptionSome { .. })
    ) && state.stack.last().copied().unwrap_or(false);
    let returned_pending = matches!(instruction, Instr::Return)
        && constructor.is_none()
        && state.stack.last().copied().unwrap_or(false);
    let class_barrier = match crate::instruction_treatment(&instruction).class() {
        crate::TreatmentClass::Helper => any_pending,
        crate::TreatmentClass::FastPath => any_pending && !virtual_new,
        crate::TreatmentClass::Call => {
            if call.is_some_and(|contract| {
                contract.virtual_result || contract.virtual_params.iter().any(|accepted| *accepted)
            }) {
                blocked_argument
            } else {
                any_pending
            }
        }
        crate::TreatmentClass::Exit => exit_materializes && any_pending,
        crate::TreatmentClass::Inline | crate::TreatmentClass::Guarded => false,
    };
    class_barrier || stored_pending || wrapped_pending || returned_pending
}

pub(super) fn merge_virtual_state(
    known: &mut Option<VirtualFlowState>,
    incoming: &VirtualFlowState,
) -> Result<bool, UnsupportedReason> {
    let Some(known) = known else {
        *known = Some(incoming.clone());
        return Ok(true);
    };
    if known.locals.len() != incoming.locals.len() || known.stack.len() != incoming.stack.len() {
        return Err(UnsupportedReason::InvalidControlFlow);
    }
    let mut changed = false;
    for (known, incoming) in known.locals.iter_mut().zip(incoming.locals.iter().copied()) {
        let merged = *known || incoming;
        changed |= merged != *known;
        *known = merged;
    }
    for (known, incoming) in known.stack.iter_mut().zip(incoming.stack.iter().copied()) {
        let merged = *known || incoming;
        changed |= merged != *known;
        *known = merged;
    }
    Ok(changed)
}

pub(crate) fn transfer_virtual_instruction(
    source: &Module,
    source_instruction: Instr,
    instruction: Instr,
    call: Option<&CallContract>,
    virtual_new: bool,
    locals: &mut [bool],
    stack: &mut Vec<bool>,
) -> Result<(), UnsupportedReason> {
    match instruction {
        Instr::LoadLocal(slot) => {
            let value = locals
                .get(slot as usize)
                .copied()
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            stack.push(value);
        }
        Instr::StoreLocal(slot) => {
            let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
            let local = locals
                .get_mut(slot as usize)
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            *local = value;
        }
        Instr::Pop => {
            stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
        }
        Instr::LoadField(_) | Instr::IsType(_) => {
            stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
            stack.push(false);
        }
        Instr::StoreField(_) => {
            if stack.len() < 2 {
                return Err(UnsupportedReason::InvalidStack);
            }
            stack.truncate(stack.len() - 2);
        }
        Instr::CastType(_) | Instr::Extended(ExtendedInstr::SealInstance) => {
            let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
            stack.push(value);
        }
        Instr::EqRef | Instr::NeRef => {
            if stack.len() < 2 {
                return Err(UnsupportedReason::InvalidStack);
            }
            stack.truncate(stack.len() - 2);
            stack.push(false);
        }
        Instr::New(_) | Instr::NewG { .. } => stack.push(virtual_new),
        Instr::Call(_) | Instr::CallG { .. } => {
            let contract = call.ok_or(UnsupportedReason::InvalidControlFlow)?;
            if stack.len() < contract.params.len() {
                return Err(UnsupportedReason::InvalidStack);
            }
            stack.truncate(stack.len() - contract.params.len());
            stack.push(contract.virtual_result);
        }
        Instr::Return => {
            stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
        }
        _ => {
            let (pops, pushes) = lm_bytecode::stack_effect(source, &source_instruction);
            if stack.len() < pops {
                return Err(UnsupportedReason::InvalidStack);
            }
            stack.truncate(stack.len() - pops);
            stack.extend(std::iter::repeat_n(false, pushes));
        }
    }
    Ok(())
}

pub(crate) fn compute_liveness(segments: &mut [Segment], locals: usize) {
    for segment in segments.iter_mut() {
        segment.live_in = vec![false; locals];
    }
    loop {
        let previous: Vec<Vec<bool>> = segments
            .iter()
            .map(|segment| segment.live_in.clone())
            .collect();
        let mut changed = false;
        for index in (0..segments.len()).rev() {
            let mut live_out = vec![false; locals];
            for successor in segments[index].successors.iter().copied() {
                for (slot, live) in previous[successor].iter().copied().enumerate() {
                    live_out[slot] |= live;
                }
            }
            let next: Vec<bool> = (0..locals)
                .map(|slot| {
                    segments[index].uses[slot]
                        || (live_out[slot] && !segments[index].definitions[slot])
                })
                .collect();
            changed |= next != segments[index].live_in;
            segments[index].live_in = next;
        }
        if !changed {
            break;
        }
    }
}

pub(crate) fn compute_dirty_locals(segments: &mut [Segment], locals: usize) {
    let mut work = VecDeque::new();
    for (segment_index, segment) in segments.iter_mut().enumerate() {
        debug_assert_eq!(segment.definitions.len(), locals);
        segment.dirty_locals = segment.definitions.clone();
        for (slot, dirty) in segment.dirty_locals.iter().copied().enumerate() {
            if dirty {
                work.push_back((segment_index, slot));
            }
        }
    }

    while let Some((segment_index, slot)) = work.pop_front() {
        let successor_count = segments[segment_index].successors.len();
        for successor_index in 0..successor_count {
            let successor = segments[segment_index].successors[successor_index];
            if !segments[successor].dirty_locals[slot] {
                segments[successor].dirty_locals[slot] = true;
                work.push_back((successor, slot));
            }
        }
    }
}
