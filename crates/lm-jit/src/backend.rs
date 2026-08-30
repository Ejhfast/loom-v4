//! Cranelift emission for one immutable native region plan.

use crate::activation::{
    NativeFunction, RawExit, RawNativeActivation, RawNativeFrame, ALLOCATION_HEAP_LIMIT,
    ALLOCATION_OK,
};
use crate::plan::{
    CallContract, FieldResult, FunctionDefinition, InlineFunctionPlan, RegionPlan, Segment,
    SegmentExit, UnsupportedReason,
};
use crate::{
    CompiledRegion, FunctionInput, NativeEntryCell, ScalarKind, EXIT_ALLOCATION, EXIT_CALL,
    EXIT_DIVIDE_BY_ZERO, EXIT_EFFECT, EXIT_FUEL, EXIT_HEAP_LIMIT, EXIT_INTEGER_OVERFLOW,
    EXIT_INTERPRETER, EXIT_INVALID_ENTRY, EXIT_RETURN, EXIT_STACK_LIMIT, EXIT_TYPE_MISMATCH,
    EXIT_UNINITIALIZED_FIELD, LOCAL_DIRTY, LOCAL_INITIALIZED,
};
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlags,
    UserFuncName,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module as _};
use lm_bytecode::{Func, Instr, NumericInstr};
use lm_heap::{
    JIT_ENTRY_GENERATION_OFFSET, JIT_ENTRY_LIVE_OFFSET, JIT_ENTRY_LIVE_TAG,
    JIT_ENTRY_OBJECT_TAG_OFFSET, JIT_ENTRY_SIZE, JIT_INSTANCE_CLASS_OFFSET,
    JIT_INSTANCE_FIELDS_OFFSET, JIT_OBJECT_INSTANCE, JIT_PAGE_MASK, JIT_PAGE_SHIFT,
    VALUE_ARRAY_DATA_OFFSET, VALUE_ARRAY_LEN_OFFSET,
};
use lm_value::{
    canonical_float_bits, ValueTag, CANONICAL_NAN_BITS, VALUE_PAYLOAD_OFFSET, VALUE_SIZE,
    VALUE_TAG_OFFSET,
};
use std::mem;
use std::sync::Mutex;

pub(super) enum CompileError {
    Unsupported(UnsupportedReason),
    Backend,
}

impl From<UnsupportedReason> for CompileError {
    fn from(reason: UnsupportedReason) -> CompileError {
        CompileError::Unsupported(reason)
    }
}

pub(super) fn compile_region(input: FunctionInput<'_>) -> Result<CompiledRegion, CompileError> {
    let plan = RegionPlan::for_function(&input)?;
    let func = input.root.runtime;

    let mut flags = settings::builder();
    flags
        .set("use_colocated_libcalls", "false")
        .map_err(|_| CompileError::Backend)?;
    flags
        .set("is_pic", "false")
        .map_err(|_| CompileError::Backend)?;
    flags
        .set("opt_level", "speed")
        .map_err(|_| CompileError::Backend)?;
    let isa = cranelift_native::builder()
        .map_err(|_| CompileError::Backend)?
        .finish(settings::Flags::new(flags))
        .map_err(|_| CompileError::Backend)?;
    let pointer_type = isa.pointer_type();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I32));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    let id = module
        .declare_function("loom_scalar_region", Linkage::Local, &signature)
        .map_err(|_| CompileError::Backend)?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, id.as_u32());
    let mut frontend = FunctionBuilderContext::new();
    emit_region(
        &mut context.func,
        &mut frontend,
        pointer_type,
        func,
        &plan,
        &input,
    )?;
    module
        .define_function(id, &mut context)
        .map_err(|_| CompileError::Backend)?;
    module
        .finalize_definitions()
        .map_err(|_| CompileError::Backend)?;
    let code = module.get_finalized_function(id);
    // SAFETY: The generated function uses the exact `NativeFunction` C ABI.
    // `CompiledRegion` retains the module that owns the executable memory.
    let entry = unsafe { mem::transmute::<*const u8, NativeFunction>(code) };
    Ok(CompiledRegion {
        plan,
        entry,
        module: Mutex::new(Some(module)),
    })
}

#[derive(Clone, Copy)]
struct NativeValues<'a> {
    locals: &'a [Variable],
    local_states: &'a [Variable],
    stack: &'a [Variable],
    fuel: Variable,
    retired: Variable,
    local_pointer: ir::Value,
    local_state_pointer: ir::Value,
    stack_pointer: ir::Value,
    allocation_context: ir::Value,
    allocate_instance: ir::Value,
    allocation_result_pointer: ir::Value,
    root_pointer: ir::Value,
    root_state_pointer: ir::Value,
    allocation_signature: ir::SigRef,
    native_signature: ir::SigRef,
    exit_pointer: ir::Value,
    activation_pointer: ir::Value,
    pointer_type: ir::Type,
}

#[derive(Clone, Copy)]
struct NativeRoot {
    bits: ir::Value,
    state: Option<ir::Value>,
}

#[derive(Clone, Copy)]
struct ExitEmission {
    retired: ir::Value,
    kind: u32,
    block: u32,
    instruction: u32,
    result: ir::Value,
}

#[derive(Clone, Copy)]
struct FaultPoint {
    block: u32,
    instruction: u32,
    prefix: u32,
}

struct InlineCallEmission<'a> {
    definition: FunctionDefinition<'a>,
    plan: &'a InlineFunctionPlan,
    root_local_kinds: &'a [ScalarKind],
    caller_stack_kinds: &'a [ScalarKind],
    deopt: FaultPoint,
    deopt_stack: &'a [ir::Value],
}

#[derive(Clone, Copy)]
struct FieldExitEmission<'a> {
    point: FaultPoint,
    fault_stack: &'a [ir::Value],
    deopt_stack: &'a [ir::Value],
}

#[derive(Clone, Copy)]
enum ObjectGuard<'a> {
    Fault(&'a [ir::Value]),
    Replay(&'a [ir::Value]),
}

struct NativeCallEmission<'a> {
    target: u32,
    contract: &'a CallContract,
    block: u32,
    instruction: u32,
    successor: ir::Block,
}

struct SegmentEmission<'a, 'b> {
    bytecode: &'a Func,
    segment: &'a Segment,
    blocks: &'a [ir::Block],
    values: NativeValues<'a>,
    plan: &'a RegionPlan,
    input: &'a FunctionInput<'b>,
    exact_fuel: bool,
    resume_blocks: Option<&'a [ir::Block]>,
}

fn emit_region(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
    pointer_type: ir::Type,
    bytecode: &Func,
    plan: &RegionPlan,
    input: &FunctionInput<'_>,
) -> Result<(), CompileError> {
    let call_conv = function.signature.call_conv;
    let mut builder = FunctionBuilder::new(function, frontend);
    let mut allocation_signature = ir::Signature::new(call_conv);
    allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    allocation_signature.returns.push(AbiParam::new(types::I32));
    let allocation_signature = builder.import_signature(allocation_signature);
    let mut native_signature = ir::Signature::new(call_conv);
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(types::I64));
    native_signature.params.push(AbiParam::new(types::I32));
    for _ in 0..7 {
        native_signature.params.push(AbiParam::new(pointer_type));
    }
    let native_signature = builder.import_signature(native_signature);
    let entry_block = builder.create_block();
    let invalid_block = builder.create_block();
    let blocks: Vec<ir::Block> = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect();
    let exact_blocks: Vec<Vec<ir::Block>> = plan
        .segments
        .iter()
        .map(|segment| {
            let has_inline_call = matches!(
                segment.exit,
                SegmentExit::Call { target, .. } if plan.inline_functions.contains_key(&target)
            );
            if has_inline_call {
                Vec::new()
            } else {
                segment
                    .fuel_stacks
                    .iter()
                    .map(|_| builder.create_block())
                    .collect()
            }
        })
        .collect();

    builder.switch_to_block(entry_block);
    builder.append_block_params_for_function_params(entry_block);
    let parameters = builder.block_params(entry_block);
    let local_pointer = parameters[0];
    let local_state_pointer = parameters[1];
    let stack_pointer = parameters[2];
    let initial_fuel = parameters[3];
    let entry = parameters[4];
    let allocation_context = parameters[5];
    let allocate_instance = parameters[6];
    let allocation_result_pointer = parameters[7];
    let root_pointer = parameters[8];
    let root_state_pointer = parameters[9];
    let exit_pointer = parameters[10];
    let activation_pointer = parameters[11];

    let mut locals = Vec::with_capacity(plan.local_kinds.len());
    let mut local_states = Vec::with_capacity(plan.local_kinds.len());
    for slot in 0..plan.local_kinds.len() {
        let local = builder.declare_var(types::I64);
        let state = builder.declare_var(types::I8);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), local_pointer, offset);
        let local_state = builder.ins().load(
            types::I8,
            MemFlags::new(),
            local_state_pointer,
            state_offset,
        );
        builder.def_var(local, value);
        builder.def_var(state, local_state);
        locals.push(local);
        local_states.push(state);
    }
    let mut stack = Vec::with_capacity(plan.max_stack);
    for slot in 0..plan.max_stack {
        let variable = builder.declare_var(types::I64);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), stack_pointer, offset);
        builder.def_var(variable, value);
        stack.push(variable);
    }
    let fuel = builder.declare_var(types::I64);
    let retired = builder.declare_var(types::I64);
    builder.def_var(fuel, initial_fuel);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.def_var(retired, zero);
    let values = NativeValues {
        locals: &locals,
        local_states: &local_states,
        stack: &stack,
        fuel,
        retired,
        local_pointer,
        local_state_pointer,
        stack_pointer,
        allocation_context,
        allocate_instance,
        allocation_result_pointer,
        root_pointer,
        root_state_pointer,
        allocation_signature,
        native_signature,
        exit_pointer,
        activation_pointer,
        pointer_type,
    };

    let mut dispatch = Switch::new();
    for (index, block) in blocks.iter().copied().enumerate() {
        dispatch.set_entry(index as u128, block);
    }
    for (offset, target) in plan.resume_targets.iter().enumerate() {
        let index = plan
            .segments
            .len()
            .checked_add(offset)
            .ok_or(CompileError::Backend)?;
        let block = exact_blocks
            .get(target.segment)
            .and_then(|blocks| blocks.get(target.offset))
            .copied()
            .ok_or(CompileError::Backend)?;
        dispatch.set_entry(index as u128, block);
    }
    dispatch.emit(&mut builder, entry, invalid_block);

    builder.switch_to_block(invalid_block);
    let retired_value = builder.use_var(retired);
    emit_exit(
        &mut builder,
        values,
        ExitEmission {
            retired: retired_value,
            kind: EXIT_INVALID_ENTRY,
            block: 0,
            instruction: 0,
            result: zero,
        },
        &[],
    )?;

    for (index, segment) in plan.segments.iter().enumerate() {
        builder.switch_to_block(blocks[index]);
        let body = builder.create_block();
        let exact_fuel = builder.create_block();
        let available = builder.use_var(fuel);
        let enough = builder.ins().icmp_imm(
            IntCC::UnsignedGreaterThanOrEqual,
            available,
            i64::from(segment.cost),
        );
        builder.ins().brif(enough, body, &[], exact_fuel, &[]);

        builder.switch_to_block(exact_fuel);
        let has_inline_call = matches!(
            segment.exit,
            SegmentExit::Call { target, .. } if plan.inline_functions.contains_key(&target)
        );
        if has_inline_call {
            let retired_value = builder.use_var(retired);
            let result = builder.ins().iconst(types::I64, 0);
            let entry_stack: Vec<ir::Value> = values
                .stack
                .iter()
                .take(segment.entry_stack.len())
                .map(|variable| builder.use_var(*variable))
                .collect();
            emit_exit(
                &mut builder,
                values,
                ExitEmission {
                    retired: retired_value,
                    kind: EXIT_FUEL,
                    block: segment.block,
                    instruction: segment.start,
                    result,
                },
                &entry_stack,
            )?;
        } else {
            let first = exact_blocks[index]
                .first()
                .copied()
                .ok_or(CompileError::Backend)?;
            builder.ins().jump(first, &[]);
            emit_segment(
                &mut builder,
                SegmentEmission {
                    bytecode,
                    segment,
                    blocks: &blocks,
                    values,
                    plan,
                    input,
                    exact_fuel: true,
                    resume_blocks: Some(&exact_blocks[index]),
                },
            )?;
        }

        builder.switch_to_block(body);
        emit_segment(
            &mut builder,
            SegmentEmission {
                bytecode,
                segment,
                blocks: &blocks,
                values,
                plan,
                input,
                exact_fuel: false,
                resume_blocks: None,
            },
        )?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn emit_segment(
    builder: &mut FunctionBuilder<'_>,
    emission: SegmentEmission<'_, '_>,
) -> Result<(), CompileError> {
    let SegmentEmission {
        bytecode,
        segment,
        blocks,
        values,
        plan,
        input,
        exact_fuel,
        resume_blocks,
    } = emission;
    let mut stack: Vec<ir::Value> = if resume_blocks.is_some() {
        Vec::new()
    } else {
        values
            .stack
            .iter()
            .take(segment.entry_stack.len())
            .map(|variable| builder.use_var(*variable))
            .collect()
    };
    let code =
        &bytecode.blocks[segment.block as usize][segment.start as usize..segment.end as usize];
    for (within, instruction) in code.iter().copied().enumerate() {
        if let Some(resume_blocks) = resume_blocks {
            let block = resume_blocks
                .get(within)
                .copied()
                .ok_or(CompileError::Backend)?;
            builder.switch_to_block(block);
            let (position, kinds) = segment
                .fuel_stacks
                .get(within)
                .ok_or(CompileError::Backend)?;
            if *position != segment.start + within as u32 {
                return Err(CompileError::Backend);
            }
            stack = values
                .stack
                .iter()
                .take(kinds.len())
                .map(|variable| builder.use_var(*variable))
                .collect();
        }
        let prefix = within as u32 + 1;
        let deferred_boundary = within + 1 == code.len()
            && matches!(
                segment.exit,
                SegmentExit::Call { .. } | SegmentExit::Effect { .. }
            );
        if exact_fuel && !deferred_boundary {
            emit_exact_fuel_check(
                builder,
                values,
                segment.block,
                segment.start + within as u32,
                &stack,
            )?;
        }
        let fault_prefix = if exact_fuel { 1 } else { prefix };
        match instruction {
            Instr::ConstUnit => {
                let value = builder.ins().iconst(types::I64, 0);
                stack.push(value);
            }
            Instr::New(class) => {
                let instruction = segment.start + within as u32;
                let site = segment
                    .allocations
                    .iter()
                    .find(|site| site.instruction == instruction)
                    .ok_or(CompileError::Backend)?;
                let mut roots = Vec::new();
                for (slot, (kind, variable)) in plan
                    .local_kinds
                    .iter()
                    .copied()
                    .zip(values.locals.iter().copied())
                    .enumerate()
                {
                    if matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            state: Some(builder.use_var(values.local_states[slot])),
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
                let value = emit_allocate_instance(
                    builder,
                    values,
                    class,
                    &roots,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &stack,
                )?;
                stack.push(value);
            }
            Instr::ConstBool(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                stack.push(value);
            }
            Instr::ConstInt(value) => {
                let value = builder.ins().iconst(types::I64, value);
                stack.push(value);
            }
            Instr::ConstFloat(bits) => {
                let value = builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64);
                stack.push(value);
            }
            Instr::OpConst(operation) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(operation)));
            }
            Instr::LoadLocal(slot) => {
                stack.push(builder.use_var(values.locals[slot as usize]));
            }
            Instr::StoreLocal(slot) => {
                let value = pop_native(&mut stack)?;
                builder.def_var(values.locals[slot as usize], value);
                let state = builder
                    .ins()
                    .iconst(types::I8, i64::from(LOCAL_DIRTY | LOCAL_INITIALIZED));
                builder.def_var(values.local_states[slot as usize], state);
            }
            Instr::Pop => {
                pop_native(&mut stack)?;
            }
            Instr::LoadField(field) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let field_result = segment
                    .field_results
                    .iter()
                    .find(|result| result.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let value = emit_load_field(
                    builder,
                    values,
                    reference,
                    field,
                    field_result,
                    FieldExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::Add | Instr::Sub | Instr::Mul => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let (result, overflow) = match instruction {
                    Instr::Add => builder.ins().sadd_overflow(left, right),
                    Instr::Sub => builder.ins().ssub_overflow(left, right),
                    Instr::Mul => builder.ins().smul_overflow(left, right),
                    _ => unreachable!(),
                };
                let result = emit_overflow_check(
                    builder,
                    values,
                    overflow,
                    result,
                    FaultPoint {
                        block: segment.block,
                        instruction: segment.start + prefix,
                        prefix: fault_prefix,
                    },
                    &stack,
                )?;
                stack.push(result);
            }
            Instr::Div | Instr::Rem => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: segment.start + prefix,
                    prefix: fault_prefix,
                };
                let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                emit_fault_check(builder, values, zero, EXIT_DIVIDE_BY_ZERO, point, &stack)?;
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
                    &stack,
                )?;
                let result = if matches!(instruction, Instr::Div) {
                    builder.ins().sdiv(left, right)
                } else {
                    builder.ins().srem(left, right)
                };
                stack.push(result);
            }
            Instr::Neg => {
                let value = pop_native(&mut stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (result, overflow) = builder.ins().ssub_overflow(zero, value);
                let result = emit_overflow_check(
                    builder,
                    values,
                    overflow,
                    result,
                    FaultPoint {
                        block: segment.block,
                        instruction: segment.start + prefix,
                        prefix: fault_prefix,
                    },
                    &stack,
                )?;
                stack.push(result);
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                stack.push(builder.ins().bxor_imm(value, 1));
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
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
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::EqBool | Instr::NeBool => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqBool) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::Numeric(operation) => {
                emit_float_instruction(builder, &mut stack, operation)?;
            }
            Instr::Call(_)
            | Instr::Perform { .. }
            | Instr::PerformValue { .. }
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Return => {}
            _ => {
                return Err(CompileError::Unsupported(
                    UnsupportedReason::UnsupportedInstruction,
                ))
            }
        }
        if exact_fuel && !deferred_boundary {
            emit_charge(builder, values, 1);
        }
        if let Some(resume_blocks) = resume_blocks.filter(|_| within + 1 < code.len()) {
            define_stack(builder, values, &stack)?;
            let next = resume_blocks
                .get(within + 1)
                .copied()
                .ok_or(CompileError::Backend)?;
            builder.ins().jump(next, &[]);
        }
    }

    if let SegmentExit::Call { target, .. } = segment.exit {
        let call_instruction = segment.end - 1;
        let prefix = segment.end - segment.start - 1;
        if let Some(inline) = plan.inline_functions.get(&target) {
            let definition = input
                .definition(target)
                .ok_or(CompileError::Unsupported(UnsupportedReason::MissingSource))?;
            let deopt_stack = stack.clone();
            let caller_kind_count = segment
                .boundary_stack
                .len()
                .checked_sub(inline.params.len())
                .ok_or(CompileError::Backend)?;
            emit_inline_call(
                builder,
                values,
                &mut stack,
                InlineCallEmission {
                    definition,
                    plan: inline,
                    root_local_kinds: &plan.local_kinds,
                    caller_stack_kinds: &segment.boundary_stack[..caller_kind_count],
                    deopt: FaultPoint {
                        block: segment.block,
                        instruction: call_instruction,
                        prefix,
                    },
                    deopt_stack: &deopt_stack,
                },
            )?;
            emit_segment_charge(builder, values, segment.cost, exact_fuel);
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        } else {
            emit_segment_charge(builder, values, segment.cost, exact_fuel);
            let contract = plan
                .call_contracts
                .get(&target)
                .ok_or(CompileError::Backend)?;
            emit_native_call(
                builder,
                values,
                &mut stack,
                NativeCallEmission {
                    target,
                    contract,
                    block: segment.block,
                    instruction: call_instruction,
                    successor: blocks[segment.successors[0]],
                },
            )?;
        }
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Effect { .. }) {
        let effect_instruction = segment.end - 1;
        emit_segment_charge(builder, values, segment.cost, exact_fuel);
        let retired = builder.use_var(values.retired);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_EFFECT,
                block: segment.block,
                instruction: effect_instruction,
                result: zero,
            },
            &stack,
        )?;
        return Ok(());
    }

    emit_segment_charge(builder, values, segment.cost, exact_fuel);
    match segment.exit {
        SegmentExit::Jump { .. } => {
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        }
        SegmentExit::Conditional { jump_on_true, .. } => {
            let condition = pop_native(&mut stack)?;
            define_stack(builder, values, &stack)?;
            let condition = builder.ins().icmp_imm(IntCC::NotEqual, condition, 0);
            let target = blocks[segment.successors[0]];
            let fallthrough = blocks[segment.successors[1]];
            if jump_on_true {
                builder.ins().brif(condition, target, &[], fallthrough, &[]);
            } else {
                builder.ins().brif(condition, fallthrough, &[], target, &[]);
            }
        }
        SegmentExit::Call { .. } => unreachable!(),
        SegmentExit::Allocation { .. } => {
            define_stack(builder, values, &stack)?;
            builder.ins().jump(blocks[segment.successors[0]], &[]);
        }
        SegmentExit::Effect { .. } => unreachable!(),
        SegmentExit::Return => {
            let result = pop_native(&mut stack)?;
            let retired = builder.use_var(values.retired);
            emit_exit(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_RETURN,
                    block: segment.block,
                    instruction: segment.end,
                    result,
                },
                &stack,
            )?;
        }
    }
    Ok(())
}

fn emit_native_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &mut Vec<ir::Value>,
    call: NativeCallEmission<'_>,
) -> Result<(), CompileError> {
    let NativeCallEmission {
        target,
        contract,
        block,
        instruction,
        successor,
    } = call;
    let argument_start = stack
        .len()
        .checked_sub(contract.params.len())
        .ok_or(CompileError::Backend)?;
    let boundary_stack = stack.clone();
    let caller_stack = stack[..argument_start].to_vec();
    let arguments = stack[argument_start..].to_vec();
    let fuel_exit = builder.create_block();
    let lookup = builder.create_block();
    let fallback = builder.create_block();
    let stack_limit = builder.create_block();
    let capacity = builder.create_block();
    let invoke = builder.create_block();
    let returned = builder.create_block();
    let propagate = builder.create_block();

    let fuel = builder.use_var(values.fuel);
    let has_fuel = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, fuel, 1);
    builder.ins().brif(has_fuel, lookup, &[], fuel_exit, &[]);

    builder.switch_to_block(fuel_exit);
    let retired = builder.use_var(values.retired);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_FUEL,
            block,
            instruction,
            result: zero,
        },
        &boundary_stack,
    )?;

    builder.switch_to_block(lookup);
    let entry_count = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, entry_count),
    )?;
    let target_in_range =
        builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThan, entry_count, i64::from(target));
    let have_target = builder.create_block();
    builder
        .ins()
        .brif(target_in_range, have_target, &[], fallback, &[]);

    builder.switch_to_block(have_target);
    let entries = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, entries),
    )?;
    let entry_offset = i32::try_from(
        (target as usize)
            .checked_mul(mem::size_of::<usize>())
            .ok_or(CompileError::Backend)?,
    )
    .map_err(|_| CompileError::Backend)?;
    let cell = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), entries, entry_offset);
    let code = builder
        .ins()
        .atomic_load(values.pointer_type, MemFlags::new(), cell);
    let published = builder.ins().icmp_imm(IntCC::NotEqual, code, 0);
    let limits = builder.create_block();
    builder.ins().brif(published, limits, &[], fallback, &[]);

    builder.switch_to_block(limits);
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let base_frames = load_activation_u32(builder, values, RawActivationField::BaseFrames)?;
    let max_frames = load_activation_u32(builder, values, RawActivationField::MaxFrames)?;
    let total_frames = builder.ins().iadd(base_frames, frame_len);
    let frame_overflow =
        builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, total_frames, max_frames);
    let stack_check = builder.create_block();
    builder
        .ins()
        .brif(frame_overflow, stack_limit, &[], stack_check, &[]);

    builder.switch_to_block(stack_check);
    let stack_values = load_activation_u32(builder, values, RawActivationField::StackValues)?;
    let caller_values = builder
        .ins()
        .iadd_imm(stack_values, -(contract.params.len() as i64));
    let local_count = load_cell_u32(builder, cell, mem::offset_of!(NativeEntryCell, local_count))?;
    let pushed_values = builder.ins().iadd(caller_values, local_count);
    let max_values = load_activation_u32(builder, values, RawActivationField::MaxStackValues)?;
    let stack_overflow = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, pushed_values, max_values);
    builder
        .ins()
        .brif(stack_overflow, stack_limit, &[], capacity, &[]);

    builder.switch_to_block(stack_limit);
    emit_charge(builder, values, 1);
    let retired = builder.use_var(values.retired);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_STACK_LIMIT,
            block,
            instruction: instruction + 1,
            result: zero,
        },
        &boundary_stack,
    )?;

    builder.switch_to_block(capacity);
    let max_stack = load_cell_u32(builder, cell, mem::offset_of!(NativeEntryCell, max_stack))?;
    let callee_stack_values = load_cell_u32(
        builder,
        cell,
        mem::offset_of!(NativeEntryCell, max_stack_values),
    )?;
    let body_values = builder.ins().iadd(pushed_values, callee_stack_values);
    let body_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, body_values, max_values);
    let frame_capacity = load_activation_u32(builder, values, RawActivationField::FrameCapacity)?;
    let frame_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, frame_len, frame_capacity);
    let scalar_len = load_activation_u32(builder, values, RawActivationField::ScalarLen)?;
    let scalar_capacity = load_activation_u32(builder, values, RawActivationField::ScalarCapacity)?;
    let window = builder.ins().iadd(local_count, max_stack);
    let scalar_end = builder.ins().iadd(scalar_len, window);
    let scalars_fit =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, scalar_end, scalar_capacity);
    let local_count_matches = builder.ins().icmp_imm(
        IntCC::Equal,
        local_count,
        i64::try_from(contract.local_count).map_err(|_| CompileError::Backend)?,
    );
    let fits = builder.ins().band(body_fits, frame_fits);
    let fits = builder.ins().band(fits, scalars_fit);
    let fits = builder.ins().band(fits, local_count_matches);
    builder.ins().brif(fits, invoke, &[], fallback, &[]);

    builder.switch_to_block(fallback);
    let retired = builder.use_var(values.retired);
    let target_value = builder.ins().iconst(types::I64, i64::from(target));
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_CALL,
            block,
            instruction,
            result: target_value,
        },
        &boundary_stack,
    )?;

    builder.switch_to_block(invoke);
    emit_charge(builder, values, 1);
    emit_spill_frame(builder, values, block, instruction + 1, &caller_stack)?;
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let frames = load_activation_pointer(builder, values, RawActivationField::Frames)?;
    let scalar_base = scalar_len;
    let scalar_base_pointer = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_byte_offset = builder.ins().ishl_imm(scalar_base_pointer, 3);
    let child_locals = builder.ins().iadd(scalars, scalar_byte_offset);
    let child_states = builder.ins().iadd(states, scalar_base_pointer);
    let local_count_pointer = builder.ins().uextend(values.pointer_type, local_count);
    let local_byte_offset = builder.ins().ishl_imm(local_count_pointer, 3);
    let child_operands = builder.ins().iadd(child_locals, local_byte_offset);
    let zero_i8 = builder.ins().iconst(types::I8, 0);
    for slot in 0..contract.local_count {
        let offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), zero_i8, child_states, offset);
    }
    let initialized = builder
        .ins()
        .iconst(types::I8, i64::from(LOCAL_INITIALIZED));
    for (slot, argument) in arguments.iter().copied().enumerate() {
        let value_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), argument, child_locals, value_offset);
        builder
            .ins()
            .store(MemFlags::new(), initialized, child_states, state_offset);
    }
    let frame_index = builder.ins().uextend(values.pointer_type, frame_len);
    let frame_offset = builder
        .ins()
        .imul_imm(frame_index, mem::size_of::<RawNativeFrame>() as i64);
    let child_frame = builder.ins().iadd(frames, frame_offset);
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, function),
        target,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, block),
        0,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, instruction),
        0,
    )?;
    store_i32_value(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, scalar_base),
        scalar_base,
    )?;
    store_i32_value(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, local_count),
        local_count,
    )?;
    store_i32_value(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, max_stack),
        max_stack,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, operand_len),
        0,
    )?;
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, native_created),
        1,
    )?;
    store_i32_value(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, caller_stack_values),
        caller_values,
    )?;
    let next_frame_len = builder.ins().iadd_imm(frame_len, 1);
    store_activation_u32(builder, values, RawActivationField::ScalarLen, scalar_end)?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::FrameLen,
        next_frame_len,
    )?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::StackValues,
        pushed_values,
    )?;
    let child_fuel = builder.use_var(values.fuel);
    let zero_entry = builder.ins().iconst(types::I32, 0);
    builder.ins().call_indirect(
        values.native_signature,
        code,
        &[
            child_locals,
            child_states,
            child_operands,
            child_fuel,
            zero_entry,
            values.allocation_context,
            values.allocate_instance,
            values.allocation_result_pointer,
            values.root_pointer,
            values.root_state_pointer,
            values.exit_pointer,
            values.activation_pointer,
        ],
    );
    let child_retired = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
    )?;
    let caller_retired = builder.use_var(values.retired);
    let total_retired = builder.ins().iadd(caller_retired, child_retired);
    builder.def_var(values.retired, total_retired);
    let caller_fuel = builder.use_var(values.fuel);
    let remaining_fuel = builder.ins().isub(caller_fuel, child_retired);
    builder.def_var(values.fuel, remaining_fuel);
    let exit_kind = load_value(
        builder,
        types::I32,
        values.exit_pointer,
        mem::offset_of!(RawExit, kind),
    )?;
    let normal_return = builder
        .ins()
        .icmp_imm(IntCC::Equal, exit_kind, i64::from(EXIT_RETURN));
    builder
        .ins()
        .brif(normal_return, returned, &[], propagate, &[]);

    builder.switch_to_block(propagate);
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
        total_retired,
    )?;
    builder.ins().return_(&[]);

    builder.switch_to_block(returned);
    let result = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        mem::offset_of!(RawExit, result),
    )?;
    store_activation_u32(builder, values, RawActivationField::ScalarLen, scalar_base)?;
    store_activation_u32(builder, values, RawActivationField::FrameLen, frame_len)?;
    let returned_stack_values = builder.ins().iadd_imm(caller_values, 1);
    store_activation_u32(
        builder,
        values,
        RawActivationField::StackValues,
        returned_stack_values,
    )?;
    stack.truncate(argument_start);
    stack.push(result);
    define_stack(builder, values, stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

fn emit_inline_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    caller_stack: &mut Vec<ir::Value>,
    call: InlineCallEmission<'_>,
) -> Result<(), CompileError> {
    let argument_start = caller_stack
        .len()
        .checked_sub(call.plan.params.len())
        .ok_or(CompileError::Backend)?;
    let arguments = caller_stack.split_off(argument_start);
    let mut locals = vec![None; call.plan.local_kinds.len()];
    for (slot, value) in arguments.into_iter().enumerate() {
        locals[slot] = Some(value);
    }
    let mut stack = Vec::with_capacity(call.plan.max_stack);
    let code = call
        .definition
        .runtime
        .blocks
        .first()
        .ok_or(CompileError::Backend)?;
    for (instruction_index, instruction) in code.iter().copied().enumerate() {
        match instruction {
            Instr::ConstUnit => stack.push(builder.ins().iconst(types::I64, 0)),
            Instr::ConstBool(value) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(value)));
            }
            Instr::ConstInt(value) => stack.push(builder.ins().iconst(types::I64, value)),
            Instr::ConstFloat(bits) => stack.push(
                builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64),
            ),
            Instr::OpConst(operation) => {
                stack.push(builder.ins().iconst(types::I64, i64::from(operation)));
            }
            Instr::LoadLocal(slot) => {
                let value = locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                    .ok_or(CompileError::Backend)?;
                stack.push(value);
            }
            Instr::StoreLocal(slot) => {
                let value = pop_native(&mut stack)?;
                *locals.get_mut(slot as usize).ok_or(CompileError::Backend)? = Some(value);
            }
            Instr::Pop => {
                pop_native(&mut stack)?;
            }
            Instr::New(class) => {
                let site = call
                    .plan
                    .allocations
                    .iter()
                    .find(|site| site.instruction as usize == instruction_index)
                    .ok_or(CompileError::Backend)?;
                let mut roots = Vec::new();
                for (slot, (kind, variable)) in call
                    .root_local_kinds
                    .iter()
                    .copied()
                    .zip(values.locals.iter().copied())
                    .enumerate()
                {
                    if matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            state: Some(builder.use_var(values.local_states[slot])),
                        });
                    }
                }
                extend_stack_roots(&mut roots, call.caller_stack_kinds, caller_stack)?;
                for ((kind, initialized), value) in call
                    .plan
                    .local_kinds
                    .iter()
                    .copied()
                    .zip(site.initialized.iter().copied())
                    .zip(locals.iter().copied())
                {
                    if initialized && matches!(kind, ScalarKind::Object(_)) {
                        roots.push(NativeRoot {
                            bits: value.ok_or(CompileError::Backend)?,
                            state: None,
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
                let (status, value) = emit_allocation_call(builder, values, class, &roots, false)?;
                let replay =
                    builder
                        .ins()
                        .icmp_imm(IntCC::NotEqual, status, i64::from(ALLOCATION_OK));
                emit_fault_check(
                    builder,
                    values,
                    replay,
                    EXIT_ALLOCATION,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(value);
            }
            Instr::Add | Instr::Sub | Instr::Mul => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let (result, overflow) = match instruction {
                    Instr::Add => builder.ins().sadd_overflow(left, right),
                    Instr::Sub => builder.ins().ssub_overflow(left, right),
                    Instr::Mul => builder.ins().smul_overflow(left, right),
                    _ => unreachable!(),
                };
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(result);
            }
            Instr::Div | Instr::Rem => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let zero = builder.ins().icmp_imm(IntCC::Equal, right, 0);
                emit_fault_check(
                    builder,
                    values,
                    zero,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                let minimum = builder.ins().iconst(types::I64, i64::MIN);
                let minimum_left = builder.ins().icmp(IntCC::Equal, left, minimum);
                let negative_one = builder.ins().icmp_imm(IntCC::Equal, right, -1);
                let overflow = builder.ins().band(minimum_left, negative_one);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                let result = if matches!(instruction, Instr::Div) {
                    builder.ins().sdiv(left, right)
                } else {
                    builder.ins().srem(left, right)
                };
                stack.push(result);
            }
            Instr::Neg => {
                let value = pop_native(&mut stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (result, overflow) = builder.ins().ssub_overflow(zero, value);
                emit_fault_check(
                    builder,
                    values,
                    overflow,
                    EXIT_INTERPRETER,
                    call.deopt,
                    call.deopt_stack,
                )?;
                stack.push(result);
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                stack.push(builder.ins().bxor_imm(value, 1));
            }
            Instr::LtInt
            | Instr::LeInt
            | Instr::GtInt
            | Instr::GeInt
            | Instr::EqInt
            | Instr::NeInt => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
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
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::EqBool | Instr::NeBool => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqBool) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                stack.push(builder.ins().uextend(types::I64, compared));
            }
            Instr::Numeric(operation) => {
                emit_float_instruction(builder, &mut stack, operation)?;
            }
            Instr::Return => {
                let result = pop_native(&mut stack)?;
                if !stack.is_empty() {
                    return Err(CompileError::Backend);
                }
                caller_stack.push(result);
                return Ok(());
            }
            _ => {
                return Err(CompileError::Unsupported(
                    UnsupportedReason::UnsupportedInstruction,
                ));
            }
        }
    }
    Err(CompileError::Backend)
}

fn pop_native(stack: &mut Vec<ir::Value>) -> Result<ir::Value, CompileError> {
    stack.pop().ok_or(CompileError::Backend)
}

fn emit_charge(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>, cost: u32) {
    let fuel = builder.use_var(values.fuel);
    let retired = builder.use_var(values.retired);
    let fuel = builder.ins().iadd_imm(fuel, -i64::from(cost));
    let retired = builder.ins().iadd_imm(retired, i64::from(cost));
    builder.def_var(values.fuel, fuel);
    builder.def_var(values.retired, retired);
}

fn emit_segment_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    cost: u32,
    exact_fuel: bool,
) {
    if !exact_fuel {
        emit_charge(builder, values, cost);
    }
}

fn emit_exact_fuel_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    let run = builder.create_block();
    let stop = builder.create_block();
    let fuel = builder.use_var(values.fuel);
    let available = builder.ins().icmp_imm(IntCC::NotEqual, fuel, 0);
    builder.ins().brif(available, run, &[], stop, &[]);
    builder.switch_to_block(stop);
    let retired = builder.use_var(values.retired);
    let result = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_FUEL,
            block,
            instruction,
            result,
        },
        stack,
    )?;
    builder.switch_to_block(run);
    Ok(())
}

fn emit_overflow_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    overflow: ir::Value,
    result: ir::Value,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<ir::Value, CompileError> {
    emit_fault_check(
        builder,
        values,
        overflow,
        EXIT_INTEGER_OVERFLOW,
        point,
        stack,
    )?;
    Ok(result)
}

fn emit_fault_check(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    faulted: ir::Value,
    kind: u32,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    let fault = builder.create_block();
    let success = builder.create_block();
    builder.ins().brif(faulted, fault, &[], success, &[]);
    builder.switch_to_block(fault);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(point.prefix));
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind,
            block: point.block,
            instruction: point.instruction,
            result: zero,
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

fn emit_load_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    field: u32,
    result: FieldResult,
    exit: FieldExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let class = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let other_class =
        builder
            .ins()
            .icmp_imm(IntCC::NotEqual, class, i64::from(result.receiver_class));
    emit_interpreter_replay(builder, values, other_class, exit.point, exit.deopt_stack)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, field_index, len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
    )?;
    let byte_offset = builder.ins().imul_imm(
        field_index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let value = builder.ins().iadd(data, byte_offset);
    let tag = load_value(builder, types::I64, value, VALUE_TAG_OFFSET)?;
    let uninitialized = builder
        .ins()
        .icmp_imm(IntCC::Equal, tag, ValueTag::Uninit as u64 as i64);
    emit_fault_check(
        builder,
        values,
        uninitialized,
        EXIT_UNINITIALIZED_FIELD,
        exit.point,
        exit.fault_stack,
    )?;
    let expected_tag = value_tag(result.kind);
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, expected_tag as u64 as i64);
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    let payload = emit_value_payload(
        builder,
        values,
        value,
        result.kind,
        exit.point,
        exit.deopt_stack,
    )?;
    if let Some(class) = result.result_class {
        let loaded = emit_object_entry(
            builder,
            values,
            payload,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        let actual = load_value(builder, types::I32, loaded, JIT_INSTANCE_CLASS_OFFSET)?;
        let mismatch = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, actual, i64::from(class));
        emit_interpreter_replay(builder, values, mismatch, exit.point, exit.deopt_stack)?;
    }
    Ok(payload)
}

fn emit_object_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    emit_object_guard(builder, values, outside, point, guard)?;

    let pages = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_pages),
    )?;
    let page_index = builder
        .ins()
        .ushr_imm(slot_index, i64::from(JIT_PAGE_SHIFT));
    let page_offset = builder.ins().imul_imm(
        page_index,
        i64::try_from(mem::size_of::<usize>()).map_err(|_| CompileError::Backend)?,
    );
    let page_address = builder.ins().iadd(pages, page_offset);
    let page = builder
        .ins()
        .load(values.pointer_type, MemFlags::trusted(), page_address, 0);
    let within_page = builder.ins().band_imm(slot_index, i64::from(JIT_PAGE_MASK));
    let entry_offset = builder.ins().imul_imm(
        within_page,
        i64::try_from(JIT_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(page, entry_offset);
    let expected_generation = builder.ins().ushr_imm(reference, 32);
    let expected_generation = builder.ins().ireduce(types::I32, expected_generation);
    let generation = load_value(builder, types::I32, entry, JIT_ENTRY_GENERATION_OFFSET)?;
    let live = load_value(builder, types::I32, entry, JIT_ENTRY_LIVE_OFFSET)?;
    let stale = builder
        .ins()
        .icmp(IntCC::NotEqual, generation, expected_generation);
    let dead = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, live, i64::from(JIT_ENTRY_LIVE_TAG));
    let invalid = builder.ins().bor(stale, dead);
    emit_object_guard(builder, values, invalid, point, guard)?;
    let kind = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let wrong_kind = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, kind, i64::from(JIT_OBJECT_INSTANCE));
    emit_object_guard(builder, values, wrong_kind, point, guard)?;
    Ok(entry)
}

fn emit_object_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    invalid: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<(), CompileError> {
    match guard {
        ObjectGuard::Fault(stack) => {
            emit_fault_check(builder, values, invalid, EXIT_TYPE_MISMATCH, point, stack)
        }
        ObjectGuard::Replay(stack) => {
            emit_interpreter_replay(builder, values, invalid, point, stack)
        }
    }
}

fn value_tag(kind: ScalarKind) -> ValueTag {
    match kind {
        ScalarKind::Unit => ValueTag::Unit,
        ScalarKind::Bool => ValueTag::Bool,
        ScalarKind::Int => ValueTag::Int,
        ScalarKind::Float => ValueTag::Float,
        ScalarKind::Object(_) => ValueTag::Obj,
        ScalarKind::Operation => ValueTag::Op,
    }
}

fn emit_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    kind: ScalarKind,
    point: FaultPoint,
    deopt_stack: &[ir::Value],
) -> Result<ir::Value, CompileError> {
    let payload = match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool => {
            let byte = load_value(builder, types::I8, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, byte)
        }
        ScalarKind::Int | ScalarKind::Object(_) => {
            load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?
        }
        ScalarKind::Float => {
            let bits = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
            let exponent = builder.ins().band_imm(bits, 0x7ff0_0000_0000_0000);
            let exponent_is_nan =
                builder
                    .ins()
                    .icmp_imm(IntCC::Equal, exponent, 0x7ff0_0000_0000_0000);
            let fraction = builder.ins().band_imm(bits, 0x000f_ffff_ffff_ffff);
            let has_fraction = builder.ins().icmp_imm(IntCC::NotEqual, fraction, 0);
            let is_nan = builder.ins().band(exponent_is_nan, has_fraction);
            let canonical = builder
                .ins()
                .icmp_imm(IntCC::Equal, bits, CANONICAL_NAN_BITS as i64);
            let not_canonical = builder.ins().bnot(canonical);
            let noncanonical = builder.ins().band(is_nan, not_canonical);
            emit_interpreter_replay(builder, values, noncanonical, point, deopt_stack)?;
            bits
        }
        ScalarKind::Operation => {
            let operation = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, operation)
        }
    };
    Ok(payload)
}

fn extend_stack_roots(
    roots: &mut Vec<NativeRoot>,
    kinds: &[ScalarKind],
    values: &[ir::Value],
) -> Result<(), CompileError> {
    if kinds.len() != values.len() {
        return Err(CompileError::Backend);
    }
    for (kind, value) in kinds.iter().copied().zip(values.iter().copied()) {
        if matches!(kind, ScalarKind::Object(_)) {
            roots.push(NativeRoot {
                bits: value,
                state: None,
            });
        }
    }
    Ok(())
}

fn emit_allocate_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    roots: &[NativeRoot],
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<ir::Value, CompileError> {
    let (status, result) = emit_allocation_call(builder, values, class, roots, true)?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(ALLOCATION_HEAP_LIMIT));
    emit_fault_check(builder, values, heap_limit, EXIT_HEAP_LIMIT, point, stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(ALLOCATION_OK));
    emit_interpreter_replay(builder, values, replay, point, stack)?;
    Ok(result)
}

fn emit_allocation_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    roots: &[NativeRoot],
    allow_collection: bool,
) -> Result<(ir::Value, ir::Value), CompileError> {
    for (slot, root) in roots.iter().copied().enumerate() {
        let value_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder.ins().store(
            MemFlags::new(),
            root.bits,
            values.root_pointer,
            value_offset,
        );
        let state = root.state.unwrap_or_else(|| {
            builder
                .ins()
                .iconst(types::I8, i64::from(LOCAL_INITIALIZED))
        });
        builder.ins().store(
            MemFlags::new(),
            state,
            values.root_state_pointer,
            state_offset,
        );
    }
    let class = builder.ins().iconst(types::I32, i64::from(class));
    let collection = builder
        .ins()
        .iconst(types::I32, i64::from(allow_collection));
    let root_count = builder.ins().iconst(
        types::I32,
        i64::try_from(roots.len()).map_err(|_| CompileError::Backend)?,
    );
    let call = builder.ins().call_indirect(
        values.allocation_signature,
        values.allocate_instance,
        &[
            values.allocation_context,
            class,
            collection,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    Ok((status, result))
}

fn emit_interpreter_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    replay: ir::Value,
    point: FaultPoint,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    let interpreter = builder.create_block();
    let success = builder.create_block();
    builder.ins().brif(replay, interpreter, &[], success, &[]);
    builder.switch_to_block(interpreter);
    let retired = builder.use_var(values.retired);
    let retired = builder
        .ins()
        .iadd_imm(retired, i64::from(point.prefix.saturating_sub(1)));
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_INTERPRETER,
            block: point.block,
            instruction: point.instruction.saturating_sub(1),
            result: zero,
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

fn emit_float_instruction(
    builder: &mut FunctionBuilder<'_>,
    stack: &mut Vec<ir::Value>,
    operation: NumericInstr,
) -> Result<(), CompileError> {
    match operation {
        NumericInstr::FloatNeg => {
            let value = float_value(builder, pop_native(stack)?);
            let value = builder.ins().fneg(value);
            stack.push(canonical_float(builder, value));
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
            stack.push(canonical_float(builder, value));
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
            stack.push(builder.ins().uextend(types::I64, compared));
        }
        _ => {
            return Err(CompileError::Unsupported(
                UnsupportedReason::UnsupportedInstruction,
            ))
        }
    }
    Ok(())
}

fn float_value(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    builder.ins().bitcast(types::F64, MemFlags::new(), bits)
}

fn canonical_float(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
    let canonical = builder.ins().iconst(types::I64, CANONICAL_NAN_BITS as i64);
    builder.ins().select(is_nan, canonical, bits)
}

fn emit_exit(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    emit_spill_frame(builder, values, exit.block, exit.instruction, stack)?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
        exit.retired,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, kind),
        exit.kind,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, block),
        exit.block,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, instruction),
        exit.instruction,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, stack_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, result),
        exit.result,
    )?;
    builder.ins().return_(&[]);
    Ok(())
}

fn emit_spill_frame(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    for (slot, variable) in values.locals.iter().copied().enumerate() {
        let value = builder.use_var(variable);
        let state = builder.use_var(values.local_states[slot]);
        let local_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value, values.local_pointer, local_offset);
        builder.ins().store(
            MemFlags::new(),
            state,
            values.local_state_pointer,
            state_offset,
        );
    }
    for (slot, value) in stack.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value, values.stack_pointer, offset);
    }
    let frame = emit_current_frame_pointer(builder, values)?;
    store_i32_constant(
        builder,
        frame,
        mem::offset_of!(RawNativeFrame, block),
        block,
    )?;
    store_i32_constant(
        builder,
        frame,
        mem::offset_of!(RawNativeFrame, instruction),
        instruction,
    )?;
    store_i32_constant(
        builder,
        frame,
        mem::offset_of!(RawNativeFrame, operand_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )?;
    Ok(())
}

fn emit_current_frame_pointer(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<ir::Value, CompileError> {
    let frames = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, frames),
    )?;
    let frame_len = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, frame_len),
    )?;
    let frame_index = builder.ins().iadd_imm(frame_len, -1);
    let frame_index = builder.ins().uextend(values.pointer_type, frame_index);
    let offset = builder
        .ins()
        .imul_imm(frame_index, mem::size_of::<RawNativeFrame>() as i64);
    Ok(builder.ins().iadd(frames, offset))
}

fn define_stack(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &[ir::Value],
) -> Result<(), CompileError> {
    if stack.len() > values.stack.len() {
        return Err(CompileError::Backend);
    }
    for (variable, value) in values.stack.iter().copied().zip(stack.iter().copied()) {
        builder.def_var(variable, value);
    }
    Ok(())
}

fn store_i32_constant(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: u32,
) -> Result<(), CompileError> {
    let value = builder.ins().iconst(types::I32, i64::from(value));
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

#[derive(Clone, Copy)]
enum RawActivationField {
    Scalars,
    States,
    ScalarLen,
    ScalarCapacity,
    Frames,
    FrameLen,
    FrameCapacity,
    StackValues,
    MaxStackValues,
    BaseFrames,
    MaxFrames,
}

impl RawActivationField {
    fn offset(self) -> usize {
        match self {
            RawActivationField::Scalars => mem::offset_of!(RawNativeActivation, scalars),
            RawActivationField::States => mem::offset_of!(RawNativeActivation, states),
            RawActivationField::ScalarLen => mem::offset_of!(RawNativeActivation, scalar_len),
            RawActivationField::ScalarCapacity => {
                mem::offset_of!(RawNativeActivation, scalar_capacity)
            }
            RawActivationField::Frames => mem::offset_of!(RawNativeActivation, frames),
            RawActivationField::FrameLen => mem::offset_of!(RawNativeActivation, frame_len),
            RawActivationField::FrameCapacity => {
                mem::offset_of!(RawNativeActivation, frame_capacity)
            }
            RawActivationField::StackValues => {
                mem::offset_of!(RawNativeActivation, stack_values)
            }
            RawActivationField::MaxStackValues => {
                mem::offset_of!(RawNativeActivation, max_stack_values)
            }
            RawActivationField::BaseFrames => {
                mem::offset_of!(RawNativeActivation, base_frames)
            }
            RawActivationField::MaxFrames => mem::offset_of!(RawNativeActivation, max_frames),
        }
    }
}

fn load_activation_u32(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
) -> Result<ir::Value, CompileError> {
    load_value(
        builder,
        types::I32,
        values.activation_pointer,
        field.offset(),
    )
}

fn load_activation_pointer(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
) -> Result<ir::Value, CompileError> {
    load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        field.offset(),
    )
}

fn store_activation_u32(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
    value: ir::Value,
) -> Result<(), CompileError> {
    store_i32_value(builder, values.activation_pointer, field.offset(), value)
}

fn load_cell_u32(
    builder: &mut FunctionBuilder<'_>,
    cell: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value(builder, types::I32, cell, offset)
}

fn store_i32_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

fn load_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    Ok(builder.ins().load(ty, MemFlags::new(), pointer, offset))
}

fn store_i64(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}
