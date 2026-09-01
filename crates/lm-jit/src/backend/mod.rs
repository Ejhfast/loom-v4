//! Cranelift emission for one immutable native region plan.

use crate::activation::{
    NativeDispatchRow, NativeFunction, NativeImageSlot, RawExit, RawNativeActivation,
    RawNativeFrame, RawNativeFunctions, RawResolvedCallCacheEntry, RawTypeEnvironmentCacheEntry,
    RawVirtualInstance, IMAGE_SLOT_CLASS, IMAGE_SLOT_EMPTY, IMAGE_SLOT_FUNCTION,
    PENDING_INSTANCE_SLOT_BASE, RESOLVED_CALL_CACHE_WAYS, RUNTIME_COLLECTION_REQUIRED,
    RUNTIME_FAULT_FLAG, RUNTIME_HEAP_LIMIT, RUNTIME_MAP_VACANT, RUNTIME_OK, RUNTIME_STACK_LIMIT,
    SCALAR_INSTANCE_SLOT_BASE, TYPE_ENVIRONMENT_CACHE_WAYS, VIRTUAL_INSTANCE_COUNT,
    VIRTUAL_INSTANCE_FIELDS,
};
use crate::plan::{
    bypasses_fuel_check, is_root_kind, transfer_virtual_instruction, CallContract,
    FunctionDefinition, HeapAccessKind, InlineFunctionPlan, ObjectContract, OptionAccessKind,
    OptionTarget, RegionPlan, ScalarFieldSource, ScalarReplacement, Segment, SegmentExit,
    UnsupportedReason, ValueCallTarget, ValueContract, VirtualReceiver,
};
use crate::{
    CallValueSite, CompiledRegion, FunctionInput, GenericVirtualCallSite, InterfaceCallSite,
    NativeEntryCell, ScalarKind, TreatmentClass, TypeEnvironmentSite, EXIT_BOUNDARY, EXIT_CALL,
    EXIT_CALLBACK_CALL, EXIT_DIVIDE_BY_ZERO, EXIT_EFFECT, EXIT_FUEL, EXIT_GENERIC_VIRTUAL_CALL,
    EXIT_GROW_ACTIVATION, EXIT_GROW_ROOTS, EXIT_GUEST_FAULT, EXIT_HEAP_LIMIT, EXIT_INLINE_CALL,
    EXIT_INTEGER_OVERFLOW, EXIT_INTERFACE_CALL, EXIT_INVALID_ENTRY, EXIT_LITERAL, EXIT_POLL,
    EXIT_REPLAY, EXIT_RETURN, EXIT_STACK_LIMIT, EXIT_STACK_ROLLOVER, EXIT_TYPE_ENVIRONMENT,
    EXIT_TYPE_MISMATCH, EXIT_TYPE_RESOLUTION, EXIT_UNINITIALIZED_FIELD, EXIT_UNREACHABLE,
    LOCAL_INITIALIZED, NATIVE_FRAME_OVERHEAD, NATIVE_STACK_BUDGET,
};
use cranelift_codegen::ir::{
    self, condcodes::FloatCC, condcodes::IntCC, types, AbiParam, AliasRegion, InstBuilder,
    MemFlags, UserFuncName,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Switch, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{default_libcall_names, Linkage, Module as _};
use lm_bytecode::{ExtendedInstr, Func, Instr, NativeInstr, NumericInstr};
use lm_heap::{
    JIT_BYTES_DATA_OFFSET, JIT_BYTES_LEN_OFFSET, JIT_BYTES_LOOKUP_HASH_OFFSET,
    JIT_BYTES_SEMANTIC_HASH_OFFSET, JIT_BYTE_BUFFER_ACTIVE_OFFSET, JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    JIT_BYTE_BUFFER_DATA_OFFSET, JIT_BYTE_BUFFER_LEN_OFFSET, JIT_CLOSURE_CAPTURES_OFFSET,
    JIT_CLOSURE_ENV_OFFSET, JIT_CLOSURE_FUNCTION_OFFSET, JIT_DIGEST_BYTES_OFFSET,
    JIT_ENTRY_BYTES_OFFSET, JIT_ENTRY_FROZEN_OFFSET, JIT_ENTRY_GENERATION_OFFSET,
    JIT_ENTRY_LIVE_OFFSET, JIT_ENTRY_LIVE_TAG, JIT_ENTRY_OBJECT_TAG_OFFSET,
    JIT_ENTRY_SHARED_KEY_OFFSET, JIT_ENTRY_SHARED_PRESENT_OFFSET, JIT_ENTRY_SIZE,
    JIT_INSTANCE_CLASS_OFFSET, JIT_INSTANCE_ENV_OFFSET, JIT_INSTANCE_FIELDS_OFFSET,
    JIT_LIST_EPOCH_OFFSET, JIT_LIST_ITEMS_OFFSET, JIT_MAP_ENTRIES_CAPACITY_OFFSET,
    JIT_MAP_ENTRIES_DATA_OFFSET, JIT_MAP_ENTRIES_LEN_OFFSET, JIT_MAP_ENTRY_COST,
    JIT_MAP_EPOCH_OFFSET, JIT_MAP_INDEX_BUILT_OFFSET, JIT_MAP_INDEX_SLOTS_DATA_OFFSET,
    JIT_MAP_INDEX_SLOTS_LEN_OFFSET, JIT_MAP_LIVE_OFFSET, JIT_OBJECT_BYTES, JIT_OBJECT_BYTE_BUFFER,
    JIT_OBJECT_CLOSURE, JIT_OBJECT_DIGEST, JIT_OBJECT_INSTANCE, JIT_OBJECT_LIST, JIT_OBJECT_MAP,
    JIT_OBJECT_STR, JIT_OBJECT_STRING_BUILDER, JIT_OBJECT_SUBSTRING, JIT_OBJECT_TUPLE,
    JIT_PAGE_MASK, JIT_PAGE_SHIFT, JIT_STRING_BUILDER_ACTIVE_OFFSET,
    JIT_STRING_BUILDER_ASCII_OFFSET, JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    JIT_STRING_BUILDER_CAPACITY_OFFSET, JIT_STRING_BUILDER_DATA_OFFSET,
    JIT_STRING_BUILDER_SCALAR_LEN_OFFSET, JIT_TEXT_BYTE_LEN_OFFSET, JIT_TEXT_DATA_OFFSET,
    JIT_TEXT_LOOKUP_HASH_OFFSET, JIT_TEXT_SCALAR_LEN_OFFSET, JIT_TEXT_SEMANTIC_HASH_OFFSET,
    JIT_TUPLE_ITEMS_OFFSET, MAP_ENTRY_KEY_OFFSET, MAP_ENTRY_SEMANTIC_HASH_OFFSET, MAP_ENTRY_SIZE,
    MAP_ENTRY_VALUE_OFFSET, MAP_SLOT_ENTRY_OFFSET, MAP_SLOT_HASH_OFFSET, MAP_SLOT_SIZE,
    MIN_OBJECT_COST, OWNED_ARRAY_DATA_OFFSET, OWNED_ARRAY_LEN_OFFSET, VALUE_ARRAY_CAPACITY_OFFSET,
    VALUE_ARRAY_DATA_OFFSET, VALUE_ARRAY_EMPTY_DATA, VALUE_ARRAY_LEN_OFFSET,
};
use lm_value::{
    canonical_float_bits, ValueTag, CANONICAL_NAN_BITS, VALUE_PAYLOAD_OFFSET, VALUE_SIZE,
    VALUE_TAG_OFFSET,
};
use std::cell::{Cell, RefCell};
use std::mem as std_mem;
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

pub(super) fn native_isa() -> Result<cranelift_codegen::isa::OwnedTargetIsa, CompileError> {
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
    cranelift_native::builder()
        .map_err(|_| CompileError::Backend)?
        .finish(settings::Flags::new(flags))
        .map_err(|_| CompileError::Backend)
}

pub(super) fn compile_region(
    input: FunctionInput<'_>,
    isa: cranelift_codegen::isa::OwnedTargetIsa,
) -> Result<CompiledRegion, CompileError> {
    let plan = RegionPlan::for_function(&input)?;
    let type_environment_sites: Vec<TypeEnvironmentSite> = plan
        .segments
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
                | lm_bytecode::Instr::NewG { app, .. }
                | lm_bytecode::Instr::Extended(
                    lm_bytecode::ExtendedInstr::CallSlot { app, .. }
                    | lm_bytecode::ExtendedInstr::NewSlot { app, .. },
                ) if *app != lm_bytecode::NO_APP => *app,
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
    let interface_call_sites: Vec<InterfaceCallSite> = plan
        .segments
        .iter()
        .filter_map(|segment| {
            let SegmentExit::InterfaceCall {
                interface,
                method,
                recv_ty,
                app,
                ..
            } = segment.exit
            else {
                return None;
            };
            let contract = segment.call_contract.as_ref()?;
            Some(InterfaceCallSite {
                function: input.root.function,
                block: segment.block,
                instruction: segment.end.checked_sub(1)?,
                interface,
                method,
                receiver_type: recv_ty,
                application: app,
                receiver_kind: *contract.params.first()?,
                parameter_count: contract.params.len(),
            })
        })
        .collect();
    let generic_virtual_call_sites: Vec<GenericVirtualCallSite> = plan
        .segments
        .iter()
        .filter_map(|segment| {
            let SegmentExit::GenericVirtualCall {
                selector,
                application,
                ..
            } = segment.exit
            else {
                return None;
            };
            let contract = segment.call_contract.as_ref()?;
            Some(GenericVirtualCallSite {
                function: input.root.function,
                block: segment.block,
                instruction: segment.end.checked_sub(1)?,
                selector,
                application,
                receiver_kind: *contract.params.first()?,
                parameter_count: contract.params.len(),
            })
        })
        .collect();
    let call_value_sites: Vec<CallValueSite> = plan
        .segments
        .iter()
        .filter_map(|segment| {
            let SegmentExit::ValueCall { .. } = segment.exit else {
                return None;
            };
            let contract = segment.call_contract.as_ref()?;
            Some(CallValueSite {
                function: input.root.function,
                block: segment.block,
                instruction: segment.end.checked_sub(1)?,
                parameter_count: contract.params.len(),
                callback: contract.value_target == Some(ValueCallTarget::Callback),
            })
        })
        .collect();

    let pointer_type = isa.pointer_type();
    let frontend_config = isa.frontend_config();
    let mut module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let mut entry_signature = module.make_signature();
    entry_signature.params.push(AbiParam::new(pointer_type));
    entry_signature.params.push(AbiParam::new(types::I32));
    let host_call_conv = entry_signature.call_conv;
    let target = BackendTarget {
        pointer_type,
        frontend_config,
        host_call_conv,
    };
    let mut body_signature = module.make_signature();
    body_signature.params.push(AbiParam::new(pointer_type));
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
        target,
        &plan,
        &input,
        &type_environment_sites,
    )?;
    module
        .define_function(body_id, &mut body_context)
        .map_err(|_| CompileError::Backend)?;
    let body_code = body_context.compiled_code().ok_or(CompileError::Backend)?;
    let body_code_size = body_code.code_buffer().len();
    let native_stack_bytes = body_code
        .buffer
        .frame_layout()
        .map_or(0, |layout| layout.frame_to_fp_offset)
        .checked_add(NATIVE_FRAME_OVERHEAD)
        .ok_or(CompileError::Unsupported(UnsupportedReason::RegionLimit))?;
    if native_stack_bytes > NATIVE_STACK_BUDGET {
        return Err(CompileError::Unsupported(UnsupportedReason::RegionLimit));
    }
    let mut entry_context = module.make_context();
    entry_context.func.signature = entry_signature;
    entry_context.func.name = UserFuncName::user(0, entry_id.as_u32());
    let mut entry_frontend = FunctionBuilderContext::new();
    emit_entry_wrapper(&mut entry_context.func, &mut entry_frontend)?;
    module
        .define_function(entry_id, &mut entry_context)
        .map_err(|_| CompileError::Backend)?;
    let entry_code_size = entry_context
        .compiled_code()
        .map(|code| code.code_buffer().len())
        .ok_or(CompileError::Backend)?;
    module
        .finalize_definitions()
        .map_err(|_| CompileError::Backend)?;
    let entry_code = module.get_finalized_function(entry_id);
    let body_code = module.get_finalized_function(body_id);
    // SAFETY: The generated function uses the exact `NativeFunction` C ABI.
    // `CompiledRegion` retains the module that owns the executable memory.
    let entry = unsafe { std_mem::transmute::<*const u8, NativeFunction>(entry_code) };
    let call_entry = body_code as usize;
    Ok(CompiledRegion {
        function: input.root.function,
        code_size: body_code_size.saturating_add(entry_code_size),
        native_stack_bytes,
        plan,
        entry,
        call_entry,
        type_environment_sites,
        interface_call_sites,
        generic_virtual_call_sites,
        call_value_sites,
        module: Mutex::new(Some(module)),
    })
}

fn emit_entry_wrapper(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
) -> Result<(), CompileError> {
    let pointer_type = function
        .signature
        .params
        .first()
        .map(|parameter| parameter.value_type)
        .ok_or(CompileError::Backend)?;
    let mut body_signature = ir::Signature::new(function.signature.call_conv);
    body_signature.params.push(AbiParam::new(pointer_type));
    body_signature.params.push(AbiParam::new(types::I64));
    body_signature.params.push(AbiParam::new(types::I32));
    let mut builder = FunctionBuilder::new(function, frontend);
    let body_signature = builder.import_signature(body_signature);
    let entry = builder.create_block();
    builder.switch_to_block(entry);
    builder.append_block_params_for_function_params(entry);
    let activation = *builder
        .block_params(entry)
        .first()
        .ok_or(CompileError::Backend)?;
    let entry_index = *builder
        .block_params(entry)
        .get(1)
        .ok_or(CompileError::Backend)?;
    let body = builder.ins().load(
        pointer_type,
        MemFlags::new(),
        activation,
        i32::try_from(std_mem::offset_of!(RawNativeActivation, root_code))
            .map_err(|_| CompileError::Backend)?,
    );
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .call_indirect(body_signature, body, &[activation, zero_i64, entry_index]);
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

#[derive(Clone, Copy)]
struct NativeValues<'a> {
    plan: &'a RegionPlan,
    locals: &'a [Variable],
    local_kinds: &'a [ScalarKind],
    dirty_locals: Option<&'a [bool]>,
    local_tags: &'a [Option<Variable>],
    local_heap_caches: &'a [Option<LocalHeapCache>],
    scalar_instances: &'a [ScalarInstanceValues],
    stack: &'a [Variable],
    stack_tags: &'a [Option<Variable>],
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
    instance_fields_signature: ir::SigRef,
    capture_allocation_signature: ir::SigRef,
    value_array_allocation_signature: ir::SigRef,
    list_growth_signature: ir::SigRef,
    list_insert_signature: ir::SigRef,
    list_reserve_signature: ir::SigRef,
    map_lookup_signature: ir::SigRef,
    map_put_discard_signature: ir::SigRef,
    map_put_commit_signature: ir::SigRef,
    map_insert_hashed_signature: ir::SigRef,
    bytes_equal_signature: ir::SigRef,
    value_equal_signature: ir::SigRef,
    object_binary_signature: ir::SigRef,
    object_unary_signature: ir::SigRef,
    digest_signature: ir::SigRef,
    heap_operation_signature: ir::SigRef,
    native_signature: ir::SigRef,
    exit_pointer: ir::Value,
    activation_pointer: ir::Value,
    replay_blocks: &'a [ReplayBlock],
    replay_failures: bool,
    inline_return: Option<ir::Block>,
    pointer_type: ir::Type,
    frontend_config: TargetFrontendConfig,
    heap_translations: &'a RefCell<HeapTranslationCache>,
}

struct ScalarInstanceValues {
    token: u64,
    active: Variable,
    fields: Vec<ScalarFieldValues>,
}

#[derive(Clone, Copy)]
struct ScalarFieldValues {
    bits: Variable,
    tag: Variable,
}

/// These variables carry one validated local reference across native backedges.
#[derive(Clone, Copy)]
struct LocalHeapCache {
    entry: Variable,
    object_kind: Variable,
    class: Variable,
    actual_class: Variable,
    list_data: Option<Variable>,
    preloaded_list_data: bool,
}

/// This compile-time map associates emitted references with their source local.
#[derive(Default)]
struct HeapTranslationCache {
    locals: Vec<(ir::Value, usize)>,
    use_cached_list_data: bool,
}

impl HeapTranslationCache {
    fn clear(&mut self) {
        self.locals.clear();
    }

    fn set_cached_list_data(&mut self, enabled: bool) {
        self.use_cached_list_data = enabled;
    }

    fn record_local(&mut self, reference: ir::Value, slot: usize) {
        self.locals.push((reference, slot));
    }

    fn forget_local(&mut self, slot: usize) {
        self.locals.retain(|(_, cached_slot)| *cached_slot != slot);
    }

    fn local(&self, reference: ir::Value) -> Option<usize> {
        self.locals
            .iter()
            .rev()
            .find_map(|(cached, slot)| (*cached == reference).then_some(*slot))
    }
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

#[derive(Clone, Copy)]
struct ReplayEmission<'a> {
    point: FaultPoint,
    deopt_stack: &'a [NativeValue],
}

struct StoreFieldEmission<'a> {
    field: u32,
    receiver_class: u32,
    contract: ValueContract,
    exit: HeapExitEmission<'a>,
}

struct LoadFieldEmission<'a> {
    field: u32,
    receiver_class: u32,
    contract: ValueContract,
    allow_pending: bool,
    exit: HeapExitEmission<'a>,
}

struct InstanceAllocationEmission<'a> {
    roots: &'a [NativeRoot],
    allow_pending: bool,
    exit: ReplayEmission<'a>,
}

struct ListOptionEmission<'a> {
    function: u32,
    result: ValueContract,
    family_type: u32,
    exit: HeapExitEmission<'a>,
    resolve: FaultPoint,
}

struct ListInsertEmission<'a> {
    reference: ir::Value,
    index: ir::Value,
    stored: NativeValue,
    contract: ValueContract,
    roots: &'a [NativeRoot],
    exit: HeapExitEmission<'a>,
}

struct CaptureAllocationEmission<'a> {
    function: u32,
    environment: ir::Value,
    capture_start: usize,
    capture_count: usize,
    roots: &'a [NativeRoot],
    callback: bool,
    point: FaultPoint,
    replay_stack: &'a [NativeValue],
    fault_stack: &'a [NativeValue],
}

#[derive(Clone, Copy)]
enum ValueArrayAllocationKind {
    Tuple,
    List,
    Map,
}

struct ValueArrayAllocationEmission<'a> {
    kind: ValueArrayAllocationKind,
    item_start: usize,
    item_count: usize,
    roots: &'a [NativeRoot],
    point: FaultPoint,
    replay_stack: &'a [NativeValue],
    fault_stack: &'a [NativeValue],
}

struct MapLookupEmission<'a> {
    reference: ir::Value,
    key: NativeValue,
    key_contract: ValueContract,
    result: MapLookupResult,
    exit: HeapExitEmission<'a>,
}

#[derive(Clone, Copy)]
enum MapLookupResult {
    Has,
    At,
    Get {
        family: ir::Value,
        value: ValueContract,
    },
}

#[derive(Clone, Copy)]
enum ObjectGuard<'a> {
    Fault(&'a [NativeValue]),
    Replay(&'a [NativeValue]),
    Branch(ir::Block),
}

struct NativeCallEmission<'a> {
    target: NativeCallTarget,
    capture: Option<NativeValue>,
    fallback: NativeCallFallback,
    contract: &'a CallContract,
    local_kinds: &'a [ScalarKind],
    boundary_kinds: &'a [ScalarKind],
    block: u32,
    instruction: u32,
    successor_entry: u32,
    successor: ir::Block,
}

struct InlineCallEmission<'a, 'b> {
    definition: FunctionDefinition<'a>,
    inline: &'b InlineFunctionPlan,
    contract: &'b CallContract,
    boundary_len: usize,
    block: u32,
    instruction: u32,
    successor: ir::Block,
}

#[derive(Clone, Copy)]
struct NativeCallTarget {
    function: ir::Value,
    environment: ir::Value,
    capture_data: ir::Value,
    capture_len: ir::Value,
    fault: Option<ir::Value>,
}

#[derive(Clone, Copy)]
enum NativeCallFallback {
    Direct,
    Replay,
}

struct SegmentEmission<'a, 'b> {
    bytecode: &'a Func,
    segment: &'a Segment,
    successor_blocks: &'a [ir::Block],
    values: NativeValues<'a>,
    plan: &'a RegionPlan,
    input: &'a FunctionInput<'b>,
    type_environment_sites: &'a [TypeEnvironmentSite],
}

struct InstructionEmission<'a, 'b, 'c, 'd> {
    builder: &'a mut FunctionBuilder<'b>,
    values: NativeValues<'c>,
    plan: &'c RegionPlan,
    input: &'c FunctionInput<'d>,
    segment: &'c Segment,
    type_environment_sites: &'c [TypeEnvironmentSite],
    stack: &'a mut Vec<NativeValue>,
    virtual_stack: &'a mut Vec<bool>,
    initialized_locals: &'a mut Vec<bool>,
    virtual_locals: &'a mut Vec<bool>,
    deferred_integer_overflow: &'a mut Option<DeferredIntegerOverflow>,
    instruction: Instr,
    within: usize,
    prefix: u32,
    fault_prefix: u32,
    prior_prefix: u32,
}

struct DeferredIntegerOverflow {
    flag: Option<ir::Value>,
    locals: Vec<NativeValue>,
    stack: Vec<NativeValue>,
}

struct ReplayBlock {
    block: ir::Block,
    used: Cell<bool>,
}

struct ReplayExitState {
    target: ir::Block,
    block: u32,
    instruction: u32,
    retired: ir::Value,
    prefix: u32,
    locals: Vec<NativeValue>,
    stack: Vec<NativeValue>,
}

#[derive(Clone, Copy)]
struct BackendTarget {
    pointer_type: ir::Type,
    frontend_config: TargetFrontendConfig,
    host_call_conv: CallConv,
}

fn emit_region(
    function: &mut ir::Function,
    frontend: &mut FunctionBuilderContext,
    target: BackendTarget,
    plan: &RegionPlan,
    input: &FunctionInput<'_>,
    type_environment_sites: &[TypeEnvironmentSite],
) -> Result<(), CompileError> {
    let bytecode = input.root.runtime;
    let BackendTarget {
        pointer_type,
        frontend_config,
        host_call_conv,
    } = target;
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
    let mut instance_fields_signature = ir::Signature::new(host_call_conv);
    instance_fields_signature
        .params
        .push(AbiParam::new(types::I32));
    instance_fields_signature
        .params
        .push(AbiParam::new(pointer_type));
    instance_fields_signature
        .returns
        .push(AbiParam::new(types::I32));
    let instance_fields_signature = builder.import_signature(instance_fields_signature);
    let mut capture_allocation_signature = ir::Signature::new(host_call_conv);
    capture_allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..6 {
        capture_allocation_signature
            .params
            .push(AbiParam::new(types::I32));
    }
    capture_allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    capture_allocation_signature
        .returns
        .push(AbiParam::new(types::I32));
    let capture_allocation_signature = builder.import_signature(capture_allocation_signature);
    let mut value_array_allocation_signature = ir::Signature::new(host_call_conv);
    value_array_allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..4 {
        value_array_allocation_signature
            .params
            .push(AbiParam::new(types::I32));
    }
    value_array_allocation_signature
        .params
        .push(AbiParam::new(pointer_type));
    value_array_allocation_signature
        .returns
        .push(AbiParam::new(types::I32));
    let value_array_allocation_signature =
        builder.import_signature(value_array_allocation_signature);
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
    let mut list_insert_signature = ir::Signature::new(host_call_conv);
    list_insert_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..4 {
        list_insert_signature.params.push(AbiParam::new(types::I64));
    }
    list_insert_signature.params.push(AbiParam::new(types::I32));
    list_insert_signature
        .returns
        .push(AbiParam::new(types::I32));
    let list_insert_signature = builder.import_signature(list_insert_signature);
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
    let mut map_lookup_signature = ir::Signature::new(host_call_conv);
    map_lookup_signature
        .params
        .push(AbiParam::new(pointer_type));
    map_lookup_signature.params.push(AbiParam::new(types::I64));
    map_lookup_signature.params.push(AbiParam::new(types::I64));
    map_lookup_signature.params.push(AbiParam::new(types::I64));
    map_lookup_signature
        .params
        .push(AbiParam::new(pointer_type));
    map_lookup_signature.returns.push(AbiParam::new(types::I32));
    let map_lookup_signature = builder.import_signature(map_lookup_signature);
    let mut map_put_discard_signature = ir::Signature::new(host_call_conv);
    map_put_discard_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..5 {
        map_put_discard_signature
            .params
            .push(AbiParam::new(types::I64));
    }
    map_put_discard_signature
        .params
        .push(AbiParam::new(types::I32));
    map_put_discard_signature
        .returns
        .push(AbiParam::new(types::I32));
    let map_put_discard_signature = builder.import_signature(map_put_discard_signature);
    let mut map_put_commit_signature = ir::Signature::new(host_call_conv);
    map_put_commit_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..7 {
        map_put_commit_signature
            .params
            .push(AbiParam::new(types::I64));
    }
    map_put_commit_signature
        .params
        .push(AbiParam::new(types::I32));
    map_put_commit_signature
        .params
        .push(AbiParam::new(types::I32));
    map_put_commit_signature
        .returns
        .push(AbiParam::new(types::I32));
    let map_put_commit_signature = builder.import_signature(map_put_commit_signature);
    let mut map_insert_hashed_signature = ir::Signature::new(host_call_conv);
    map_insert_hashed_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..7 {
        map_insert_hashed_signature
            .params
            .push(AbiParam::new(types::I64));
    }
    map_insert_hashed_signature
        .params
        .push(AbiParam::new(types::I32));
    map_insert_hashed_signature
        .returns
        .push(AbiParam::new(types::I32));
    let map_insert_hashed_signature = builder.import_signature(map_insert_hashed_signature);
    let mut bytes_equal_signature = ir::Signature::new(host_call_conv);
    for _ in 0..3 {
        bytes_equal_signature
            .params
            .push(AbiParam::new(pointer_type));
    }
    bytes_equal_signature
        .returns
        .push(AbiParam::new(types::I32));
    let bytes_equal_signature = builder.import_signature(bytes_equal_signature);
    let mut value_equal_signature = ir::Signature::new(host_call_conv);
    value_equal_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..4 {
        value_equal_signature.params.push(AbiParam::new(types::I64));
    }
    value_equal_signature
        .params
        .push(AbiParam::new(pointer_type));
    value_equal_signature
        .returns
        .push(AbiParam::new(types::I32));
    let value_equal_signature = builder.import_signature(value_equal_signature);
    let mut object_binary_signature = ir::Signature::new(host_call_conv);
    object_binary_signature
        .params
        .push(AbiParam::new(pointer_type));
    object_binary_signature
        .params
        .push(AbiParam::new(types::I64));
    object_binary_signature
        .params
        .push(AbiParam::new(types::I64));
    object_binary_signature
        .params
        .push(AbiParam::new(pointer_type));
    object_binary_signature
        .returns
        .push(AbiParam::new(types::I32));
    let object_binary_signature = builder.import_signature(object_binary_signature);
    let mut object_unary_signature = ir::Signature::new(host_call_conv);
    object_unary_signature
        .params
        .push(AbiParam::new(pointer_type));
    object_unary_signature
        .params
        .push(AbiParam::new(types::I64));
    object_unary_signature
        .params
        .push(AbiParam::new(pointer_type));
    object_unary_signature
        .returns
        .push(AbiParam::new(types::I32));
    let object_unary_signature = builder.import_signature(object_unary_signature);
    let mut digest_signature = ir::Signature::new(host_call_conv);
    digest_signature.params.push(AbiParam::new(pointer_type));
    digest_signature.params.push(AbiParam::new(types::I64));
    for _ in 0..4 {
        digest_signature.params.push(AbiParam::new(types::I32));
    }
    digest_signature.params.push(AbiParam::new(pointer_type));
    digest_signature.returns.push(AbiParam::new(types::I32));
    let digest_signature = builder.import_signature(digest_signature);
    let mut heap_operation_signature = ir::Signature::new(host_call_conv);
    heap_operation_signature
        .params
        .push(AbiParam::new(pointer_type));
    for _ in 0..3 {
        heap_operation_signature
            .params
            .push(AbiParam::new(types::I64));
    }
    heap_operation_signature
        .params
        .push(AbiParam::new(types::I32));
    heap_operation_signature
        .params
        .push(AbiParam::new(pointer_type));
    heap_operation_signature
        .returns
        .push(AbiParam::new(types::I32));
    let heap_operation_signature = builder.import_signature(heap_operation_signature);
    let mut native_signature = ir::Signature::new(call_conv);
    native_signature.params.push(AbiParam::new(pointer_type));
    native_signature.params.push(AbiParam::new(types::I64));
    native_signature.params.push(AbiParam::new(types::I32));
    let native_signature = builder.import_signature(native_signature);
    let entry_block = builder.create_block();
    let invalid_block = builder.create_block();
    let blocks: Vec<ir::Block> = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect();
    let preloads_list_data = plan.preloaded_list_data.iter().any(|preload| *preload);
    let preload_block = preloads_list_data.then(|| builder.create_block());
    let preload_failure = preloads_list_data.then(|| builder.create_block());
    let preload_entry_blocks: Vec<ir::Block> = if preloads_list_data {
        plan.segments
            .iter()
            .map(|_| builder.create_block())
            .collect()
    } else {
        Vec::new()
    };
    let preload_boundary_blocks: Vec<ir::Block> = if preloads_list_data {
        plan.segments
            .iter()
            .map(|_| builder.create_block())
            .collect()
    } else {
        Vec::new()
    };
    let body_blocks: Vec<ir::Block> = (0..plan.segments.len())
        .map(|_| builder.create_block())
        .collect();
    let replay_blocks: Vec<Vec<ReplayBlock>> = plan
        .segments
        .iter()
        .map(|segment| {
            (!segment.replay_stacks.is_empty())
                .then(|| ReplayBlock {
                    block: builder.create_block(),
                    used: Cell::new(false),
                })
                .into_iter()
                .collect()
        })
        .collect();
    builder.set_cold_block(invalid_block);
    if let Some(block) = preload_failure {
        builder.set_cold_block(block);
    }
    for block in preload_boundary_blocks.iter().copied() {
        builder.set_cold_block(block);
    }
    for replay in replay_blocks.iter().flatten() {
        builder.set_cold_block(replay.block);
    }

    builder.switch_to_block(entry_block);
    builder.append_block_params_for_function_params(entry_block);
    let parameters = builder.block_params(entry_block);
    let activation_pointer = parameters[0];
    let retired_base = parameters[1];
    let entry = parameters[2];
    let scalars = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, scalars),
    )?;
    let tags = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, tags),
    )?;
    let states = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, states),
    )?;
    let frames = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, frames),
    )?;
    let frame_len = load_value(
        &mut builder,
        types::I32,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, frame_len),
    )?;
    let frame_index = builder.ins().iadd_imm(frame_len, -1);
    let frame_index = builder.ins().uextend(pointer_type, frame_index);
    let frame_offset = builder
        .ins()
        .imul_imm(frame_index, std_mem::size_of::<RawNativeFrame>() as i64);
    let frame = builder.ins().iadd(frames, frame_offset);
    let scalar_base = load_cell_u32(
        &mut builder,
        frame,
        std_mem::offset_of!(RawNativeFrame, scalar_base),
    )?;
    let scalar_base = builder.ins().uextend(pointer_type, scalar_base);
    let scalar_byte_offset = builder.ins().ishl_imm(scalar_base, 3);
    let local_pointer = builder.ins().iadd(scalars, scalar_byte_offset);
    let local_tag_pointer = builder.ins().iadd(tags, scalar_byte_offset);
    let local_state_pointer = builder.ins().iadd(states, scalar_base);
    let local_bytes = i64::try_from(
        plan.local_kinds
            .len()
            .checked_mul(8)
            .ok_or(CompileError::Backend)?,
    )
    .map_err(|_| CompileError::Backend)?;
    let stack_pointer = builder.ins().iadd_imm(local_pointer, local_bytes);
    let stack_tag_pointer = builder.ins().iadd_imm(local_tag_pointer, local_bytes);
    let poll_deadline = load_value(
        &mut builder,
        types::I64,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, poll_deadline),
    )?;
    let initial_fuel = builder.ins().isub(poll_deadline, retired_base);
    let runtime_context = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, runtime_context),
    )?;
    let runtime_functions = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, runtime_functions),
    )?;
    let allocation_result_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, allocation_result),
    )?;
    let root_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, roots),
    )?;
    let root_tag_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, root_tags),
    )?;
    let root_state_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, root_states),
    )?;
    let exit_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        std_mem::offset_of!(RawNativeActivation, exit),
    )?;

    let mut locals = Vec::with_capacity(plan.local_kinds.len());
    let mut local_tags = Vec::with_capacity(plan.local_kinds.len());
    for slot in 0..plan.local_kinds.len() {
        let local = builder.declare_var(types::I64);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), local_pointer, offset);
        let tag = if value_tag(plan.local_kinds[slot]).is_none() {
            let tag = builder.declare_var(types::I64);
            let value_tag =
                builder
                    .ins()
                    .load(types::I64, MemFlags::new(), local_tag_pointer, offset);
            builder.def_var(tag, value_tag);
            Some(tag)
        } else {
            None
        };
        builder.def_var(local, value);
        locals.push(local);
        local_tags.push(tag);
    }
    let zero_pointer = builder.ins().iconst(pointer_type, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    let local_heap_caches: Vec<Option<LocalHeapCache>> = plan
        .local_kinds
        .iter()
        .copied()
        .enumerate()
        .map(|(slot, kind)| {
            if !matches!(
                kind,
                ScalarKind::Object(_) | ScalarKind::Tagged(_) | ScalarKind::Callback(_)
            ) {
                return None;
            }
            let cache = LocalHeapCache {
                entry: builder.declare_var(pointer_type),
                object_kind: builder.declare_var(types::I64),
                class: builder.declare_var(types::I64),
                actual_class: builder.declare_var(types::I32),
                list_data: plan.cached_list_data[slot].then(|| builder.declare_var(pointer_type)),
                preloaded_list_data: plan.preloaded_list_data[slot],
            };
            builder.def_var(cache.entry, zero_pointer);
            builder.def_var(cache.object_kind, zero_i64);
            builder.def_var(cache.class, zero_i64);
            builder.def_var(cache.actual_class, zero_i32);
            if let Some(list_data) = cache.list_data {
                builder.def_var(list_data, zero_pointer);
            }
            Some(cache)
        })
        .collect();
    let scalar_instances = plan
        .scalar_instances
        .iter()
        .enumerate()
        .map(|(site, instance)| {
            let site = u32::try_from(site).map_err(|_| CompileError::Backend)?;
            let token = u64::from(
                SCALAR_INSTANCE_SLOT_BASE
                    .checked_sub(site)
                    .ok_or(CompileError::Backend)?,
            );
            let active = builder.declare_var(types::I64);
            builder.def_var(active, zero_i64);
            let fields = (0..instance.field_count)
                .map(|_| {
                    let bits = builder.declare_var(types::I64);
                    let tag = builder.declare_var(types::I64);
                    builder.def_var(bits, zero_i64);
                    builder.def_var(tag, zero_i64);
                    Ok(ScalarFieldValues { bits, tag })
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            Ok(ScalarInstanceValues {
                token,
                active,
                fields,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let mut stack = Vec::with_capacity(plan.max_stack);
    let mut stack_tags = Vec::with_capacity(plan.max_stack);
    let dynamic_stack_tags: Vec<bool> = (0..plan.max_stack)
        .map(|slot| {
            plan.segments.iter().any(|segment| {
                segment.fuel_stacks.iter().any(|(_, kinds)| {
                    kinds
                        .get(slot)
                        .copied()
                        .is_some_and(|kind| value_tag(kind).is_none())
                })
            })
        })
        .collect();
    for (slot, dynamic_tag) in dynamic_stack_tags.iter().copied().enumerate() {
        let variable = builder.declare_var(types::I64);
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let value = builder
            .ins()
            .load(types::I64, MemFlags::new(), stack_pointer, offset);
        let tag = if dynamic_tag {
            let tag = builder.declare_var(types::I64);
            let value_tag =
                builder
                    .ins()
                    .load(types::I64, MemFlags::new(), stack_tag_pointer, offset);
            builder.def_var(tag, value_tag);
            Some(tag)
        } else {
            None
        };
        builder.def_var(variable, value);
        stack.push(variable);
        stack_tags.push(tag);
    }
    let fuel = builder.declare_var(types::I64);
    builder.def_var(fuel, initial_fuel);
    let retired = builder.declare_var(types::I64);
    builder.def_var(retired, retired_base);
    let zero = builder.ins().iconst(types::I64, 0);
    let heap_translations = RefCell::new(HeapTranslationCache::default());
    let values = NativeValues {
        plan,
        locals: &locals,
        local_kinds: &plan.local_kinds,
        dirty_locals: None,
        local_tags: &local_tags,
        local_heap_caches: &local_heap_caches,
        scalar_instances: &scalar_instances,
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
        instance_fields_signature,
        capture_allocation_signature,
        value_array_allocation_signature,
        list_growth_signature,
        list_insert_signature,
        list_reserve_signature,
        map_lookup_signature,
        map_put_discard_signature,
        map_put_commit_signature,
        map_insert_hashed_signature,
        bytes_equal_signature,
        value_equal_signature,
        object_binary_signature,
        object_unary_signature,
        digest_signature,
        heap_operation_signature,
        native_signature,
        exit_pointer,
        activation_pointer,
        replay_blocks: &[],
        replay_failures: false,
        inline_return: None,
        pointer_type,
        frontend_config,
        heap_translations: &heap_translations,
    };

    let mut dispatch = Switch::new();
    for (index, block) in blocks.iter().copied().enumerate() {
        dispatch.set_entry(
            index as u128,
            preload_entry_blocks.get(index).copied().unwrap_or(block),
        );
    }
    dispatch.emit(&mut builder, entry, invalid_block);

    builder.switch_to_block(invalid_block);
    let retired_value = emit_retired(&mut builder, values);
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

    if let (Some(preload), Some(failure)) = (preload_block, preload_failure) {
        builder.append_block_param(preload, types::I32);
        for (index, block) in preload_entry_blocks.iter().copied().enumerate() {
            builder.switch_to_block(block);
            let target = builder.ins().iconst(types::I32, index as i64);
            builder.ins().jump(preload, &[target.into()]);
        }

        builder.switch_to_block(preload);
        emit_preloaded_list_data(&mut builder, values, plan, failure)?;
        let target = builder.block_params(preload)[0];
        let mut success = Switch::new();
        for (index, block) in blocks.iter().copied().enumerate() {
            success.set_entry(index as u128, block);
        }
        success.emit(&mut builder, target, invalid_block);

        builder.switch_to_block(failure);
        let mut failed = Switch::new();
        for (index, block) in preload_boundary_blocks.iter().copied().enumerate() {
            failed.set_entry(index as u128, block);
        }
        failed.emit(&mut builder, target, invalid_block);

        for (index, segment) in plan.segments.iter().enumerate() {
            builder.switch_to_block(preload_boundary_blocks[index]);
            let segment_values = NativeValues {
                dirty_locals: Some(&segment.dirty_locals),
                ..values
            };
            emit_preload_boundary(&mut builder, segment_values, segment)?;
        }
    }

    for (index, segment) in plan.segments.iter().enumerate() {
        builder.switch_to_block(blocks[index]);
        let body = body_blocks[index];
        let segment_values = NativeValues {
            dirty_locals: Some(&segment.dirty_locals),
            replay_blocks: &replay_blocks[index],
            ..values
        };
        if segment.carries_reserved_prefix {
            emit_entry_exit(&mut builder, segment_values, segment, EXIT_BOUNDARY)?;
        } else {
            let fuel_boundary = builder.create_block();
            builder.set_cold_block(fuel_boundary);
            let available = builder.use_var(values.fuel);
            let enough = builder.ins().icmp_imm(
                IntCC::SignedGreaterThanOrEqual,
                available,
                i64::from(segment.fuel_reserve),
            );
            builder.ins().brif(enough, body, &[], fuel_boundary, &[]);

            builder.switch_to_block(fuel_boundary);
            emit_reservation_boundary(&mut builder, segment_values, segment, body)?;
        }

        builder.switch_to_block(body);
        let fast_successors: Vec<ir::Block> = segment
            .successors
            .iter()
            .map(|successor| {
                if bypasses_fuel_check(&plan.segments, index, *successor) {
                    body_blocks[*successor]
                } else {
                    blocks[*successor]
                }
            })
            .collect();
        emit_segment(
            &mut builder,
            SegmentEmission {
                bytecode,
                segment,
                successor_blocks: &fast_successors,
                values: segment_values,
                plan,
                input,
                type_environment_sites,
            },
        )?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn emit_preloaded_list_data(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    plan: &RegionPlan,
    failure: ir::Block,
) -> Result<(), CompileError> {
    let point = FaultPoint {
        block: 0,
        instruction: 0,
        prefix: 0,
    };
    for (slot, preload) in plan.preloaded_list_data.iter().copied().enumerate() {
        if !preload {
            continue;
        }
        let reference = builder.use_var(values.locals[slot]);
        let entry = emit_heap_entry_miss(
            builder,
            values,
            reference,
            point,
            ObjectGuard::Branch(failure),
        )?;
        let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
        let wrong_kind = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(JIT_OBJECT_LIST));
        emit_object_guard(
            builder,
            values,
            wrong_kind,
            point,
            ObjectGuard::Branch(failure),
        )?;
        let data = load_immutable_heap_value(
            builder,
            values.pointer_type,
            entry,
            JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
        )?;
        let cache = values.local_heap_caches[slot].ok_or(CompileError::Backend)?;
        let list_data = cache.list_data.ok_or(CompileError::Backend)?;
        let list_proof = builder
            .ins()
            .iconst(types::I64, i64::from(JIT_OBJECT_LIST) + 1);
        builder.def_var(cache.entry, entry);
        builder.def_var(cache.object_kind, list_proof);
        builder.def_var(list_data, data);
    }
    Ok(())
}

fn emit_preload_boundary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
) -> Result<(), CompileError> {
    emit_entry_exit(builder, values, segment, EXIT_BOUNDARY)
}

fn emit_entry_exit(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
    kind: u32,
) -> Result<(), CompileError> {
    let kind_value = builder.ins().iconst(types::I32, i64::from(kind));
    emit_entry_exit_with_kind(builder, values, segment, kind, kind_value)
}

fn emit_entry_exit_with_kind(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
    shape_kind: u32,
    kind: ir::Value,
) -> Result<(), CompileError> {
    let stack = values
        .stack
        .iter()
        .copied()
        .zip(segment.entry_stack.iter().copied())
        .enumerate()
        .map(|(slot, (bits, kind))| {
            Ok(NativeValue {
                bits: builder.use_var(bits),
                tag: emit_slot_tag(builder, values.stack_tags[slot], kind)?,
            })
        })
        .collect::<Result<Vec<_>, CompileError>>()?;
    let retired = emit_retired(builder, values);
    let zero = builder.ins().iconst(types::I64, 0);
    let locals = capture_local_values(builder, values)?;
    emit_exit_with_locals_and_kind(
        builder,
        values,
        ExitEmission {
            retired,
            kind: shape_kind,
            block: segment.block,
            instruction: segment.start,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        kind,
        &locals,
        &stack,
    )
}

fn emit_segment(
    builder: &mut FunctionBuilder<'_>,
    emission: SegmentEmission<'_, '_>,
) -> Result<(), CompileError> {
    let values = emission.values;
    let segment = emission.segment;
    let stack = segment_entry_values(builder, values, segment)?;
    let replay_exit = if let Some(replay) = values.replay_blocks.first() {
        Some(ReplayExitState {
            target: replay.block,
            block: segment.block,
            instruction: segment.start,
            retired: builder.use_var(values.retired),
            prefix: segment.reserved_prefix_cost,
            locals: capture_local_values(builder, values)?,
            stack: stack.clone(),
        })
    } else {
        None
    };
    emit_segment_body(builder, emission, stack, segment.virtual_stack_in.clone())?;
    if let Some(replay) = replay_exit {
        let used = values
            .replay_blocks
            .iter()
            .find(|candidate| candidate.block == replay.target)
            .is_some_and(|candidate| candidate.used.get());
        if used {
            builder.switch_to_block(replay.target);
            let retired = if replay.prefix == 0 {
                replay.retired
            } else {
                builder
                    .ins()
                    .iadd_imm(replay.retired, i64::from(replay.prefix))
            };
            let zero = builder.ins().iconst(types::I64, 0);
            emit_exit_with_locals(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_REPLAY,
                    block: replay.block,
                    instruction: replay.instruction,
                    result: NativeValue {
                        bits: zero,
                        tag: zero,
                    },
                },
                &replay.locals,
                &replay.stack,
            )?;
        }
    }
    Ok(())
}

fn segment_entry_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
) -> Result<Vec<NativeValue>, CompileError> {
    values
        .stack
        .iter()
        .copied()
        .zip(segment.entry_stack.iter().copied())
        .enumerate()
        .map(|(slot, (bits, kind))| {
            Ok(NativeValue {
                bits: builder.use_var(bits),
                tag: emit_slot_tag(builder, values.stack_tags[slot], kind)?,
            })
        })
        .collect::<Result<_, CompileError>>()
}

mod alloc;
mod builders;
mod calls;
mod dispatch;
mod exits;
mod heap;
mod lists;
mod maps;
mod mem;
mod numeric;
mod text;

use alloc::*;
use builders::*;
use calls::*;
use dispatch::*;
use exits::*;
use heap::*;
use lists::*;
use maps::*;
use mem::*;
use numeric::*;
use text::*;

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

fn emit_slot_tag(
    builder: &mut FunctionBuilder<'_>,
    variable: Option<Variable>,
    kind: ScalarKind,
) -> Result<ir::Value, CompileError> {
    if let Some(tag) = value_tag(kind) {
        return Ok(builder.ins().iconst(types::I64, tag as u64 as i64));
    }
    variable
        .map(|variable| builder.use_var(variable))
        .ok_or(CompileError::Backend)
}

fn define_slot_tag(
    builder: &mut FunctionBuilder<'_>,
    variable: Option<Variable>,
    kind: ScalarKind,
    tag: ir::Value,
) -> Result<(), CompileError> {
    if value_tag(kind).is_some() {
        return Ok(());
    }
    let variable = variable.ok_or(CompileError::Backend)?;
    builder.def_var(variable, tag);
    Ok(())
}

fn emit_local_state(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    slot: usize,
) -> Result<ir::Value, CompileError> {
    let offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
    Ok(builder.ins().load(
        types::I8,
        MemFlags::new(),
        values.local_state_pointer,
        offset,
    ))
}

fn store_local_state(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    slot: usize,
    state: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), state, values.local_state_pointer, offset);
    Ok(())
}

fn clear_local_heap_cache(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    slot: usize,
) {
    let Some(cache) = values.local_heap_caches.get(slot).copied().flatten() else {
        return;
    };
    let zero_pointer = builder.ins().iconst(values.pointer_type, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    builder.def_var(cache.entry, zero_pointer);
    builder.def_var(cache.object_kind, zero_i64);
    builder.def_var(cache.class, zero_i64);
    builder.def_var(cache.actual_class, zero_i32);
    if let Some(list_data) = cache.list_data {
        builder.def_var(list_data, zero_pointer);
    }
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
