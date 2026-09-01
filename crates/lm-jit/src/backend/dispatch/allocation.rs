//! Instruction emission for one dispatch domain.

use super::*;

pub(super) fn emit(emission: &mut InstructionEmission<'_, '_, '_, '_>) -> Result<(), CompileError> {
    let instruction = emission.instruction;
    let builder = &mut *emission.builder;
    let values = emission.values;
    let plan = emission.plan;
    let input = emission.input;
    let segment = emission.segment;
    let type_environment_sites = emission.type_environment_sites;
    let stack = &mut *emission.stack;
    let within = emission.within;
    let fault_prefix = emission.fault_prefix;
    let prior_prefix = emission.prior_prefix;
    match instruction {
        Instr::MakeClosure { func, captures } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let capture_count = usize::try_from(captures).map_err(|_| CompileError::Backend)?;
            let stack_start = stack
                .len()
                .checked_sub(capture_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, capture_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                capture_count,
            )?;
            let frame = emit_current_frame_pointer(builder, values)?;
            let environment = load_cell_u32(
                builder,
                frame,
                std_mem::offset_of!(RawNativeFrame, environment),
            )?;
            let result = emit_capture_allocation(
                builder,
                values,
                CaptureAllocationEmission {
                    function: func,
                    environment,
                    capture_start,
                    capture_count,
                    roots: &roots,
                    callback: false,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::Extended(ExtendedInstr::MakeCallback { func, captures }) => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let capture_count = usize::try_from(captures).map_err(|_| CompileError::Backend)?;
            let stack_start = stack
                .len()
                .checked_sub(capture_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, capture_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                capture_count,
            )?;
            let frame = emit_current_frame_pointer(builder, values)?;
            let environment = load_cell_u32(
                builder,
                frame,
                std_mem::offset_of!(RawNativeFrame, environment),
            )?;
            let result = emit_capture_allocation(
                builder,
                values,
                CaptureAllocationEmission {
                    function: func,
                    environment,
                    capture_start,
                    capture_count,
                    roots: &roots,
                    callback: true,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::Callback as u64 as i64);
            stack.push(NativeValue { bits: result, tag });
        }
        Instr::TupleNew { count, .. }
        | Instr::ListNew { count, .. }
        | Instr::MapNew { count, .. } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let item_count = usize::try_from(count).map_err(|_| CompileError::Backend)?;
            let item_count = if matches!(instruction, Instr::MapNew { .. }) {
                item_count.checked_mul(2).ok_or(CompileError::Backend)?
            } else {
                item_count
            };
            let stack_start = stack
                .len()
                .checked_sub(item_count)
                .ok_or(CompileError::Backend)?;
            let post_stack = stack[..stack_start].to_vec();
            let (roots, item_start) = collect_capture_allocation_roots(
                builder,
                values,
                &plan.local_kinds,
                &site.stack,
                stack,
                item_count,
            )?;
            let kind = match instruction {
                Instr::TupleNew { .. } => ValueArrayAllocationKind::Tuple,
                Instr::ListNew { .. } => ValueArrayAllocationKind::List,
                Instr::MapNew { .. } => ValueArrayAllocationKind::Map,
                _ => return Err(CompileError::Backend),
            };
            let result = emit_value_array_allocation(
                builder,
                values,
                ValueArrayAllocationEmission {
                    kind,
                    item_start,
                    item_count,
                    roots: &roots,
                    point: FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    replay_stack: stack,
                    fault_stack: &post_stack,
                },
            )?;
            stack.truncate(stack_start);
            push_static(builder, stack, ScalarKind::Object(0), result)?;
        }
        Instr::New(class) | Instr::NewG { class, .. } => {
            let position = segment.start + within as u32;
            let site = segment
                .allocations
                .iter()
                .find(|site| site.instruction == position)
                .ok_or(CompileError::Backend)?;
            let mut roots = Vec::new();
            for (slot, (kind, variable)) in plan
                .local_kinds
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
            extend_stack_roots(&mut roots, &site.stack, stack)?;
            let environment = if matches!(instruction, Instr::NewG { .. }) {
                let site = type_environment_sites
                    .iter()
                    .find(|site| site.block == segment.block && site.instruction == position)
                    .ok_or(CompileError::Backend)?;
                emit_type_environment_lookup(
                    builder,
                    values,
                    site,
                    FaultPoint {
                        block: segment.block,
                        instruction: position,
                        prefix: prior_prefix,
                    },
                    stack,
                )?
            } else {
                builder.ins().iconst(types::I32, 0)
            };
            let value = emit_allocate_instance(
                builder,
                values,
                class,
                instance_field_count(input, class),
                environment,
                InstanceAllocationEmission {
                    roots: &roots,
                    allow_pending: plan
                        .virtual_constructor
                        .is_some_and(|constructor| constructor.class == class),
                    exit: ReplayEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: position + 1,
                            prefix: fault_prefix,
                        },
                        deopt_stack: stack,
                    },
                },
            )?;
            push_static(builder, stack, ScalarKind::Object(0), value)?;
        }
        _ => return Err(CompileError::Backend),
    }
    Ok(())
}
