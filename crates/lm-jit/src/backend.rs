//! Cranelift emission for one immutable native region plan.

use crate::activation::{
    NativeFunction, RawExit, RawNativeActivation, RawNativeFrame, RawNativeFunctions,
    RawTypeEnvironmentCacheEntry, RUNTIME_HEAP_LIMIT, RUNTIME_OK, TYPE_ENVIRONMENT_CACHE_WAYS,
};
use crate::plan::{
    CallContract, FunctionDefinition, HeapAccessKind, InlineFunctionPlan, ObjectContract,
    OptionAccessKind, OptionTarget, RegionPlan, Segment, SegmentExit, UnsupportedReason,
    ValueContract,
};
use crate::{
    CompiledRegion, FunctionInput, NativeEntryCell, ScalarKind, TypeEnvironmentSite,
    EXIT_ALLOCATION, EXIT_CALL, EXIT_DIVIDE_BY_ZERO, EXIT_EFFECT, EXIT_FUEL, EXIT_GROW_ACTIVATION,
    EXIT_HEAP_LIMIT, EXIT_INTEGER_OVERFLOW, EXIT_INTERPRETER, EXIT_INVALID_ENTRY, EXIT_LITERAL,
    EXIT_REPLAY, EXIT_RETURN, EXIT_STACK_LIMIT, EXIT_TYPE_ENVIRONMENT, EXIT_TYPE_MISMATCH,
    EXIT_TYPE_RESOLUTION, EXIT_UNINITIALIZED_FIELD, EXIT_UNREACHABLE, LOCAL_DIRTY,
    LOCAL_INITIALIZED,
};
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, InstBuilder, MemFlags,
    UserFuncName,
};
use cranelift_codegen::isa::CallConv;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module as _};
use lm_bytecode::{ExtendedInstr, Func, Instr, NativeInstr, NumericInstr};
use lm_heap::{
    JIT_BYTES_DATA_OFFSET, JIT_BYTES_LEN_OFFSET, JIT_ENTRY_BYTES_OFFSET, JIT_ENTRY_FROZEN_OFFSET,
    JIT_ENTRY_GENERATION_OFFSET, JIT_ENTRY_LIVE_OFFSET, JIT_ENTRY_LIVE_TAG,
    JIT_ENTRY_OBJECT_TAG_OFFSET, JIT_ENTRY_SIZE, JIT_INSTANCE_CLASS_OFFSET,
    JIT_INSTANCE_FIELDS_OFFSET, JIT_LIST_EPOCH_OFFSET, JIT_LIST_ITEMS_OFFSET, JIT_OBJECT_BYTES,
    JIT_OBJECT_CLOSURE, JIT_OBJECT_INSTANCE, JIT_OBJECT_LIST, JIT_OBJECT_MAP, JIT_OBJECT_STR,
    JIT_OBJECT_SUBSTRING, JIT_OBJECT_TUPLE, JIT_PAGE_MASK, JIT_PAGE_SHIFT,
    JIT_TEXT_BYTE_LEN_OFFSET, JIT_TEXT_SCALAR_LEN_OFFSET, JIT_TUPLE_ITEMS_OFFSET,
    VALUE_ARRAY_CAPACITY_OFFSET, VALUE_ARRAY_DATA_OFFSET, VALUE_ARRAY_LEN_OFFSET,
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
    let type_environment_sites: Vec<TypeEnvironmentSite> =
        plan.segments
            .iter()
            .filter_map(|segment| {
                let instruction = input
                    .root
                    .runtime
                    .blocks
                    .get(segment.block as usize)?
                    .get(segment.end.checked_sub(1)? as usize)?;
                let application = match instruction {
                    lm_bytecode::Instr::CallG { app, .. }
                    | lm_bytecode::Instr::NewG { app, .. } => *app,
                    _ => return None,
                };
                Some(TypeEnvironmentSite {
                    function: input.root.function,
                    block: segment.block,
                    instruction: segment.end.checked_sub(1)?,
                    application,
                })
            })
            .collect();

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
    flags
        .set("preserve_frame_pointers", "true")
        .map_err(|_| CompileError::Backend)?;
    let isa = cranelift_native::builder()
        .map_err(|_| CompileError::Backend)?
        .finish(settings::Flags::new(flags))
        .map_err(|_| CompileError::Backend)?;
    let pointer_type = isa.pointer_type();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let mut entry_signature = module.make_signature();
    append_native_parameters(&mut entry_signature, pointer_type);
    let host_call_conv = entry_signature.call_conv;
    let mut body_signature = entry_signature.clone();
    body_signature.call_conv = CallConv::Tail;
    body_signature.params.push(AbiParam::new(types::I64));
    body_signature.params.push(AbiParam::new(types::I32));
    let body_id = module
        .declare_function("loom_native_body", Linkage::Local, &body_signature)
        .map_err(|_| CompileError::Backend)?;
    let entry_id = module
        .declare_function("loom_native_entry", Linkage::Local, &entry_signature)
        .map_err(|_| CompileError::Backend)?;
    let mut body_context = module.make_context();
    body_context.func.signature = body_signature;
    body_context.func.name = UserFuncName::user(0, body_id.as_u32());
    let mut frontend = FunctionBuilderContext::new();
    emit_region(
        &mut body_context.func,
        &mut frontend,
        pointer_type,
        host_call_conv,
        &plan,
        &input,
        &type_environment_sites,
    )?;
    module
        .define_function(body_id, &mut body_context)
        .map_err(|_| CompileError::Backend)?;
    let mut entry_context = module.make_context();
    entry_context.func.signature = entry_signature;
    entry_context.func.name = UserFuncName::user(0, entry_id.as_u32());
    let body_reference = module.declare_func_in_func(body_id, &mut entry_context.func);
    let mut entry_frontend = FunctionBuilderContext::new();
    emit_entry_wrapper(&mut entry_context.func, &mut entry_frontend, body_reference)?;
    module
        .define_function(entry_id, &mut entry_context)
        .map_err(|_| CompileError::Backend)?;
    module
        .finalize_definitions()
        .map_err(|_| CompileError::Backend)?;
    let entry_code = module.get_finalized_function(entry_id);
    let body_code = module.get_finalized_function(body_id);
    // SAFETY: The generated function uses the exact `NativeFunction` C ABI.
    // `CompiledRegion` retains the module that owns the executable memory.
    let entry = unsafe { mem::transmute::<*const u8, NativeFunction>(entry_code) };
    let call_entry = body_code as usize;
    Ok(CompiledRegion {
        function: input.root.function,
        plan,
        entry,
        call_entry,
        type_environment_sites,
        module: Mutex::new(Some(module)),
    })
}

fn append_native_parameters(signature: &mut ir::Signature, pointer_type: ir::Type) {
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(types::I64));
    signature.params.push(AbiParam::new(types::I32));
    for _ in 0..8 {
        signature.params.push(AbiParam::new(pointer_type));
    }
}

fn emit_entry_wrapper(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
    body: ir::FuncRef,
) -> Result<(), CompileError> {
    let mut builder = FunctionBuilder::new(function, frontend);
    let entry = builder.create_block();
    builder.switch_to_block(entry);
    builder.append_block_params_for_function_params(entry);
    let mut arguments = builder.block_params(entry).to_vec();
    let activation = *arguments.get(14).ok_or(CompileError::Backend)?;
    let frame_len = builder.ins().load(
        types::I32,
        MemFlags::new(),
        activation,
        i32::try_from(mem::offset_of!(RawNativeActivation, frame_len))
            .map_err(|_| CompileError::Backend)?,
    );
    let detached = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, frame_len, 1);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    let one_i32 = builder.ins().iconst(types::I32, 1);
    let detached = builder.ins().select(detached, one_i32, zero_i32);
    arguments.push(zero_i64);
    arguments.push(detached);
    builder.ins().call(body, &arguments);
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

#[derive(Clone, Copy)]
struct NativeValues<'a> {
    locals: &'a [Variable],
    local_tags: &'a [Variable],
    local_states: &'a [Variable],
    stack: &'a [Variable],
    stack_tags: &'a [Variable],
    fuel: Variable,
    retired: Variable,
    local_pointer: ir::Value,
    local_tag_pointer: ir::Value,
    local_state_pointer: ir::Value,
    stack_pointer: ir::Value,
    stack_tag_pointer: ir::Value,
    runtime_context: ir::Value,
    runtime_functions: ir::Value,
    allocation_result_pointer: ir::Value,
    root_pointer: ir::Value,
    root_tag_pointer: ir::Value,
    root_state_pointer: ir::Value,
    allocation_signature: ir::SigRef,
    list_growth_signature: ir::SigRef,
    list_reserve_signature: ir::SigRef,
    native_signature: ir::SigRef,
    exit_pointer: ir::Value,
    activation_pointer: ir::Value,
    detached_return: ir::Value,
    pointer_type: ir::Type,
}

#[derive(Clone, Copy)]
struct NativeRoot {
    bits: ir::Value,
    tag: ir::Value,
    state: Option<ir::Value>,
}

#[derive(Clone, Copy)]
struct NativeValue {
    bits: ir::Value,
    tag: ir::Value,
}

#[derive(Clone, Copy)]
struct ExitEmission {
    retired: ir::Value,
    kind: u32,
    block: u32,
    instruction: u32,
    result: NativeValue,
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
    deopt_stack: &'a [NativeValue],
}

#[derive(Clone, Copy)]
struct HeapExitEmission<'a> {
    point: FaultPoint,
    fault_stack: &'a [NativeValue],
    deopt_stack: &'a [NativeValue],
}

#[derive(Clone, Copy)]
struct NumericExitEmission<'a> {
    point: FaultPoint,
    deopt_stack: &'a [NativeValue],
}

struct StoreFieldEmission<'a> {
    field: u32,
    receiver_class: u32,
    contract: ValueContract,
    exit: HeapExitEmission<'a>,
}

#[derive(Clone, Copy)]
enum ObjectGuard<'a> {
    Fault(&'a [NativeValue]),
    Replay(&'a [NativeValue]),
}

struct NativeCallEmission<'a> {
    target: u32,
    environment: ir::Value,
    contract: &'a CallContract,
    block: u32,
    instruction: u32,
    successor_entry: u32,
    successor: ir::Block,
}

struct SegmentEmission<'a, 'b> {
    bytecode: &'a Func,
    segment: &'a Segment,
    blocks: &'a [ir::Block],
    values: NativeValues<'a>,
    plan: &'a RegionPlan,
    input: &'a FunctionInput<'b>,
    type_environment_sites: &'a [TypeEnvironmentSite],
    exact_fuel: bool,
    resume_blocks: Option<&'a [ir::Block]>,
}

fn emit_region(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
    pointer_type: ir::Type,
    host_call_conv: CallConv,
    plan: &RegionPlan,
    input: &FunctionInput<'_>,
    type_environment_sites: &[TypeEnvironmentSite],
) -> Result<(), CompileError> {
    let bytecode = input.root.runtime;
    let call_conv = function.signature.call_conv;
    let mut builder = FunctionBuilder::new(function, frontend);
    let mut allocation_signature = ir::Signature::new(host_call_conv);
    allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature.params.push(AbiParam::new(types::I32));
    allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    allocation_signature.returns.push(AbiParam::new(types::I32));
    let allocation_signature = builder.import_signature(allocation_signature);
    let mut list_growth_signature = ir::Signature::new(host_call_conv);
    list_growth_signature
        .params
        .push(AbiParam::new(pointer_type));
    list_growth_signature.params.push(AbiParam::new(types::I64));
    list_growth_signature.params.push(AbiParam::new(types::I64));
    list_growth_signature.params.push(AbiParam::new(types::I64));
    list_growth_signature.params.push(AbiParam::new(types::I32));
    list_growth_signature
        .returns
        .push(AbiParam::new(types::I32));
    let list_growth_signature = builder.import_signature(list_growth_signature);
    let mut list_reserve_signature = ir::Signature::new(host_call_conv);
    list_reserve_signature
        .params
        .push(AbiParam::new(pointer_type));
    list_reserve_signature
        .params
        .push(AbiParam::new(types::I64));
    list_reserve_signature
        .params
        .push(AbiParam::new(types::I64));
    list_reserve_signature
        .params
        .push(AbiParam::new(types::I32));
    list_reserve_signature
        .returns
        .push(AbiParam::new(types::I32));
    let list_reserve_signature = builder.import_signature(list_reserve_signature);
    let mut native_signature = ir::Signature::new(call_conv);
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(types::I64));
    native_signature.params.push(AbiParam::new(types::I32));
    for _ in 0..8 {
        native_signature.params.push(AbiParam::new(pointer_type));
    }
    native_signature.params.push(AbiParam::new(types::I64));
    native_signature.params.push(AbiParam::new(types::I32));
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
    let local_tag_pointer = parameters[1];
    let local_state_pointer = parameters[2];
    let stack_pointer = parameters[3];
    let stack_tag_pointer = parameters[4];
    let initial_fuel = parameters[5];
    let entry = parameters[6];
    let runtime_context = parameters[7];
    let runtime_functions = parameters[8];
    let allocation_result_pointer = parameters[9];
    let root_pointer = parameters[10];
    let root_tag_pointer = parameters[11];
    let root_state_pointer = parameters[12];
    let exit_pointer = parameters[13];
    let activation_pointer = parameters[14];
    let retired_base = parameters[15];
    let detached_return = parameters[16];

    let mut locals = Vec::with_capacity(plan.local_kinds.len());
    let mut local_tags = Vec::with_capacity(plan.local_kinds.len());
    let mut local_states = Vec::with_capacity(plan.local_kinds.len());
    for slot in 0..plan.local_kinds.len() {
        let local = builder.declare_var(types::I64);
        let tag = builder.declare_var(types::I64);
        let state = builder.declare_var(types::I8);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), local_pointer, offset);
        let value_tag = builder
            .ins()
            .load(types::I64, MemFlags::new(), local_tag_pointer, offset);
        let local_state = builder.ins().load(
            types::I8,
            MemFlags::new(),
            local_state_pointer,
            state_offset,
        );
        builder.def_var(local, value);
        builder.def_var(tag, value_tag);
        builder.def_var(state, local_state);
        locals.push(local);
        local_tags.push(tag);
        local_states.push(state);
    }
    let mut stack = Vec::with_capacity(plan.max_stack);
    let mut stack_tags = Vec::with_capacity(plan.max_stack);
    for slot in 0..plan.max_stack {
        let variable = builder.declare_var(types::I64);
        let tag = builder.declare_var(types::I64);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), stack_pointer, offset);
        let value_tag = builder
            .ins()
            .load(types::I64, MemFlags::new(), stack_tag_pointer, offset);
        builder.def_var(variable, value);
        builder.def_var(tag, value_tag);
        stack.push(variable);
        stack_tags.push(tag);
    }
    let fuel = builder.declare_var(types::I64);
    let retired = builder.declare_var(types::I64);
    builder.def_var(fuel, initial_fuel);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.def_var(retired, retired_base);
    let values = NativeValues {
        locals: &locals,
        local_tags: &local_tags,
        local_states: &local_states,
        stack: &stack,
        stack_tags: &stack_tags,
        fuel,
        retired,
        local_pointer,
        local_tag_pointer,
        local_state_pointer,
        stack_pointer,
        stack_tag_pointer,
        runtime_context,
        runtime_functions,
        allocation_result_pointer,
        root_pointer,
        root_tag_pointer,
        root_state_pointer,
        allocation_signature,
        list_growth_signature,
        list_reserve_signature,
        native_signature,
        exit_pointer,
        activation_pointer,
        detached_return,
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
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
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
            let entry_stack: Vec<NativeValue> = values
                .stack
                .iter()
                .copied()
                .zip(values.stack_tags.iter().copied())
                .take(segment.entry_stack.len())
                .map(|(bits, tag)| NativeValue {
                    bits: builder.use_var(bits),
                    tag: builder.use_var(tag),
                })
                .collect();
            emit_exit(
                &mut builder,
                values,
                ExitEmission {
                    retired: retired_value,
                    kind: EXIT_FUEL,
                    block: segment.block,
                    instruction: segment.start,
                    result: NativeValue {
                        bits: result,
                        tag: result,
                    },
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
                    type_environment_sites,
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
                type_environment_sites,
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
        type_environment_sites,
        exact_fuel,
        resume_blocks,
    } = emission;
    let mut stack: Vec<NativeValue> = if resume_blocks.is_some() {
        Vec::new()
    } else {
        values
            .stack
            .iter()
            .copied()
            .zip(values.stack_tags.iter().copied())
            .take(segment.entry_stack.len())
            .map(|(bits, tag)| NativeValue {
                bits: builder.use_var(bits),
                tag: builder.use_var(tag),
            })
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
                .copied()
                .zip(values.stack_tags.iter().copied())
                .take(kinds.len())
                .map(|(bits, tag)| NativeValue {
                    bits: builder.use_var(bits),
                    tag: builder.use_var(tag),
                })
                .collect();
        }
        let prefix = within as u32 + 1;
        let deferred_boundary = within + 1 == code.len()
            && matches!(
                segment.exit,
                SegmentExit::Call { .. }
                    | SegmentExit::Effect { .. }
                    | SegmentExit::Interpreter { .. }
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
                push_static(builder, &mut stack, ScalarKind::Unit, value)?;
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
                    if matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            tag: builder.use_var(values.local_tags[slot]),
                            state: Some(builder.use_var(values.local_states[slot])),
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
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
                            prefix: if exact_fuel { 0 } else { prefix - 1 },
                        },
                        &stack,
                    )?
                } else {
                    builder.ins().iconst(types::I32, 0)
                };
                let value = emit_allocate_instance(
                    builder,
                    values,
                    class,
                    environment,
                    &roots,
                    FaultPoint {
                        block: segment.block,
                        instruction: position + 1,
                        prefix: fault_prefix,
                    },
                    &stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), value)?;
            }
            Instr::ConstBool(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Bool, value)?;
            }
            Instr::ConstInt(value) => {
                let value = builder.ins().iconst(types::I64, value);
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::ConstFloat(bits) => {
                let value = builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64);
                push_static(builder, &mut stack, ScalarKind::Float, value)?;
            }
            Instr::ConstChar(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::ConstStr(index) => {
                let instruction = segment.start + within as u32;
                let value = emit_literal_load(
                    builder,
                    values,
                    index as usize,
                    FaultPoint {
                        block: segment.block,
                        instruction,
                        prefix: if exact_fuel { 0 } else { prefix - 1 },
                    },
                    &stack,
                )?;
                stack.push(value);
            }
            Instr::ConstBytes(index) => {
                let literal = input
                    .runtime_string_count()
                    .checked_add(index as usize)
                    .ok_or(CompileError::Backend)?;
                let instruction = segment.start + within as u32;
                let value = emit_literal_load(
                    builder,
                    values,
                    literal,
                    FaultPoint {
                        block: segment.block,
                        instruction,
                        prefix: if exact_fuel { 0 } else { prefix - 1 },
                    },
                    &stack,
                )?;
                stack.push(value);
            }
            Instr::OpConst(operation) => {
                let value = builder.ins().iconst(types::I64, i64::from(operation));
                push_static(builder, &mut stack, ScalarKind::Operation, value)?;
            }
            Instr::LoadLocal(slot) => {
                stack.push(NativeValue {
                    bits: builder.use_var(values.locals[slot as usize]),
                    tag: builder.use_var(values.local_tags[slot as usize]),
                });
            }
            Instr::StoreLocal(slot) => {
                let value = pop_value(&mut stack)?;
                builder.def_var(values.locals[slot as usize], value.bits);
                builder.def_var(values.local_tags[slot as usize], value.tag);
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
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::LoadField {
                    receiver_class,
                    value,
                } = access.kind
                else {
                    return Err(CompileError::Backend);
                };
                let value = emit_load_field(
                    builder,
                    values,
                    reference,
                    field,
                    receiver_class,
                    value,
                    HeapExitEmission {
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
            Instr::StoreField(field) => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::StoreField {
                    receiver_class,
                    value,
                } = access.kind
                else {
                    return Err(CompileError::Backend);
                };
                emit_store_field(
                    builder,
                    values,
                    reference,
                    stored,
                    StoreFieldEmission {
                        field,
                        receiver_class,
                        contract: value,
                        exit: HeapExitEmission {
                            point: FaultPoint {
                                block: segment.block,
                                instruction: instruction + 1,
                                prefix: fault_prefix,
                            },
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                    },
                )?;
            }
            Instr::TupleGet(index) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::TupleGet { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let value = emit_tuple_get(
                    builder,
                    values,
                    reference,
                    index,
                    value,
                    HeapExitEmission {
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
            Instr::Extended(ExtendedInstr::OptionSome { .. }) => {
                let value = pop_value(&mut stack)?;
                stack.push(value);
            }
            Instr::Extended(ExtendedInstr::OptionNone { .. }) => {
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, OptionAccessKind::None) {
                    return Err(CompileError::Backend);
                }
                let family = emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: if exact_fuel { 0 } else { prefix - 1 },
                    },
                    &stack,
                )?;
                let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
                let payload = builder.ins().bor(family, arm);
                let tag = builder
                    .ins()
                    .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
                stack.push(NativeValue { bits: payload, tag });
            }
            Instr::Extended(ExtendedInstr::OptionPayload { .. }) => {
                let instruction_index = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
                let access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::Payload { value: contract } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let family = emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: if exact_fuel { 0 } else { prefix - 1 },
                    },
                    &deopt_stack,
                )?;
                let exact_none = emit_exact_option_none(builder, value, family);
                emit_fault_check(
                    builder,
                    values,
                    exact_none,
                    EXIT_TYPE_MISMATCH,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                emit_native_value_contract(
                    builder,
                    values,
                    value,
                    contract,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                stack.push(value);
            }
            Instr::Extended(ExtendedInstr::ListGet { .. }) => {
                let instruction_index = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::ListGet { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let result = emit_list_get(
                    builder,
                    values,
                    input.root.function,
                    reference,
                    index,
                    value,
                    access.family_type,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction_index + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index,
                        prefix: if exact_fuel { 0 } else { prefix - 1 },
                    },
                )?;
                stack.push(result);
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let option = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied();
                if let Some(access) = option {
                    let target = match access.kind {
                        OptionAccessKind::IsType { target }
                        | OptionAccessKind::CastType { target } => target,
                        _ => return Err(CompileError::Backend),
                    };
                    let family = emit_option_family(
                        builder,
                        values,
                        input.root.function,
                        access.family_type,
                        FaultPoint {
                            block: segment.block,
                            instruction: instruction_index,
                            prefix: if exact_fuel { 0 } else { prefix - 1 },
                        },
                        &deopt_stack,
                    )?;
                    let exact_none = emit_exact_option_none(builder, value, family);
                    let matches = match target {
                        OptionTarget::Family => builder.ins().iconst(types::I8, 1),
                        OptionTarget::Some => builder.ins().bxor_imm(exact_none, 1),
                        OptionTarget::None => exact_none,
                    };
                    if matches!(instruction, Instr::IsType(_)) {
                        let result = builder.ins().uextend(types::I64, matches);
                        push_static(builder, &mut stack, ScalarKind::Bool, result)?;
                    } else {
                        let mismatch = builder.ins().bxor_imm(matches, 1);
                        emit_interpreter_replay(
                            builder,
                            values,
                            mismatch,
                            FaultPoint {
                                block: segment.block,
                                instruction: instruction_index + 1,
                                prefix: fault_prefix,
                            },
                            &deopt_stack,
                        )?;
                        stack.push(value);
                    }
                } else {
                    let access = segment
                        .heap_accesses
                        .iter()
                        .find(|access| access.instruction == instruction_index)
                        .copied()
                        .ok_or(CompileError::Backend)?;
                    let target_class = match access.kind {
                        HeapAccessKind::IsType { target_class }
                        | HeapAccessKind::CastType { target_class } => target_class,
                        _ => return Err(CompileError::Backend),
                    };
                    let point = FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    };
                    let entry = emit_object_entry(
                        builder,
                        values,
                        value.bits,
                        JIT_OBJECT_INSTANCE,
                        point,
                        ObjectGuard::Replay(&deopt_stack),
                    )?;
                    let actual = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
                    let matches = emit_class_matches(builder, values, actual, target_class)?;
                    if matches!(instruction, Instr::IsType(_)) {
                        let result = builder.ins().uextend(types::I64, matches);
                        push_static(builder, &mut stack, ScalarKind::Bool, result)?;
                    } else {
                        let mismatch = builder.ins().bxor_imm(matches, 1);
                        emit_interpreter_replay(builder, values, mismatch, point, &deopt_stack)?;
                        stack.push(value);
                    }
                }
            }
            Instr::ListLen => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListLen) {
                    return Err(CompileError::Backend);
                }
                let value = emit_list_len(
                    builder,
                    values,
                    reference,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::ListAt => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::ListAt { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let value = emit_list_at(
                    builder,
                    values,
                    reference,
                    index,
                    value,
                    HeapExitEmission {
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
            Instr::Extended(ExtendedInstr::ListSet) => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::ListSet { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                emit_list_set(
                    builder,
                    values,
                    reference,
                    index,
                    stored,
                    value,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::ListPush => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::ListPush { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let root_kinds = segment
                    .replay_stacks
                    .iter()
                    .find(|(position, _)| *position == instruction)
                    .map(|(_, stack)| stack.as_slice())
                    .ok_or(CompileError::Backend)?;
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
                emit_list_push(
                    builder,
                    values,
                    reference,
                    stored,
                    value,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListReserve) => {
                let deopt_stack = stack.clone();
                let additional = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListReserve) {
                    return Err(CompileError::Backend);
                }
                let root_kinds = segment
                    .replay_stacks
                    .iter()
                    .find(|(position, _)| *position == instruction)
                    .map(|(_, stack)| stack.as_slice())
                    .ok_or(CompileError::Backend)?;
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
                emit_list_reserve(
                    builder,
                    values,
                    reference,
                    additional,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListReorder) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListReorder) {
                    return Err(CompileError::Backend);
                }
                emit_list_reorder(
                    builder,
                    values,
                    reference,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::ListCapacity) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListCapacity) {
                    return Err(CompileError::Backend);
                }
                let value = emit_list_capacity(
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::ListEpoch) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListEpoch) {
                    return Err(CompileError::Backend);
                }
                let value = emit_list_epoch(
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::ListIterLen) => {
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListIterLen) {
                    return Err(CompileError::Backend);
                }
                let value = emit_list_iter_len(
                    builder,
                    values,
                    reference,
                    expected,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), reference)?;
            }
            Instr::Native(NativeInstr::BytesLen) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::BytesAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::BytesGet) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Native(NativeInstr::StrByteLen | NativeInstr::StrCharCount) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                let result = builder.ins().bxor_imm(value, 1);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
                let value = pop_native(&mut stack)?;
                let seed = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
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
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::EqRef | Instr::NeRef => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqRef) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(operation)
                if crate::instruction_has_dedicated_treatment(&instruction) =>
            {
                emit_char_instruction(builder, &mut stack, operation)?;
            }
            Instr::Numeric(operation)
                if crate::instruction_has_dedicated_treatment(&instruction) =>
            {
                let deopt_stack = stack.clone();
                emit_numeric_instruction(
                    builder,
                    values,
                    &mut stack,
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
            Instr::Call(_)
            | Instr::CallG { .. }
            | Instr::Perform { .. }
            | Instr::PerformValue { .. }
            | Instr::TupleNew { .. }
            | Instr::ListNew { .. }
            | Instr::Jump(_)
            | Instr::JumpIfFalse(_)
            | Instr::JumpIfTrue(_)
            | Instr::Unreachable
            | Instr::Return => {}
            _ if deferred_boundary && matches!(segment.exit, SegmentExit::Interpreter { .. }) => {}
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
            let contract = segment
                .call_contract
                .as_ref()
                .ok_or(CompileError::Backend)?;
            let environment = match segment.exit {
                SegmentExit::Call {
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
                    emit_type_environment_lookup(
                        builder,
                        values,
                        site,
                        FaultPoint {
                            block: segment.block,
                            instruction: call_instruction,
                            prefix: 0,
                        },
                        &stack,
                    )?
                }
                SegmentExit::Call { app: None, .. } => builder.ins().iconst(types::I32, 0),
                _ => return Err(CompileError::Backend),
            };
            emit_native_call(
                builder,
                values,
                &mut stack,
                NativeCallEmission {
                    target,
                    environment,
                    contract,
                    block: segment.block,
                    instruction: call_instruction,
                    successor_entry: u32::try_from(segment.successors[0])
                        .map_err(|_| CompileError::Backend)?,
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
                result: NativeValue {
                    bits: zero,
                    tag: zero,
                },
            },
            &stack,
        )?;
        return Ok(());
    }

    if matches!(segment.exit, SegmentExit::Interpreter { .. }) {
        let instruction = segment.end - 1;
        emit_segment_charge(builder, values, segment.cost, exact_fuel);
        let retired = builder.use_var(values.retired);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_INTERPRETER,
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
        emit_segment_charge(builder, values, segment.cost, exact_fuel);
        let retired = builder.use_var(values.retired);
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
        SegmentExit::Interpreter { .. } => unreachable!(),
        SegmentExit::Unreachable => unreachable!(),
        SegmentExit::Return => {
            let result = pop_value(&mut stack)?;
            emit_function_return(builder, values, segment.block, segment.end, result, &stack)?;
        }
    }
    Ok(())
}

fn emit_type_environment_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    site: &TypeEnvironmentSite,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    emit_type_cache_lookup(
        builder,
        values,
        site.function,
        point,
        TypeCacheRequest::Environment,
        stack,
    )
}

#[derive(Clone, Copy)]
enum TypeCacheRequest {
    Environment,
    OptionFamily { ty: u32 },
}

fn emit_type_cache_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    point: FaultPoint,
    request: TypeCacheRequest,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let hit = builder.create_block();
    let miss = builder.create_block();
    builder.append_block_param(hit, types::I32);
    let store = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, type_store_id),
    )?;
    let frame = emit_current_frame_pointer(builder, values)?;
    let parent = load_cell_u32(builder, frame, mem::offset_of!(RawNativeFrame, environment))?;
    let cache = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, type_environments),
    )?;
    let mask = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, type_environment_mask),
    )?;
    let shifted_parent = builder.ins().ushr_imm(parent, 16);
    let parent_hash = builder.ins().bxor(parent, shifted_parent);
    let site_hash =
        crate::activation::type_environment_site_hash(function, point.block, point.instruction);
    let site_hash = builder.ins().iconst(types::I32, i64::from(site_hash));
    let set = builder.ins().bxor(site_hash, parent_hash);
    let set = builder.ins().band(set, mask);
    let set = builder.ins().uextend(values.pointer_type, set);
    let first = builder.ins().imul_imm(
        set,
        (TYPE_ENVIRONMENT_CACHE_WAYS * mem::size_of::<RawTypeEnvironmentCacheEntry>()) as i64,
    );
    let first = builder.ins().iadd(cache, first);
    for index in 0..TYPE_ENVIRONMENT_CACHE_WAYS {
        let next = builder.create_block();
        let entry_offset = index
            .checked_mul(mem::size_of::<RawTypeEnvironmentCacheEntry>())
            .ok_or(CompileError::Backend)?;
        let entry_offset = i64::try_from(entry_offset).map_err(|_| CompileError::Backend)?;
        let entry = builder.ins().iadd_imm(first, entry_offset);
        let store_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, store))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_store = builder
            .ins()
            .atomic_load(types::I64, MemFlags::new(), store_address);
        let function_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, function))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_function =
            builder
                .ins()
                .atomic_load(types::I32, MemFlags::new(), function_address);
        let block_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, block))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_block = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), block_address);
        let instruction_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, instruction))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_instruction =
            builder
                .ins()
                .atomic_load(types::I32, MemFlags::new(), instruction_address);
        let parent_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, parent))
                .map_err(|_| CompileError::Backend)?,
        );
        let cached_parent = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), parent_address);
        let child_address = builder.ins().iadd_imm(
            entry,
            i64::try_from(mem::offset_of!(RawTypeEnvironmentCacheEntry, child))
                .map_err(|_| CompileError::Backend)?,
        );
        let child = builder
            .ins()
            .atomic_load(types::I32, MemFlags::new(), child_address);
        let same_store = builder.ins().icmp(IntCC::Equal, cached_store, store);
        let same_function =
            builder
                .ins()
                .icmp_imm(IntCC::Equal, cached_function, i64::from(function));
        let same_block = builder
            .ins()
            .icmp_imm(IntCC::Equal, cached_block, i64::from(point.block));
        let same_instruction = builder.ins().icmp_imm(
            IntCC::Equal,
            cached_instruction,
            i64::from(point.instruction),
        );
        let same_parent = builder.ins().icmp(IntCC::Equal, cached_parent, parent);
        let matched = builder.ins().band(same_store, same_function);
        let matched = builder.ins().band(matched, same_block);
        let matched = builder.ins().band(matched, same_instruction);
        let matched = builder.ins().band(matched, same_parent);
        builder.ins().brif(matched, hit, &[child.into()], next, &[]);
        builder.switch_to_block(next);
    }
    builder.ins().jump(miss, &[]);

    builder.switch_to_block(miss);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(point.prefix));
    let parent_bits = builder.ins().uextend(types::I64, parent);
    let (kind, bits) = match request {
        TypeCacheRequest::Environment => (EXIT_TYPE_ENVIRONMENT, parent_bits),
        TypeCacheRequest::OptionFamily { ty } => (
            EXIT_TYPE_RESOLUTION,
            builder.ins().iconst(types::I64, i64::from(ty)),
        ),
    };
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits,
                tag: parent_bits,
            },
        },
        stack,
    )?;

    builder.switch_to_block(hit);
    Ok(builder.block_params(hit)[0])
}

fn emit_native_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    stack: &mut Vec<NativeValue>,
    call: NativeCallEmission<'_>,
) -> Result<(), CompileError> {
    let NativeCallEmission {
        target,
        environment,
        contract,
        block,
        instruction,
        successor_entry,
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
    let storage = builder.create_block();
    let grow = builder.create_block();
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
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
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
    let frames = load_activation_pointer(builder, values, RawActivationField::Frames)?;
    let active_frame_index = builder.ins().iadd_imm(frame_len, -1);
    let active_frame_index = builder
        .ins()
        .uextend(values.pointer_type, active_frame_index);
    let active_frame_offset = builder
        .ins()
        .imul_imm(active_frame_index, mem::size_of::<RawNativeFrame>() as i64);
    let active_frame = builder.ins().iadd(frames, active_frame_offset);
    let caller_prefix = load_cell_u32(
        builder,
        active_frame,
        mem::offset_of!(RawNativeFrame, caller_stack_values),
    )?;
    let active_local_count = load_cell_u32(
        builder,
        active_frame,
        mem::offset_of!(RawNativeFrame, local_count),
    )?;
    let active_values = builder.ins().iadd(caller_prefix, active_local_count);
    let active_values = builder.ins().iadd_imm(
        active_values,
        i64::try_from(boundary_stack.len()).map_err(|_| CompileError::Backend)?,
    );
    let caller_values = builder
        .ins()
        .iadd_imm(active_values, -(contract.params.len() as i64));
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
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
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
    let compatible = builder.ins().band(body_fits, local_count_matches);
    builder.ins().brif(compatible, storage, &[], fallback, &[]);

    builder.switch_to_block(storage);
    let storage_fits = builder.ins().band(frame_fits, scalars_fit);
    builder.ins().brif(storage_fits, invoke, &[], grow, &[]);

    builder.switch_to_block(grow);
    let retired = builder.use_var(values.retired);
    let target_value = builder.ins().iconst(types::I64, i64::from(target));
    let required_scalars = builder.ins().uextend(types::I64, scalar_end);
    let required_scalars = builder.ins().ishl_imm(required_scalars, 32);
    let growth = builder.ins().bor(required_scalars, target_value);
    let environment_tag = builder.ins().uextend(types::I64, environment);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_GROW_ACTIVATION,
            block,
            instruction,
            result: NativeValue {
                bits: growth,
                tag: environment_tag,
            },
        },
        &boundary_stack,
    )?;

    builder.switch_to_block(fallback);
    let retired = builder.use_var(values.retired);
    let target_value = builder.ins().iconst(types::I64, i64::from(target));
    let environment_tag = builder.ins().uextend(types::I64, environment);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_CALL,
            block,
            instruction,
            result: NativeValue {
                bits: target_value,
                tag: environment_tag,
            },
        },
        &boundary_stack,
    )?;

    builder.switch_to_block(invoke);
    emit_charge(builder, values, 1);
    let prior_changed = load_activation_u32(builder, values, RawActivationField::ChangedFrom)?;
    let caller_frame = emit_current_frame_pointer(builder, values)?;
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let scalar_base = scalar_len;
    let scalar_base_pointer = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_byte_offset = builder.ins().ishl_imm(scalar_base_pointer, 3);
    let child_locals = builder.ins().iadd(scalars, scalar_byte_offset);
    let child_tags = builder.ins().iadd(tags, scalar_byte_offset);
    let child_states = builder.ins().iadd(states, scalar_base_pointer);
    let local_count_pointer = builder.ins().uextend(values.pointer_type, local_count);
    let local_byte_offset = builder.ins().ishl_imm(local_count_pointer, 3);
    let child_operands = builder.ins().iadd(child_locals, local_byte_offset);
    let child_operand_tags = builder.ins().iadd(child_tags, local_byte_offset);
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
            .store(MemFlags::new(), argument.bits, child_locals, value_offset);
        builder
            .ins()
            .store(MemFlags::new(), argument.tag, child_tags, value_offset);
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
    store_i32_value(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, environment),
        environment,
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
    store_i32_constant(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, resume_entry),
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
    let child_fuel = builder.use_var(values.fuel);
    let zero_entry = builder.ins().iconst(types::I32, 0);
    let zero_retired = builder.ins().iconst(types::I64, 0);
    builder.ins().call_indirect(
        values.native_signature,
        code,
        &[
            child_locals,
            child_tags,
            child_states,
            child_operands,
            child_operand_tags,
            child_fuel,
            zero_entry,
            values.runtime_context,
            values.runtime_functions,
            values.allocation_result_pointer,
            values.root_pointer,
            values.root_tag_pointer,
            values.root_state_pointer,
            values.exit_pointer,
            values.activation_pointer,
            zero_retired,
            zero_entry,
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
    let frame_is_earlier = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, frame_len, prior_changed);
    let changed_from = builder
        .ins()
        .select(frame_is_earlier, frame_len, prior_changed);
    store_activation_u32(
        builder,
        values,
        RawActivationField::ChangedFrom,
        changed_from,
    )?;
    emit_spill_frame_to(
        builder,
        values,
        caller_frame,
        block,
        instruction + 1,
        &caller_stack,
    )?;
    store_i32_constant(
        builder,
        caller_frame,
        mem::offset_of!(RawNativeFrame, resume_entry),
        successor_entry,
    )?;
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
    let result_tag = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        mem::offset_of!(RawExit, result_tag),
    )?;
    store_activation_u32(builder, values, RawActivationField::ScalarLen, scalar_base)?;
    store_activation_u32(builder, values, RawActivationField::FrameLen, frame_len)?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::ChangedFrom,
        prior_changed,
    )?;
    stack.truncate(argument_start);
    stack.push(NativeValue {
        bits: result,
        tag: result_tag,
    });
    define_stack(builder, values, stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

fn emit_inline_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    caller_stack: &mut Vec<NativeValue>,
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
            Instr::ConstUnit => {
                let value = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, value)?;
            }
            Instr::ConstBool(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Bool, value)?;
            }
            Instr::ConstInt(value) => {
                let value = builder.ins().iconst(types::I64, value);
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::ConstFloat(bits) => {
                let value = builder
                    .ins()
                    .iconst(types::I64, canonical_float_bits(bits) as i64);
                push_static(builder, &mut stack, ScalarKind::Float, value)?;
            }
            Instr::ConstChar(value) => {
                let value = builder.ins().iconst(types::I64, i64::from(value));
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::OpConst(operation) => {
                let value = builder.ins().iconst(types::I64, i64::from(operation));
                push_static(builder, &mut stack, ScalarKind::Operation, value)?;
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
                let value = pop_value(&mut stack)?;
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
                    if matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_)) {
                        roots.push(NativeRoot {
                            bits: builder.use_var(variable),
                            tag: builder.use_var(values.local_tags[slot]),
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
                    if initialized && matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_))
                    {
                        let value = value.ok_or(CompileError::Backend)?;
                        roots.push(NativeRoot {
                            bits: value.bits,
                            tag: value.tag,
                            state: None,
                        });
                    }
                }
                extend_stack_roots(&mut roots, &site.stack, &stack)?;
                let environment = builder.ins().iconst(types::I32, 0);
                let (status, value) =
                    emit_allocation_call(builder, values, class, environment, &roots, false)?;
                let replay = builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
                emit_fault_check(
                    builder,
                    values,
                    replay,
                    EXIT_ALLOCATION,
                    call.deopt,
                    call.deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), value)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Not => {
                let value = pop_native(&mut stack)?;
                let result = builder.ins().bxor_imm(value, 1);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
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
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
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
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::EqRef | Instr::NeRef => {
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let condition = if matches!(instruction, Instr::EqRef) {
                    IntCC::Equal
                } else {
                    IntCC::NotEqual
                };
                let compared = builder.ins().icmp(condition, left, right);
                let result = builder.ins().uextend(types::I64, compared);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(NativeInstr::HashCombine | NativeInstr::HashUnorderedCombine) => {
                let value = pop_native(&mut stack)?;
                let seed = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Native(operation) => {
                emit_char_instruction(builder, &mut stack, operation)?;
            }
            Instr::Numeric(operation) => {
                emit_numeric_instruction(
                    builder,
                    values,
                    &mut stack,
                    operation,
                    NumericExitEmission {
                        point: call.deopt,
                        deopt_stack: call.deopt_stack,
                    },
                )?;
            }
            Instr::Return => {
                let result = pop_value(&mut stack)?;
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

fn pop_value(stack: &mut Vec<NativeValue>) -> Result<NativeValue, CompileError> {
    stack.pop().ok_or(CompileError::Backend)
}

fn pop_native(stack: &mut Vec<NativeValue>) -> Result<ir::Value, CompileError> {
    Ok(pop_value(stack)?.bits)
}

fn static_value(
    builder: &mut FunctionBuilder<'_>,
    kind: ScalarKind,
    bits: ir::Value,
) -> Result<NativeValue, CompileError> {
    let tag = value_tag(kind).ok_or(CompileError::Backend)?;
    Ok(NativeValue {
        bits,
        tag: builder.ins().iconst(types::I64, tag as i64),
    })
}

fn push_static(
    builder: &mut FunctionBuilder<'_>,
    stack: &mut Vec<NativeValue>,
    kind: ScalarKind,
    bits: ir::Value,
) -> Result<(), CompileError> {
    stack.push(static_value(builder, kind, bits)?);
    Ok(())
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
    stack: &[NativeValue],
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
            result: NativeValue {
                bits: result,
                tag: result,
            },
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
    stack: &[NativeValue],
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
    stack: &[NativeValue],
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
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
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
    receiver_class: u32,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let class = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let class_matches = emit_class_matches(builder, values, class, receiver_class)?;
    let other_class = builder.ins().bxor_imm(class_matches, 1);
    emit_interpreter_replay(builder, values, other_class, exit.point, exit.deopt_stack)?;
    let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
    let value = emit_array_element(
        builder,
        values,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET,
        field_index,
        exit.point,
        exit.fault_stack,
    )?;
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
    emit_loaded_value(builder, values, value, result, exit.point, exit.deopt_stack)
}

fn emit_store_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    emission: StoreFieldEmission<'_>,
) -> Result<(), CompileError> {
    let StoreFieldEmission {
        field,
        receiver_class,
        contract,
        exit,
    } = emission;
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let class = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let class_matches = emit_class_matches(builder, values, class, receiver_class)?;
    let other_class = builder.ins().bxor_imm(class_matches, 1);
    emit_interpreter_replay(builder, values, other_class, exit.point, exit.deopt_stack)?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
    let address = emit_array_element(
        builder,
        values,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET,
        field_index,
        exit.point,
        exit.fault_stack,
    )?;
    emit_store_value(builder, address, stored, contract.kind)
}

fn emit_tuple_get(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: u32,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_TUPLE,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let index = builder.ins().iconst(values.pointer_type, i64::from(index));
    let address = emit_array_element(
        builder,
        values,
        entry,
        JIT_TUPLE_ITEMS_OFFSET,
        index,
        exit.point,
        exit.fault_stack,
    )?;
    emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )
}

fn emit_list_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

fn emit_list_capacity(
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
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        capacity
    } else {
        builder.ins().uextend(types::I64, capacity)
    })
}

fn emit_list_epoch(
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
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let unobserved = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let observed = builder.ins().select(unobserved, one, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, observed)?;
    Ok(builder.ins().uextend(types::I64, observed))
}

fn emit_list_iter_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    expected: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let expected_epoch = builder.ins().ireduce(types::I32, expected);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let changed = builder.ins().icmp(IntCC::NotEqual, epoch, expected_epoch);
    let invalid = builder.ins().bor(negative, changed);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

fn emit_seal_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let actual = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let matches = emit_class_matches(builder, values, actual, class)?;
    let mismatch = builder.ins().bxor_imm(matches, 1);
    emit_interpreter_replay(builder, values, mismatch, point, deopt_stack)?;
    let frozen = builder.ins().iconst(types::I8, 1);
    store_i8_value(builder, entry, JIT_ENTRY_FROZEN_OFFSET, frozen)
}

fn emit_list_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_list_get(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    family_type: u32,
    exit: HeapExitEmission<'_>,
    resolve: FaultPoint,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let array_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, array_index, len);
    let absent = builder.ins().bor(negative, outside);
    builder.ins().brif(absent, missing, &[], present, &[]);

    builder.switch_to_block(present);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, array_index)?;
    let value = emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(done, &[value.bits.into(), value.tag.into()]);

    builder.switch_to_block(missing);
    let family = emit_option_family(
        builder,
        values,
        function,
        family_type,
        resolve,
        exit.deopt_stack,
    )?;
    let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let payload = builder.ins().bor(family, arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(done, &[payload.into(), tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

fn emit_list_set(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    stored: NativeValue,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    emit_store_value(builder, address, stored, contract.kind)
}

fn emit_list_push(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    contract: ValueContract,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        stored.bits,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let epoch_exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(
        builder,
        values,
        epoch_exhausted,
        exit.point,
        exit.deopt_stack,
    )?;

    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    let has_capacity = builder.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), used_pointer, 0);
    let next_used = builder.ins().iadd_imm(used, VALUE_SIZE as i64);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let slow_charge = builder.ins().bor(charge_overflow, collection_due);
    let fast_charge = builder.ins().bxor_imm(slow_charge, 1);
    let fast = builder.ins().band(has_capacity, fast_charge);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, len)?;
    emit_store_value(builder, address, stored, contract.kind)?;
    let next_len = builder.ins().iadd_imm(len, 1);
    let len_offset = i32::try_from(JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET)
        .map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), next_len, entry, len_offset);
    let tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let next_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder.ins().select(tracked, next_epoch, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, next_epoch)?;
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().iadd_imm(object_bytes, VALUE_SIZE as i64);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    builder
        .ins()
        .store(MemFlags::new(), next_used, used_pointer, 0);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow_block);
    let status = emit_list_growth_call(builder, values, reference, stored, roots)?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        exit.point,
        exit.fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

fn emit_list_reserve(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, additional, 0);
    emit_interpreter_replay(builder, values, negative, exit.point, exit.deopt_stack)?;
    let native_additional = if values.pointer_type == types::I64 {
        additional
    } else {
        let too_large =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, additional, i64::from(u32::MAX));
        emit_interpreter_replay(builder, values, too_large, exit.point, exit.deopt_stack)?;
        builder.ins().ireduce(values.pointer_type, additional)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
    )?;
    let spare = builder.ins().isub(capacity, len);
    let enough = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, native_additional, spare);
    let fast = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(enough, fast, &[], slow, &[]);

    builder.switch_to_block(fast);
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow);
    let status = emit_list_reserve_call(builder, values, reference, additional, roots)?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        exit.point,
        exit.fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

fn emit_list_reorder(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, exhausted, exit.point, exit.deopt_stack)?;
    let tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let next = builder.ins().iadd_imm(epoch, 1);
    let next = builder.ins().select(tracked, next, epoch);
    store_i32_value(builder, entry, JIT_LIST_EPOCH_OFFSET, next)
}

fn emit_bytes_len(
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

fn emit_text_len(
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
    )?;
    let len = load_value(builder, values.pointer_type, entry, offset)?;
    Ok(if values.pointer_type == types::I64 {
        len
    } else {
        builder.ins().uextend(types::I64, len)
    })
}

fn emit_bytes_at(
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

fn emit_bytes_get(
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

fn emit_mutable_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let frozen = load_value(builder, types::I8, entry, JIT_ENTRY_FROZEN_OFFSET)?;
    let frozen = builder.ins().icmp_imm(IntCC::NotEqual, frozen, 0);
    emit_interpreter_replay(builder, values, frozen, exit.point, exit.deopt_stack)
}

fn emit_checked_list_index(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    index: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    Ok(index)
}

fn emit_array_element(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    array_offset: usize,
    index: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        array_offset + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        point,
        fault_stack,
    )?;
    emit_array_address(builder, values, entry, array_offset, index)
}

fn emit_array_address(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    array_offset: usize,
    index: ir::Value,
) -> Result<ir::Value, CompileError> {
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        array_offset + VALUE_ARRAY_DATA_OFFSET,
    )?;
    let byte_offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().iadd(data, byte_offset))
}

fn emit_option_family(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    family_type: u32,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let family = emit_type_cache_lookup(
        builder,
        values,
        function,
        point,
        TypeCacheRequest::OptionFamily { ty: family_type },
        stack,
    )?;
    Ok(builder.ins().uextend(types::I64, family))
}

fn emit_literal_load(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    literal: usize,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<NativeValue, CompileError> {
    let load = builder.create_block();
    let missing = builder.create_block();
    let ready = builder.create_block();
    let index = builder.ins().iconst(
        values.pointer_type,
        i64::try_from(literal).map_err(|_| CompileError::Backend)?,
    );
    let count = load_activation_pointer(builder, values, RawActivationField::LiteralCount)?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, count);
    builder.ins().brif(outside, missing, &[], load, &[]);

    builder.switch_to_block(load);
    let literals = load_activation_pointer(builder, values, RawActivationField::LiteralValues)?;
    let offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let address = builder.ins().iadd(literals, offset);
    let tag = load_value(builder, types::I64, address, VALUE_TAG_OFFSET)?;
    let invalid = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, ValueTag::Obj as u64 as i64);
    builder.ins().brif(invalid, missing, &[], ready, &[]);

    builder.switch_to_block(missing);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(point.prefix));
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_LITERAL,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        stack,
    )?;

    builder.switch_to_block(ready);
    let bits = load_value(builder, types::I64, address, VALUE_PAYLOAD_OFFSET)?;
    Ok(NativeValue { bits, tag })
}

fn emit_exact_option_none(
    builder: &mut FunctionBuilder<'_>,
    value: NativeValue,
    family: ir::Value,
) -> ir::Value {
    let empty = builder
        .ins()
        .icmp_imm(IntCC::Equal, value.tag, ValueTag::EmptyCase as u64 as i64);
    let stored_family = builder.ins().ireduce(types::I32, value.bits);
    let family = builder.ins().ireduce(types::I32, family);
    let same_family = builder.ins().icmp(IntCC::Equal, stored_family, family);
    let arm = builder.ins().ushr_imm(value.bits, 32);
    let none_arm = builder.ins().icmp_imm(IntCC::Equal, arm, 1);
    let exact_none = builder.ins().band(empty, same_family);
    builder.ins().band(exact_none, none_arm)
}

fn emit_native_value_contract(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: NativeValue,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    if let Some(expected_tag) = value_tag(contract.kind) {
        let replay = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, value.tag, expected_tag as u64 as i64);
        emit_interpreter_replay(builder, values, replay, point, deopt_stack)?;
    }
    if matches!(contract.kind, ScalarKind::Float) {
        emit_canonical_float_guard(builder, values, value.bits, point, deopt_stack)?;
    }
    emit_value_contract(builder, values, value.bits, contract, point, deopt_stack)
}

fn emit_loaded_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    address: ir::Value,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<NativeValue, CompileError> {
    let tag = load_value(builder, types::I64, address, VALUE_TAG_OFFSET)?;
    if let Some(expected_tag) = value_tag(contract.kind) {
        let replay = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, expected_tag as u64 as i64);
        emit_interpreter_replay(builder, values, replay, point, deopt_stack)?;
    }
    let payload = emit_value_payload(builder, values, address, contract.kind, point, deopt_stack)?;
    emit_value_contract(builder, values, payload, contract, point, deopt_stack)?;
    Ok(NativeValue { bits: payload, tag })
}

fn emit_value_contract(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    payload: ir::Value,
    contract: ValueContract,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let Some(object) = contract.object else {
        return Ok(());
    };
    if matches!(object, ObjectContract::Text) {
        emit_text_entry(
            builder,
            values,
            payload,
            point,
            ObjectGuard::Replay(deopt_stack),
        )?;
        return Ok(());
    }
    let tag = match object {
        ObjectContract::Str => JIT_OBJECT_STR,
        ObjectContract::Text => unreachable!(),
        ObjectContract::Instance(_) => JIT_OBJECT_INSTANCE,
        ObjectContract::List => JIT_OBJECT_LIST,
        ObjectContract::Map => JIT_OBJECT_MAP,
        ObjectContract::Tuple => JIT_OBJECT_TUPLE,
        ObjectContract::Closure => JIT_OBJECT_CLOSURE,
        ObjectContract::Bytes => JIT_OBJECT_BYTES,
    };
    let entry = emit_object_entry(
        builder,
        values,
        payload,
        tag,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    if let ObjectContract::Instance(class) = object {
        let actual = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
        let matches = emit_class_matches(builder, values, actual, class)?;
        let mismatch = builder.ins().bxor_imm(matches, 1);
        emit_interpreter_replay(builder, values, mismatch, point, deopt_stack)?;
    }
    Ok(())
}

fn emit_class_matches(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    actual: ir::Value,
    target: u32,
) -> Result<ir::Value, CompileError> {
    let test = builder.create_block();
    let parent = builder.create_block();
    let matched = builder.create_block();
    let missed = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(test, types::I32);
    builder.append_block_param(done, types::I8);
    builder.ins().jump(test, &[actual.into()]);

    builder.switch_to_block(test);
    let current = builder.block_params(test)[0];
    let equal = builder
        .ins()
        .icmp_imm(IntCC::Equal, current, i64::from(target));
    builder.ins().brif(equal, matched, &[], parent, &[]);

    builder.switch_to_block(parent);
    let current_index = builder.ins().uextend(values.pointer_type, current);
    let class_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, class_count),
    )?;
    let outside = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        current_index,
        class_count,
    );
    let load_parent = builder.create_block();
    builder.ins().brif(outside, missed, &[], load_parent, &[]);

    builder.switch_to_block(load_parent);
    let parents = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, class_parents),
    )?;
    let offset = builder
        .ins()
        .imul_imm(current_index, mem::size_of::<u32>() as i64);
    let address = builder.ins().iadd(parents, offset);
    let next = builder
        .ins()
        .load(types::I32, MemFlags::trusted(), address, 0);
    let at_root = builder
        .ins()
        .icmp_imm(IntCC::Equal, next, i64::from(lm_bytecode::NO_PARENT));
    builder
        .ins()
        .brif(at_root, missed, &[], test, &[next.into()]);

    builder.switch_to_block(matched);
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(done, &[one.into()]);

    builder.switch_to_block(missed);
    let zero = builder.ins().iconst(types::I8, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_store_value(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
    value: NativeValue,
    kind: ScalarKind,
) -> Result<(), CompileError> {
    let tag = match value_tag(kind) {
        Some(tag) => builder.ins().iconst(types::I64, tag as u64 as i64),
        None => value.tag,
    };
    store_i64(builder, address, VALUE_TAG_OFFSET, tag)?;
    store_i64(builder, address, VALUE_PAYLOAD_OFFSET, value.bits)
}

fn emit_object_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    object_tag: u32,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_heap_entry(builder, values, reference, point, guard)?;
    let kind = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let wrong_kind = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, kind, i64::from(object_tag));
    emit_object_guard(builder, values, wrong_kind, point, guard)?;
    Ok(entry)
}

fn emit_text_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_heap_entry(builder, values, reference, point, guard)?;
    let kind = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let string = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(JIT_OBJECT_STR));
    let substring = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(JIT_OBJECT_SUBSTRING));
    let valid = builder.ins().bor(string, substring);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_object_guard(builder, values, invalid, point, guard)?;
    Ok(entry)
}

fn emit_heap_entry(
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

fn value_tag(kind: ScalarKind) -> Option<ValueTag> {
    Some(match kind {
        ScalarKind::Unit => ValueTag::Unit,
        ScalarKind::Bool => ValueTag::Bool,
        ScalarKind::Int => ValueTag::Int,
        ScalarKind::Float => ValueTag::Float,
        ScalarKind::Char => ValueTag::Char,
        ScalarKind::Object(_) => ValueTag::Obj,
        ScalarKind::Tagged(_) => return None,
        ScalarKind::Operation => ValueTag::Op,
    })
}

fn emit_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    kind: ScalarKind,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let payload = match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool => {
            let byte = load_value(builder, types::I8, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, byte)
        }
        ScalarKind::Int | ScalarKind::Object(_) | ScalarKind::Tagged(_) => {
            load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?
        }
        ScalarKind::Char => {
            let scalar = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, scalar)
        }
        ScalarKind::Float => {
            let bits = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
            emit_canonical_float_guard(builder, values, bits, point, deopt_stack)?;
            bits
        }
        ScalarKind::Operation => {
            let operation = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
            builder.ins().uextend(types::I64, operation)
        }
    };
    Ok(payload)
}

fn emit_canonical_float_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    bits: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let exponent = builder.ins().band_imm(bits, 0x7ff0_0000_0000_0000);
    let exponent_is_nan = builder
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
    emit_interpreter_replay(builder, values, noncanonical, point, deopt_stack)
}

fn extend_stack_roots(
    roots: &mut Vec<NativeRoot>,
    kinds: &[ScalarKind],
    values: &[NativeValue],
) -> Result<(), CompileError> {
    if kinds.len() != values.len() {
        return Err(CompileError::Backend);
    }
    for (kind, value) in kinds.iter().copied().zip(values.iter().copied()) {
        if matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_)) {
            roots.push(NativeRoot {
                bits: value.bits,
                tag: value.tag,
                state: None,
            });
        }
    }
    Ok(())
}

fn collect_native_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
) -> Result<Vec<NativeRoot>, CompileError> {
    let mut roots = Vec::new();
    for (slot, (kind, variable)) in local_kinds
        .iter()
        .copied()
        .zip(values.locals.iter().copied())
        .enumerate()
    {
        if matches!(kind, ScalarKind::Object(_) | ScalarKind::Tagged(_)) {
            roots.push(NativeRoot {
                bits: builder.use_var(variable),
                tag: builder.use_var(values.local_tags[slot]),
                state: Some(builder.use_var(values.local_states[slot])),
            });
        }
    }
    extend_stack_roots(&mut roots, stack_kinds, stack)?;
    Ok(roots)
}

fn emit_allocate_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let (status, result) = emit_allocation_call(builder, values, class, environment, roots, true)?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(builder, values, heap_limit, EXIT_HEAP_LIMIT, point, stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, stack)?;
    Ok(result)
}

fn emit_allocation_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
    allow_collection: bool,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let class = builder.ins().iconst(types::I32, i64::from(class));
    let collection = builder
        .ins()
        .iconst(types::I32, i64::from(allow_collection));
    let allocate_instance = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, allocate_instance),
    )?;
    let call = builder.ins().call_indirect(
        values.allocation_signature,
        allocate_instance,
        &[
            values.runtime_context,
            class,
            environment,
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

fn emit_list_growth_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let grow_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, grow_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_growth_signature,
        grow_list,
        &[
            values.runtime_context,
            reference,
            stored.bits,
            stored.tag,
            root_count,
        ],
    );
    Ok(builder.inst_results(call)[0])
}

fn emit_list_reserve_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let reserve_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, reserve_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_reserve_signature,
        reserve_list,
        &[values.runtime_context, reference, additional, root_count],
    );
    Ok(builder.inst_results(call)[0])
}

fn emit_runtime_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
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
        builder.ins().store(
            MemFlags::new(),
            root.tag,
            values.root_tag_pointer,
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
    Ok(builder.ins().iconst(
        types::I32,
        i64::try_from(roots.len()).map_err(|_| CompileError::Backend)?,
    ))
}

fn emit_interpreter_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    replay: ir::Value,
    point: FaultPoint,
    stack: &[NativeValue],
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
            kind: EXIT_REPLAY,
            block: point.block,
            instruction: point.instruction.saturating_sub(1),
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        stack,
    )?;
    builder.switch_to_block(success);
    Ok(())
}

fn emit_numeric_instruction(
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

fn float_is_finite(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    let exponent = builder.ins().band_imm(bits, 0x7ff0_0000_0000_0000);
    builder
        .ins()
        .icmp_imm(IntCC::NotEqual, exponent, 0x7ff0_0000_0000_0000)
}

fn float_fits_int(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
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

fn emit_char_instruction(
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

fn float_value(builder: &mut FunctionBuilder<'_>, bits: ir::Value) -> ir::Value {
    builder.ins().bitcast(types::F64, MemFlags::new(), bits)
}

fn canonical_float(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
    let bits = builder.ins().bitcast(types::I64, MemFlags::new(), value);
    let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
    let canonical = builder.ins().iconst(types::I64, CANONICAL_NAN_BITS as i64);
    builder.ins().select(is_nan, canonical, bits)
}

fn emit_stable_hash_mix(builder: &mut FunctionBuilder<'_>, value: ir::Value) -> ir::Value {
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

fn emit_exit(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    stack: &[NativeValue],
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
        mem::offset_of!(RawExit, result_tag),
        exit.result.tag,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, result),
        exit.result.bits,
    )?;
    builder.ins().return_(&[]);
    Ok(())
}

fn emit_function_return(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    result: NativeValue,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let normal = builder.create_block();
    let direct = builder.create_block();
    let parent_return = builder.create_block();
    let lookup = builder.create_block();
    let tail = builder.create_block();
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let has_parent = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, frame_len, 1);
    builder
        .ins()
        .brif(has_parent, parent_return, &[], normal, &[]);

    builder.switch_to_block(parent_return);
    let detached = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, values.detached_return, 0);
    builder.ins().brif(detached, lookup, &[], direct, &[]);

    builder.switch_to_block(direct);
    let retired = builder.use_var(values.retired);
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
        retired,
    )?;
    store_i32_constant(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, kind),
        EXIT_RETURN,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, result_tag),
        result.tag,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, result),
        result.bits,
    )?;
    builder.ins().return_(&[]);

    builder.switch_to_block(lookup);
    let frames = load_activation_pointer(builder, values, RawActivationField::Frames)?;
    let parent_index = builder.ins().iadd_imm(frame_len, -2);
    let parent_index = builder.ins().uextend(values.pointer_type, parent_index);
    let parent_offset = builder
        .ins()
        .imul_imm(parent_index, mem::size_of::<RawNativeFrame>() as i64);
    let parent = builder.ins().iadd(frames, parent_offset);
    let parent_function =
        load_cell_u32(builder, parent, mem::offset_of!(RawNativeFrame, function))?;
    let entry_count = load_activation_u32(builder, values, RawActivationField::EntryCount)?;
    let function_in_range =
        builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, entry_count, parent_function);
    let load_entry = builder.create_block();
    builder
        .ins()
        .brif(function_in_range, load_entry, &[], normal, &[]);

    builder.switch_to_block(load_entry);
    let entries = load_activation_pointer(builder, values, RawActivationField::Entries)?;
    let entry_index = builder.ins().uextend(values.pointer_type, parent_function);
    let entry_offset = builder
        .ins()
        .imul_imm(entry_index, mem::size_of::<usize>() as i64);
    let entry = builder.ins().iadd(entries, entry_offset);
    let cell = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), entry, 0);
    let code = builder
        .ins()
        .atomic_load(values.pointer_type, MemFlags::new(), cell);
    let published = builder.ins().icmp_imm(IntCC::NotEqual, code, 0);
    builder.ins().brif(published, tail, &[], normal, &[]);

    builder.switch_to_block(tail);
    let child_index = builder.ins().iadd_imm(frame_len, -1);
    let child_index = builder.ins().uextend(values.pointer_type, child_index);
    let child_offset = builder
        .ins()
        .imul_imm(child_index, mem::size_of::<RawNativeFrame>() as i64);
    let child = builder.ins().iadd(frames, child_offset);
    let child_scalar_base =
        load_cell_u32(builder, child, mem::offset_of!(RawNativeFrame, scalar_base))?;
    let parent_scalar_base = load_cell_u32(
        builder,
        parent,
        mem::offset_of!(RawNativeFrame, scalar_base),
    )?;
    let parent_local_count = load_cell_u32(
        builder,
        parent,
        mem::offset_of!(RawNativeFrame, local_count),
    )?;
    let parent_operand_len = load_cell_u32(
        builder,
        parent,
        mem::offset_of!(RawNativeFrame, operand_len),
    )?;
    let parent_operand = builder.ins().iadd(parent_scalar_base, parent_local_count);
    let parent_operand = builder.ins().iadd(parent_operand, parent_operand_len);
    let parent_operand = builder.ins().uextend(values.pointer_type, parent_operand);
    let parent_operand_offset = builder.ins().ishl_imm(parent_operand, 3);
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let result_pointer = builder.ins().iadd(scalars, parent_operand_offset);
    let result_tag_pointer = builder.ins().iadd(tags, parent_operand_offset);
    builder
        .ins()
        .store(MemFlags::new(), result.bits, result_pointer, 0);
    builder
        .ins()
        .store(MemFlags::new(), result.tag, result_tag_pointer, 0);
    let next_operand_len = builder.ins().iadd_imm(parent_operand_len, 1);
    store_i32_value(
        builder,
        parent,
        mem::offset_of!(RawNativeFrame, operand_len),
        next_operand_len,
    )?;
    let next_frame_len = builder.ins().iadd_imm(frame_len, -1);
    let changed_from = load_activation_u32(builder, values, RawActivationField::ChangedFrom)?;
    let frame_is_earlier =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, next_frame_len, changed_from);
    let changed_from = builder
        .ins()
        .select(frame_is_earlier, next_frame_len, changed_from);
    store_activation_u32(
        builder,
        values,
        RawActivationField::FrameLen,
        next_frame_len,
    )?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::ChangedFrom,
        changed_from,
    )?;
    store_activation_u32(
        builder,
        values,
        RawActivationField::ScalarLen,
        child_scalar_base,
    )?;
    let parent_local_base = builder
        .ins()
        .uextend(values.pointer_type, parent_scalar_base);
    let parent_local_offset = builder.ins().ishl_imm(parent_local_base, 3);
    let parent_locals = builder.ins().iadd(scalars, parent_local_offset);
    let parent_tags = builder.ins().iadd(tags, parent_local_offset);
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let parent_states = builder.ins().iadd(states, parent_local_base);
    let parent_local_count = builder
        .ins()
        .uextend(values.pointer_type, parent_local_count);
    let parent_operand_offset = builder.ins().ishl_imm(parent_local_count, 3);
    let parent_operands = builder.ins().iadd(parent_locals, parent_operand_offset);
    let parent_operand_tags = builder.ins().iadd(parent_tags, parent_operand_offset);
    let parent_entry = load_cell_u32(
        builder,
        parent,
        mem::offset_of!(RawNativeFrame, resume_entry),
    )?;
    let fuel = builder.use_var(values.fuel);
    let retired = builder.use_var(values.retired);
    let detached = builder.ins().iconst(types::I32, 1);
    builder.ins().return_call_indirect(
        values.native_signature,
        code,
        &[
            parent_locals,
            parent_tags,
            parent_states,
            parent_operands,
            parent_operand_tags,
            fuel,
            parent_entry,
            values.runtime_context,
            values.runtime_functions,
            values.allocation_result_pointer,
            values.root_pointer,
            values.root_tag_pointer,
            values.root_state_pointer,
            values.exit_pointer,
            values.activation_pointer,
            retired,
            detached,
        ],
    );

    builder.switch_to_block(normal);
    let retired = builder.use_var(values.retired);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_RETURN,
            block,
            instruction,
            result,
        },
        stack,
    )
}

fn emit_spill_frame(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    emit_spill_frame_to(builder, values, frame, block, instruction, stack)
}

fn emit_spill_frame_to(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    block: u32,
    instruction: u32,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    for (slot, variable) in values.locals.iter().copied().enumerate() {
        let value = builder.use_var(variable);
        let tag = builder.use_var(values.local_tags[slot]);
        let state = builder.use_var(values.local_states[slot]);
        let local_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let state_offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value, values.local_pointer, local_offset);
        builder
            .ins()
            .store(MemFlags::new(), tag, values.local_tag_pointer, local_offset);
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
            .store(MemFlags::new(), value.bits, values.stack_pointer, offset);
        builder
            .ins()
            .store(MemFlags::new(), value.tag, values.stack_tag_pointer, offset);
    }
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
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if stack.len() > values.stack.len() {
        return Err(CompileError::Backend);
    }
    for ((variable, tag), value) in values
        .stack
        .iter()
        .copied()
        .zip(values.stack_tags.iter().copied())
        .zip(stack.iter().copied())
    {
        builder.def_var(variable, value.bits);
        builder.def_var(tag, value.tag);
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
    Tags,
    States,
    ScalarLen,
    ScalarCapacity,
    Frames,
    FrameLen,
    FrameCapacity,
    ChangedFrom,
    Entries,
    EntryCount,
    MaxStackValues,
    BaseFrames,
    MaxFrames,
    LiteralValues,
    LiteralCount,
}

impl RawActivationField {
    fn offset(self) -> usize {
        match self {
            RawActivationField::Scalars => mem::offset_of!(RawNativeActivation, scalars),
            RawActivationField::Tags => mem::offset_of!(RawNativeActivation, tags),
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
            RawActivationField::ChangedFrom => {
                mem::offset_of!(RawNativeActivation, changed_from)
            }
            RawActivationField::Entries => mem::offset_of!(RawNativeActivation, entries),
            RawActivationField::EntryCount => mem::offset_of!(RawNativeActivation, entry_count),
            RawActivationField::MaxStackValues => {
                mem::offset_of!(RawNativeActivation, max_stack_values)
            }
            RawActivationField::BaseFrames => {
                mem::offset_of!(RawNativeActivation, base_frames)
            }
            RawActivationField::MaxFrames => mem::offset_of!(RawNativeActivation, max_frames),
            RawActivationField::LiteralValues => {
                mem::offset_of!(RawNativeActivation, literal_values)
            }
            RawActivationField::LiteralCount => {
                mem::offset_of!(RawNativeActivation, literal_count)
            }
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

fn store_i8_value(
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
