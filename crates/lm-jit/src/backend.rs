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
    bypasses_fuel_check, is_root_kind, transfer_virtual_instruction, CallContract, HeapAccessKind,
    ObjectContract, OptionAccessKind, OptionTarget, RegionPlan, ScalarFieldSource,
    ScalarReplacement, Segment, SegmentExit, UnsupportedReason, ValueCallTarget, ValueContract,
    VirtualReceiver,
};
use crate::{
    CallValueSite, CompiledRegion, FunctionInput, GenericVirtualCallSite, InterfaceCallSite,
    NativeEntryCell, ScalarKind, TreatmentClass, TypeEnvironmentSite, EXIT_BOUNDARY, EXIT_CALL,
    EXIT_CALLBACK_CALL, EXIT_DIVIDE_BY_ZERO, EXIT_EFFECT, EXIT_FUEL, EXIT_GENERIC_VIRTUAL_CALL,
    EXIT_GROW_ACTIVATION, EXIT_GROW_ROOTS, EXIT_GUEST_FAULT, EXIT_HEAP_LIMIT,
    EXIT_INTEGER_OVERFLOW, EXIT_INTERFACE_CALL, EXIT_INVALID_ENTRY, EXIT_LITERAL, EXIT_POLL,
    EXIT_REPLAY, EXIT_RETURN, EXIT_STACK_LIMIT, EXIT_TYPE_ENVIRONMENT, EXIT_TYPE_MISMATCH,
    EXIT_TYPE_RESOLUTION, EXIT_UNINITIALIZED_FIELD, EXIT_UNREACHABLE, LOCAL_INITIALIZED,
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
    let body_code_size = body_context
        .compiled_code()
        .map(|code| code.code_buffer().len())
        .ok_or(CompileError::Backend)?;
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
    let entry = unsafe { mem::transmute::<*const u8, NativeFunction>(entry_code) };
    let call_entry = body_code as usize;
    Ok(CompiledRegion {
        function: input.root.function,
        code_size: body_code_size.saturating_add(entry_code_size),
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
        i32::try_from(mem::offset_of!(RawNativeActivation, root_code))
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
        mem::offset_of!(RawNativeActivation, scalars),
    )?;
    let tags = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, tags),
    )?;
    let states = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, states),
    )?;
    let frames = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, frames),
    )?;
    let frame_len = load_value(
        &mut builder,
        types::I32,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, frame_len),
    )?;
    let frame_index = builder.ins().iadd_imm(frame_len, -1);
    let frame_index = builder.ins().uextend(pointer_type, frame_index);
    let frame_offset = builder
        .ins()
        .imul_imm(frame_index, mem::size_of::<RawNativeFrame>() as i64);
    let frame = builder.ins().iadd(frames, frame_offset);
    let scalar_base = load_cell_u32(
        &mut builder,
        frame,
        mem::offset_of!(RawNativeFrame, scalar_base),
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
        mem::offset_of!(RawNativeActivation, poll_deadline),
    )?;
    let initial_fuel = builder.ins().isub(poll_deadline, retired_base);
    let runtime_context = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, runtime_context),
    )?;
    let runtime_functions = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, runtime_functions),
    )?;
    let allocation_result_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, allocation_result),
    )?;
    let root_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, roots),
    )?;
    let root_tag_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, root_tags),
    )?;
    let root_state_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, root_states),
    )?;
    let exit_pointer = load_value(
        &mut builder,
        pointer_type,
        activation_pointer,
        mem::offset_of!(RawNativeActivation, exit),
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
    let stack: Vec<NativeValue> = values
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
        .collect::<Result<_, CompileError>>()?;
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

fn emit_segment_body(
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
        match instruction {
            Instr::ConstUnit => {
                let value = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, value)?;
            }
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
                    &stack,
                    capture_count,
                )?;
                let frame = emit_current_frame_pointer(builder, values)?;
                let environment =
                    load_cell_u32(builder, frame, mem::offset_of!(RawNativeFrame, environment))?;
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
                        replay_stack: &stack,
                        fault_stack: &post_stack,
                    },
                )?;
                stack.truncate(stack_start);
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                    &stack,
                    capture_count,
                )?;
                let frame = emit_current_frame_pointer(builder, values)?;
                let environment =
                    load_cell_u32(builder, frame, mem::offset_of!(RawNativeFrame, environment))?;
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
                        replay_stack: &stack,
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
                    &stack,
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
                        replay_stack: &stack,
                        fault_stack: &post_stack,
                    },
                )?;
                stack.truncate(stack_start);
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                            prefix: prior_prefix,
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
                            deopt_stack: &stack,
                        },
                    },
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
                        prefix: prior_prefix,
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
                        prefix: prior_prefix,
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
                let slot = slot as usize;
                let bits = builder.use_var(values.locals[slot]);
                if virtual_locals[slot] {
                    emit_retain_pending_instance(builder, values, bits)?;
                }
                if values.local_heap_caches[slot].is_some() {
                    values
                        .heap_translations
                        .borrow_mut()
                        .record_local(bits, slot);
                }
                stack.push(NativeValue {
                    bits,
                    tag: emit_slot_tag(builder, values.local_tags[slot], plan.local_kinds[slot])?,
                });
            }
            Instr::StoreLocal(slot) => {
                let slot = slot as usize;
                if virtual_locals[slot] {
                    let old = builder.use_var(values.locals[slot]);
                    emit_release_pending_instance(builder, values, old)?;
                }
                let value = pop_value(&mut stack)?;
                builder.def_var(values.locals[slot], value.bits);
                define_slot_tag(
                    builder,
                    values.local_tags[slot],
                    plan.local_kinds[slot],
                    value.tag,
                )?;
                if !initialized_locals[slot] {
                    let state = builder
                        .ins()
                        .iconst(types::I8, i64::from(LOCAL_INITIALIZED));
                    store_local_state(builder, values, slot, state)?;
                    initialized_locals[slot] = true;
                }
                values.heap_translations.borrow_mut().forget_local(slot);
                clear_local_heap_cache(builder, values, slot);
            }
            Instr::Pop => {
                if virtual_stack.last().copied().unwrap_or(false) {
                    let value = stack.last().copied().ok_or(CompileError::Backend)?;
                    emit_release_pending_instance(builder, values, value.bits)?;
                }
                pop_native(&mut stack)?;
            }
            Instr::LoadCapture(index) => {
                let instruction = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::LoadCapture { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let value = emit_load_capture(
                    builder,
                    values,
                    index,
                    value,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &stack,
                    },
                )?;
                stack.push(value);
            }
            Instr::LoadField(field) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
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
                    LoadFieldEmission {
                        field,
                        receiver_class,
                        contract: value,
                        allow_pending,
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
                if allow_pending {
                    emit_release_pending_instance(builder, values, reference)?;
                }
                stack.push(value);
            }
            Instr::StoreField(field) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack
                    .len()
                    .checked_sub(2)
                    .and_then(|index| virtual_stack.get(index))
                    .copied()
                    .unwrap_or(false);
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
                    allow_pending,
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
                if allow_pending {
                    emit_release_pending_instance(builder, values, reference)?;
                }
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
            Instr::EqDigest | Instr::NeDigest => {
                let deopt_stack = stack.clone();
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::DigestCompare) {
                    return Err(CompileError::Backend);
                }
                let equal = emit_digest_equal(
                    builder,
                    values,
                    left,
                    right,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                let result = if matches!(instruction, Instr::EqDigest) {
                    equal
                } else {
                    builder.ins().bxor_imm(equal, 1)
                };
                let result = builder.ins().uextend(types::I64, result);
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Extended(ExtendedInstr::AsCallback) => {
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::AsCallback) {
                    return Err(CompileError::Backend);
                }
                emit_object_entry(
                    builder,
                    values,
                    value.bits,
                    JIT_OBJECT_CLOSURE,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    ObjectGuard::Replay(&deopt_stack),
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
                        prefix: prior_prefix,
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
                        prefix: prior_prefix,
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
                        prefix: prior_prefix,
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::ListPop { .. }) => {
                let instruction_index = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::ListPop { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let result = emit_list_pop(
                    builder,
                    values,
                    reference,
                    ListOptionEmission {
                        function: input.root.function,
                        result: value,
                        family_type: access.family_type,
                        exit: HeapExitEmission {
                            point: FaultPoint {
                                block: segment.block,
                                instruction: instruction_index + 1,
                                prefix: fault_prefix,
                            },
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                        resolve: FaultPoint {
                            block: segment.block,
                            instruction: instruction_index,
                            prefix: prior_prefix,
                        },
                    },
                )?;
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::ListContains) => {
                let deopt_stack = stack.clone();
                let needle = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let result = emit_runtime_value_lookup(
                    builder,
                    values,
                    mem::offset_of!(RawNativeFunctions, list_contains),
                    reference,
                    needle,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                stack.push(result);
            }
            Instr::IsType(_) | Instr::CastType(_) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
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
                            prefix: prior_prefix,
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
                        if allow_pending {
                            emit_release_pending_instance(builder, values, value.bits)?;
                        }
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
                    let actual = if allow_pending {
                        emit_instance_storage(
                            builder,
                            values,
                            value.bits,
                            None,
                            point,
                            ObjectGuard::Replay(&deopt_stack),
                            ObjectGuard::Replay(&deopt_stack),
                        )?
                        .actual_class
                    } else {
                        let entry = emit_object_entry(
                            builder,
                            values,
                            value.bits,
                            JIT_OBJECT_INSTANCE,
                            point,
                            ObjectGuard::Replay(&deopt_stack),
                        )?;
                        load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?
                    };
                    let matches = emit_class_matches(builder, values, actual, target_class)?;
                    if matches!(instruction, Instr::IsType(_)) {
                        let result = builder.ins().uextend(types::I64, matches);
                        if allow_pending {
                            emit_release_pending_instance(builder, values, value.bits)?;
                        }
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
            Instr::MapLen => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapLen) {
                    return Err(CompileError::Backend);
                }
                let value = emit_map_len(
                    builder,
                    values,
                    reference,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::MapHas | Instr::MapAt => {
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let (key_contract, value_contract) = match (instruction, access.kind) {
                    (Instr::MapHas, HeapAccessKind::MapHas { key }) => (key, None),
                    (Instr::MapAt, HeapAccessKind::MapAt { key, value }) => (key, Some(value)),
                    _ => return Err(CompileError::Backend),
                };
                let point = FaultPoint {
                    block: segment.block,
                    instruction: instruction_index + 1,
                    prefix: fault_prefix,
                };
                let result = emit_map_lookup(
                    builder,
                    values,
                    MapLookupEmission {
                        reference,
                        key,
                        key_contract,
                        result: if matches!(instruction, Instr::MapAt) {
                            MapLookupResult::At
                        } else {
                            MapLookupResult::Has
                        },
                        exit: HeapExitEmission {
                            point,
                            fault_stack: &stack,
                            deopt_stack: &deopt_stack,
                        },
                    },
                )?;
                if let Some(contract) = value_contract {
                    emit_native_value_contract(
                        builder,
                        values,
                        result,
                        contract,
                        point,
                        &deopt_stack,
                    )?;
                }
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapGet { .. }) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let heap_access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapGet { key: key_contract } = heap_access.kind else {
                    return Err(CompileError::Backend);
                };
                let option_access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::MapGet { value } = option_access.kind else {
                    return Err(CompileError::Backend);
                };
                let family = emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    option_access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction,
                        prefix: prior_prefix,
                    },
                    &deopt_stack,
                )?;
                let result = emit_map_lookup(
                    builder,
                    values,
                    MapLookupEmission {
                        reference,
                        key,
                        key_contract,
                        result: MapLookupResult::Get { family, value },
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
                stack.push(result);
            }
            Instr::MapPut { discard, .. } => {
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let heap_access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapPut { key: key_contract } = heap_access.kind else {
                    return Err(CompileError::Backend);
                };
                let option_access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::MapPut {
                    value: previous_contract,
                    discard: planned_discard,
                } = option_access.kind
                else {
                    return Err(CompileError::Backend);
                };
                if planned_discard != discard {
                    return Err(CompileError::Backend);
                }
                let family = if discard {
                    None
                } else {
                    Some(emit_option_family(
                        builder,
                        values,
                        input.root.function,
                        option_access.family_type,
                        FaultPoint {
                            block: segment.block,
                            instruction: instruction_index,
                            prefix: prior_prefix,
                        },
                        &deopt_stack,
                    )?)
                };
                let root_kinds = segment
                    .replay_stacks
                    .iter()
                    .find(|(position, _)| *position == instruction_index)
                    .map(|(_, stack)| stack.as_slice())
                    .ok_or(CompileError::Backend)?;
                let roots = collect_native_roots(
                    builder,
                    values,
                    &plan.local_kinds,
                    root_kinds,
                    &deopt_stack,
                )?;
                let result = emit_map_put(
                    builder,
                    values,
                    reference,
                    key,
                    key_contract,
                    stored,
                    family,
                    previous_contract,
                    &roots,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: instruction_index + 1,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                if let Some(result) = result {
                    stack.push(result);
                }
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
            Instr::Extended(ExtendedInstr::ListInsert) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::ListInsert { value } = access.kind else {
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
                emit_list_insert(
                    builder,
                    values,
                    ListInsertEmission {
                        reference,
                        index,
                        stored,
                        contract: value,
                        roots: &roots,
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
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(
                operation @ (ExtendedInstr::ListRemove | ExtendedInstr::ListSwapRemove),
            ) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::ListRemove { value, swap } = access.kind else {
                    return Err(CompileError::Backend);
                };
                if swap != matches!(operation, ExtendedInstr::ListSwapRemove) {
                    return Err(CompileError::Backend);
                }
                let result = emit_list_remove(
                    builder,
                    values,
                    reference,
                    index,
                    value,
                    swap,
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::ListTruncate) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let length = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::ListTruncate) {
                    return Err(CompileError::Backend);
                }
                emit_list_truncate(
                    builder,
                    values,
                    reference,
                    length,
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
            Instr::Extended(ExtendedInstr::MapEpoch) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapEpoch) {
                    return Err(CompileError::Backend);
                }
                let value = emit_map_epoch(
                    builder,
                    values,
                    reference,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::MapIterLen) => {
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let instruction_index = segment.start + within as u32;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction_index)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapIterLen) {
                    return Err(CompileError::Backend);
                }
                let value = emit_map_iter_len(
                    builder,
                    values,
                    reference,
                    expected,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction_index + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, value)?;
            }
            Instr::Extended(ExtendedInstr::MapNextIndex) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let expected = pop_native(&mut stack)?;
                let cursor = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapNextIndex) {
                    return Err(CompileError::Backend);
                }
                let result = emit_map_next_index(
                    builder,
                    values,
                    reference,
                    cursor,
                    expected,
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
                stack.push(result);
            }
            Instr::Extended(operation @ (ExtendedInstr::MapKeyAt | ExtendedInstr::MapValueAt)) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let contract = match (operation, access.kind) {
                    (ExtendedInstr::MapKeyAt, HeapAccessKind::MapKeyAt { value }) => value,
                    (ExtendedInstr::MapValueAt, HeapAccessKind::MapValueAt { value }) => value,
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_map_entry_at(
                    builder,
                    values,
                    reference,
                    index,
                    matches!(operation, ExtendedInstr::MapValueAt),
                    contract,
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapRemove { .. }) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let heap_access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapRemove { key: key_contract } = heap_access.kind else {
                    return Err(CompileError::Backend);
                };
                let option_access = segment
                    .option_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let OptionAccessKind::MapRemove { value } = option_access.kind else {
                    return Err(CompileError::Backend);
                };
                let family = emit_option_family(
                    builder,
                    values,
                    input.root.function,
                    option_access.family_type,
                    FaultPoint {
                        block: segment.block,
                        instruction,
                        prefix: prior_prefix,
                    },
                    &deopt_stack,
                )?;
                let result = emit_map_remove(
                    builder,
                    values,
                    reference,
                    key,
                    key_contract,
                    family,
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapClear) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapClear) {
                    return Err(CompileError::Backend);
                }
                emit_object_unary_runtime_value(
                    builder,
                    values,
                    mem::offset_of!(RawNativeFunctions, map_clear),
                    reference,
                    ValueContract {
                        kind: ScalarKind::Unit,
                        object: None,
                    },
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
            Instr::Extended(ExtendedInstr::MapReserve) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let additional = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapReserve) {
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
                let status = emit_map_reserve_call(builder, values, reference, additional, &roots)?;
                emit_runtime_status(
                    builder,
                    values,
                    status,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &stack,
                    &deopt_stack,
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::MapProbe) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let prior = pop_native(&mut stack)?;
                let semantic = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapProbe) {
                    return Err(CompileError::Backend);
                }
                let result = emit_map_runtime_value(
                    builder,
                    values,
                    mem::offset_of!(RawNativeFunctions, map_probe),
                    reference,
                    semantic,
                    prior,
                    ValueContract {
                        kind: ScalarKind::Int,
                        object: None,
                    },
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapProbeFound) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let epoch = builder.ins().ushr_imm(token, 32);
                let invalid = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
                emit_interpreter_replay(
                    builder,
                    values,
                    invalid,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                let low = builder.ins().ireduce(types::I32, token);
                let found = builder.ins().icmp_imm(IntCC::NotEqual, low, 0);
                let found = builder.ins().uextend(types::I64, found);
                push_static(builder, &mut stack, ScalarKind::Bool, found)?;
            }
            Instr::Extended(
                operation @ (ExtendedInstr::MapProbeKey | ExtendedInstr::MapProbeValue),
            ) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let (function_offset, contract) = match (operation, access.kind) {
                    (ExtendedInstr::MapProbeKey, HeapAccessKind::MapProbeKey { value }) => {
                        (mem::offset_of!(RawNativeFunctions, map_probe_key), value)
                    }
                    (ExtendedInstr::MapProbeValue, HeapAccessKind::MapProbeValue { value }) => {
                        (mem::offset_of!(RawNativeFunctions, map_probe_value), value)
                    }
                    _ => return Err(CompileError::Backend),
                };
                let result = emit_object_binary_runtime_value(
                    builder,
                    values,
                    function_offset,
                    reference,
                    token,
                    contract,
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapProbeSetValue) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let stored = pop_value(&mut stack)?;
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapProbeSetValue { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                emit_native_value_contract(
                    builder,
                    values,
                    stored,
                    value,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                let function = load_value(
                    builder,
                    values.pointer_type,
                    values.runtime_functions,
                    mem::offset_of!(RawNativeFunctions, map_probe_set_value),
                )?;
                let call = builder.ins().call_indirect(
                    values.value_equal_signature,
                    function,
                    &[
                        values.runtime_context,
                        reference,
                        token,
                        stored.bits,
                        stored.tag,
                        values.allocation_result_pointer,
                    ],
                );
                let status = builder.inst_results(call)[0];
                emit_runtime_status(
                    builder,
                    values,
                    status,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &stack,
                    &deopt_stack,
                )?;
                let unit = NativeValue {
                    bits: builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        values.allocation_result_pointer,
                        0,
                    ),
                    tag: builder.ins().load(
                        types::I64,
                        MemFlags::new(),
                        values.allocation_result_pointer,
                        8,
                    ),
                };
                emit_native_value_contract(
                    builder,
                    values,
                    unit,
                    ValueContract {
                        kind: ScalarKind::Unit,
                        object: None,
                    },
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                let zero = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, zero)?;
            }
            Instr::Extended(ExtendedInstr::MapProbeRemove) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapProbeRemove { value } = access.kind else {
                    return Err(CompileError::Backend);
                };
                let result = emit_object_binary_runtime_value(
                    builder,
                    values,
                    mem::offset_of!(RawNativeFunctions, map_probe_remove),
                    reference,
                    token,
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
                stack.push(result);
            }
            Instr::Extended(ExtendedInstr::MapInsertHashed) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let token = pop_native(&mut stack)?;
                let semantic = pop_native(&mut stack)?;
                let stored = pop_value(&mut stack)?;
                let key = pop_value(&mut stack)?;
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                let HeapAccessKind::MapInsertHashed {
                    key: key_contract,
                    value: value_contract,
                } = access.kind
                else {
                    return Err(CompileError::Backend);
                };
                let point = FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                };
                emit_native_value_contract(
                    builder,
                    values,
                    key,
                    key_contract,
                    point,
                    &deopt_stack,
                )?;
                emit_native_value_contract(
                    builder,
                    values,
                    stored,
                    value_contract,
                    point,
                    &deopt_stack,
                )?;
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
                let root_count = emit_runtime_roots(builder, values, &roots)?;
                let function = load_value(
                    builder,
                    values.pointer_type,
                    values.runtime_functions,
                    mem::offset_of!(RawNativeFunctions, map_insert_hashed),
                )?;
                let call = builder.ins().call_indirect(
                    values.map_insert_hashed_signature,
                    function,
                    &[
                        values.runtime_context,
                        reference,
                        key.bits,
                        key.tag,
                        stored.bits,
                        stored.tag,
                        semantic,
                        token,
                        root_count,
                    ],
                );
                let status = builder.inst_results(call)[0];
                emit_runtime_status(builder, values, status, point, &stack, &deopt_stack)?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::MapWriteGuard) => {
                let instruction = segment.start + within as u32;
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let access = segment
                    .heap_accesses
                    .iter()
                    .find(|access| access.instruction == instruction)
                    .copied()
                    .ok_or(CompileError::Backend)?;
                if !matches!(access.kind, HeapAccessKind::MapWriteGuard) {
                    return Err(CompileError::Backend);
                }
                let point = FaultPoint {
                    block: segment.block,
                    instruction: instruction + 1,
                    prefix: fault_prefix,
                };
                let entry = emit_object_entry(
                    builder,
                    values,
                    reference,
                    JIT_OBJECT_MAP,
                    point,
                    ObjectGuard::Replay(&deopt_stack),
                )?;
                emit_mutable_guard(
                    builder,
                    values,
                    entry,
                    HeapExitEmission {
                        point,
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let unit = builder.ins().iconst(types::I64, 0);
                push_static(builder, &mut stack, ScalarKind::Unit, unit)?;
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                let deopt_stack = stack.clone();
                let allow_pending = virtual_stack.last().copied().unwrap_or(false);
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
                    allow_pending,
                    FaultPoint {
                        block: segment.block,
                        instruction: instruction + 1,
                        prefix: fault_prefix,
                    },
                    &deopt_stack,
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), reference)?;
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
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let point = FaultPoint {
                    block: segment.block,
                    instruction: position + 1,
                    prefix: fault_prefix,
                };
                let result = match instruction {
                    Instr::Native(NativeInstr::SbAppendStr) => {
                        let source = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_text(
                            builder,
                            values,
                            target,
                            source,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendBool) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_bool(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendInt) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_int(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::SbAppendChar) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_string_builder_append_char(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbAppend) => {
                        let value = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_append(
                            builder,
                            values,
                            target,
                            value,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbExtend) => {
                        let source = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_extend(
                            builder,
                            values,
                            target,
                            source,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    Instr::Native(NativeInstr::BbReserve) => {
                        let additional = pop_native(&mut stack)?;
                        let target = pop_native(&mut stack)?;
                        emit_byte_buffer_reserve(
                            builder,
                            values,
                            target,
                            additional,
                            &roots,
                            HeapExitEmission {
                                point,
                                fault_stack: &stack,
                                deopt_stack: &deopt_stack,
                            },
                        )?
                    }
                    _ => return Err(CompileError::Backend),
                };
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset) = match instruction {
                    Instr::FaultCode => {
                        let fault = pop_native(&mut stack)?;
                        (
                            [fault, zero, zero],
                            mem::offset_of!(RawNativeFunctions, fault_code),
                        )
                    }
                    Instr::FaultDenied => {
                        let reason = pop_native(&mut stack)?;
                        (
                            [reason, zero, zero],
                            mem::offset_of!(RawNativeFunctions, fault_denied),
                        )
                    }
                    Instr::Extended(ExtendedInstr::DynPack { ty }) => {
                        let value = pop_value(&mut stack)?;
                        let frame = emit_current_frame_pointer(builder, values)?;
                        let environment = load_cell_u32(
                            builder,
                            frame,
                            mem::offset_of!(RawNativeFrame, environment),
                        )?;
                        let environment = builder.ins().uextend(types::I64, environment);
                        let environment = builder.ins().ishl_imm(environment, 32);
                        let ty = builder.ins().iconst(types::I64, i64::from(ty));
                        let packed = builder.ins().bor(ty, environment);
                        (
                            [value.bits, value.tag, packed],
                            mem::offset_of!(RawNativeFunctions, dyn_pack),
                        )
                    }
                    Instr::Native(NativeInstr::SbNew) => (
                        [zero, zero, zero],
                        mem::offset_of!(RawNativeFunctions, string_builder_new),
                    ),
                    Instr::Native(NativeInstr::BbNew) => (
                        [zero, zero, zero],
                        mem::offset_of!(RawNativeFunctions, byte_buffer_new),
                    ),
                    Instr::Numeric(NumericInstr::SbAppendFloat) => {
                        let value = pop_native(&mut stack)?;
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, value, zero],
                            mem::offset_of!(RawNativeFunctions, string_builder_append_float),
                        )
                    }
                    Instr::Native(NativeInstr::SbBuild) => {
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, zero, zero],
                            mem::offset_of!(RawNativeFunctions, string_builder_build),
                        )
                    }
                    Instr::Native(NativeInstr::SbFinish) => {
                        let builder_value = pop_native(&mut stack)?;
                        (
                            [builder_value, zero, zero],
                            mem::offset_of!(RawNativeFunctions, string_builder_finish),
                        )
                    }
                    Instr::Native(NativeInstr::BbBuild) => {
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, zero, zero],
                            mem::offset_of!(RawNativeFunctions, byte_buffer_build),
                        )
                    }
                    Instr::Native(NativeInstr::BbFinish) => {
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, zero, zero],
                            mem::offset_of!(RawNativeFunctions, byte_buffer_finish),
                        )
                    }
                    Instr::Native(NativeInstr::BytesNew) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_from_text),
                        )
                    }
                    Instr::Native(NativeInstr::BytesSlice) => {
                        let length = pop_native(&mut stack)?;
                        let start = pop_native(&mut stack)?;
                        let source = pop_native(&mut stack)?;
                        (
                            [source, start, length],
                            mem::offset_of!(RawNativeFunctions, bytes_slice),
                        )
                    }
                    Instr::Native(NativeInstr::BytesConcat) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_concat),
                        )
                    }
                    Instr::Native(NativeInstr::BytesCompact) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_compact),
                        )
                    }
                    Instr::Native(NativeInstr::BytesTextView) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_text_view),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitAnd) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_bit_and),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitOr) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_bit_or),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitXor) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_bit_xor),
                        )
                    }
                    Instr::Numeric(NumericInstr::BytesBitNot) => {
                        let source = pop_native(&mut stack)?;
                        (
                            [source, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_bit_not),
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
            }
            Instr::Native(NativeInstr::SbLen | NativeInstr::SbByteLen | NativeInstr::BbLen) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
            }
            Instr::Native(NativeInstr::SbClear | NativeInstr::BbClear) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Object(0), reference)?;
            }
            Instr::Native(NativeInstr::BbAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
            Instr::Native(NativeInstr::TextAtByte) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::Native(NativeInstr::TextAt) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Char, value)?;
            }
            Instr::Native(NativeInstr::TextIsBoundary) => {
                let deopt_stack = stack.clone();
                let index = pop_native(&mut stack)?;
                let reference = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Bool, value)?;
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
                    Some(site) => collect_native_roots(
                        builder,
                        values,
                        &plan.local_kinds,
                        &site.stack,
                        &stack,
                    )?,
                    None => Vec::new(),
                };
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset, result_kind) = match instruction {
                    Instr::Extended(ExtendedInstr::SyntaxTreeRoot) => {
                        let tree = pop_native(&mut stack)?;
                        (
                            [tree, zero, zero],
                            mem::offset_of!(RawNativeFunctions, syntax_tree_root),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Extended(
                        operation @ (ExtendedInstr::SyntaxKind
                        | ExtendedInstr::SyntaxCategory
                        | ExtendedInstr::SyntaxRangeStart
                        | ExtendedInstr::SyntaxRangeEnd),
                    ) => {
                        let element = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxKind => {
                                mem::offset_of!(RawNativeFunctions, syntax_kind)
                            }
                            ExtendedInstr::SyntaxCategory => {
                                mem::offset_of!(RawNativeFunctions, syntax_category)
                            }
                            ExtendedInstr::SyntaxRangeStart => {
                                mem::offset_of!(RawNativeFunctions, syntax_range_start)
                            }
                            ExtendedInstr::SyntaxRangeEnd => {
                                mem::offset_of!(RawNativeFunctions, syntax_range_end)
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
                        let element = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxText => {
                                mem::offset_of!(RawNativeFunctions, syntax_text)
                            }
                            ExtendedInstr::SyntaxChildren => {
                                mem::offset_of!(RawNativeFunctions, syntax_children)
                            }
                            ExtendedInstr::SyntaxDetach => {
                                mem::offset_of!(RawNativeFunctions, syntax_detach)
                            }
                            ExtendedInstr::SyntaxToTree => {
                                mem::offset_of!(RawNativeFunctions, syntax_to_tree)
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
                        let value = pop_native(&mut stack)?;
                        let kind = pop_native(&mut stack)?;
                        let builder_value = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            ExtendedInstr::SyntaxBuildToken => {
                                mem::offset_of!(RawNativeFunctions, syntax_build_token)
                            }
                            ExtendedInstr::SyntaxBuildTrivia => {
                                mem::offset_of!(RawNativeFunctions, syntax_build_trivia)
                            }
                            ExtendedInstr::SyntaxBuildNode => {
                                mem::offset_of!(RawNativeFunctions, syntax_build_node)
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, result_kind, result)?;
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
                        &stack,
                    )?
                };
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
                        &stack,
                    )?
                };
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
            Instr::EqValue | Instr::NeValue => {
                let deopt_stack = stack.clone();
                let right = pop_value(&mut stack)?;
                let left = pop_value(&mut stack)?;
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                let result = if matches!(instruction, Instr::EqValue) {
                    equal
                } else {
                    builder.ins().bxor_imm(equal, 1)
                };
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Freeze => {
                let deopt_stack = stack.clone();
                let value = pop_value(&mut stack)?;
                let result = emit_typed_object_unary(
                    builder,
                    values,
                    mem::offset_of!(RawNativeFunctions, freeze_graph),
                    value.bits,
                    HeapExitEmission {
                        point: FaultPoint {
                            block: segment.block,
                            instruction: segment.start + prefix,
                            prefix: fault_prefix,
                        },
                        fault_stack: &stack,
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
                    collect_native_roots(builder, values, &plan.local_kinds, &site.stack, &stack)?;
                let reference = pop_native(&mut stack)?;
                let frame = emit_current_frame_pointer(builder, values)?;
                let environment =
                    load_cell_u32(builder, frame, mem::offset_of!(RawNativeFrame, environment))?;
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
                push_static(builder, &mut stack, ScalarKind::Object(0), result)?;
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
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
                let function_offset = match operation {
                    NativeInstr::EqStr
                    | NativeInstr::NeStr
                    | NativeInstr::TextLt
                    | NativeInstr::TextLe
                    | NativeInstr::TextGt
                    | NativeInstr::TextGe => {
                        mem::offset_of!(RawNativeFunctions, text_compare)
                    }
                    _ => mem::offset_of!(RawNativeFunctions, bytes_compare),
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
                        fault_stack: &stack,
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
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(operation @ (NativeInstr::TextHash | NativeInstr::BytesHash)) => {
                let deopt_stack = stack.clone();
                let reference = pop_native(&mut stack)?;
                let function_offset = if matches!(operation, NativeInstr::TextHash) {
                    mem::offset_of!(RawNativeFunctions, text_hash)
                } else {
                    mem::offset_of!(RawNativeFunctions, bytes_hash)
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, ScalarKind::Int, result)?;
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
                    Some(site) => collect_native_roots(
                        builder,
                        values,
                        &plan.local_kinds,
                        &site.stack,
                        &stack,
                    )?,
                    None => Vec::new(),
                };
                let zero = builder.ins().iconst(types::I64, 0);
                let (arguments, function_offset, result_kind) = match instruction {
                    Instr::Native(NativeInstr::StrConcat) => {
                        let right = pop_native(&mut stack)?;
                        let left = pop_native(&mut stack)?;
                        (
                            [left, right, zero],
                            mem::offset_of!(RawNativeFunctions, text_concat),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::StrStartsWith) => {
                        let prefix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, prefix, zero],
                            mem::offset_of!(RawNativeFunctions, text_starts_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrEndsWith) => {
                        let suffix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, suffix, zero],
                            mem::offset_of!(RawNativeFunctions, text_ends_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrContains) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            mem::offset_of!(RawNativeFunctions, text_contains),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::StrFindIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            mem::offset_of!(RawNativeFunctions, text_find_scalar),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::TextFindByteIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, zero],
                            mem::offset_of!(RawNativeFunctions, text_find_byte),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextTrim
                        | NativeInstr::TextTrimStart
                        | NativeInstr::TextTrimEnd),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let function_offset = match operation {
                            NativeInstr::TextTrim => {
                                mem::offset_of!(RawNativeFunctions, text_trim)
                            }
                            NativeInstr::TextTrimStart => {
                                mem::offset_of!(RawNativeFunctions, text_trim_start)
                            }
                            NativeInstr::TextTrimEnd => {
                                mem::offset_of!(RawNativeFunctions, text_trim_end)
                            }
                            _ => return Err(CompileError::Backend),
                        };
                        ([text, zero, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextToLowerAscii | NativeInstr::TextToUpperAscii),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextToLowerAscii)
                        {
                            mem::offset_of!(RawNativeFunctions, text_lower_ascii)
                        } else {
                            mem::offset_of!(RawNativeFunctions, text_upper_ascii)
                        };
                        ([text, zero, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(NativeInstr::TextReplace) => {
                        let replacement = pop_native(&mut stack)?;
                        let needle = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, needle, replacement],
                            mem::offset_of!(RawNativeFunctions, text_replace),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextParseIntStatus
                        | NativeInstr::TextParseIntValue),
                    ) => {
                        let radix = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset =
                            if matches!(operation, NativeInstr::TextParseIntStatus) {
                                mem::offset_of!(RawNativeFunctions, text_parse_int_status)
                            } else {
                                mem::offset_of!(RawNativeFunctions, text_parse_int_value)
                            };
                        ([text, radix, zero], function_offset, ScalarKind::Int)
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextPadStart | NativeInstr::TextPadEnd),
                    ) => {
                        let width = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextPadStart) {
                            mem::offset_of!(RawNativeFunctions, text_pad_start)
                        } else {
                            mem::offset_of!(RawNativeFunctions, text_pad_end)
                        };
                        ([text, width, zero], function_offset, ScalarKind::Object(0))
                    }
                    Instr::Native(NativeInstr::BytesEndsWith) => {
                        let suffix = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, suffix, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_ends_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::BytesContains) => {
                        let needle = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, needle, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_contains),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::TextSplit) => {
                        let separator = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        (
                            [text, separator, zero],
                            mem::offset_of!(RawNativeFunctions, text_split),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextLines) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            mem::offset_of!(RawNativeFunctions, text_lines),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(
                        operation @ (NativeInstr::TextSlice | NativeInstr::TextSliceBytes),
                    ) => {
                        let length = pop_native(&mut stack)?;
                        let start = pop_native(&mut stack)?;
                        let text = pop_native(&mut stack)?;
                        let function_offset = if matches!(operation, NativeInstr::TextSlice) {
                            mem::offset_of!(RawNativeFunctions, text_slice)
                        } else {
                            mem::offset_of!(RawNativeFunctions, text_slice_bytes)
                        };
                        (
                            [text, start, length],
                            function_offset,
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextBytes) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            mem::offset_of!(RawNativeFunctions, text_bytes),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::TextToString) => {
                        let text = pop_native(&mut stack)?;
                        (
                            [text, zero, zero],
                            mem::offset_of!(RawNativeFunctions, text_to_string),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BytesText) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_text),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BbFindFrom) => {
                        let start = pop_native(&mut stack)?;
                        let needle = pop_native(&mut stack)?;
                        let buffer = pop_native(&mut stack)?;
                        (
                            [buffer, needle, start],
                            mem::offset_of!(RawNativeFunctions, byte_buffer_find_from),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::BytesStartsWith) => {
                        let prefix = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, prefix, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_starts_with),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Native(NativeInstr::BytesFindIndex) => {
                        let needle = pop_native(&mut stack)?;
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, needle, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_find_index),
                            ScalarKind::Int,
                        )
                    }
                    Instr::Native(NativeInstr::BytesHex) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_hex),
                            ScalarKind::Object(0),
                        )
                    }
                    Instr::Native(NativeInstr::BytesIsUtf8) => {
                        let bytes = pop_native(&mut stack)?;
                        (
                            [bytes, zero, zero],
                            mem::offset_of!(RawNativeFunctions, bytes_is_utf8),
                            ScalarKind::Bool,
                        )
                    }
                    Instr::Numeric(
                        operation @ (NumericInstr::TextParseFloatStatus
                        | NumericInstr::TextParseFloatValue),
                    ) => {
                        let text = pop_native(&mut stack)?;
                        let (function_offset, result_kind) =
                            if matches!(operation, NumericInstr::TextParseFloatStatus) {
                                (
                                    mem::offset_of!(RawNativeFunctions, text_parse_float_status),
                                    ScalarKind::Int,
                                )
                            } else {
                                (
                                    mem::offset_of!(RawNativeFunctions, text_parse_float_value),
                                    ScalarKind::Float,
                                )
                            };
                        ([text, zero, zero], function_offset, result_kind)
                    }
                    Instr::Numeric(NumericInstr::FloatFixed) => {
                        let digits = pop_native(&mut stack)?;
                        let value = pop_native(&mut stack)?;
                        (
                            [value, digits, zero],
                            mem::offset_of!(RawNativeFunctions, float_fixed),
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
                        fault_stack: &stack,
                        deopt_stack: &deopt_stack,
                    },
                )?;
                push_static(builder, &mut stack, result_kind, result)?;
            }
            Instr::EqRef | Instr::NeRef => {
                let release_right = virtual_stack.last().copied().unwrap_or(false);
                let release_left = virtual_stack
                    .len()
                    .checked_sub(2)
                    .and_then(|index| virtual_stack.get(index))
                    .copied()
                    .unwrap_or(false);
                let right = pop_native(&mut stack)?;
                let left = pop_native(&mut stack)?;
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
                push_static(builder, &mut stack, ScalarKind::Bool, result)?;
            }
            Instr::Native(operation) => {
                emit_char_instruction(builder, &mut stack, operation)?;
            }
            Instr::Numeric(operation) => {
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

fn emit_interface_receiver_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let object = builder.create_block();
    let immediate = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let is_object = builder
        .ins()
        .icmp_imm(IntCC::Equal, receiver.tag, ValueTag::Obj as u64 as i64);
    builder.ins().brif(is_object, object, &[], immediate, &[]);

    builder.switch_to_block(immediate);
    builder.ins().jump(done, &[receiver.tag.into()]);

    builder.switch_to_block(object);
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let entry = emit_heap_entry(
        builder,
        values,
        receiver.bits,
        guard_point,
        ObjectGuard::Replay(stack),
    )?;
    let object_tag = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
    let object_key = builder.ins().uextend(types::I64, object_tag);
    let object_key = builder.ins().bor_imm(object_key, 1_i64 << 62);
    let instance = builder.create_block();
    let other_object = builder.create_block();
    let is_instance =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, object_tag, i64::from(JIT_OBJECT_INSTANCE));
    builder
        .ins()
        .brif(is_instance, instance, &[], other_object, &[]);

    builder.switch_to_block(instance);
    let class = load_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let class_key = builder.ins().uextend(types::I64, class);
    let class_key = builder.ins().bor_imm(class_key, i64::MIN);
    builder.ins().jump(done, &[class_key.into()]);

    builder.switch_to_block(other_object);
    builder.ins().jump(done, &[object_key.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_image_slot_call_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    slot: u32,
    constructor: bool,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I32);

    let count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, image_slot_count),
    )?;
    let slot_index = builder.ins().iconst(values.pointer_type, i64::from(slot));
    let in_range = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, count, slot_index);
    let zero = builder.ins().iconst(types::I32, 0);
    let invalid = builder.ins().iconst(
        types::I32,
        i64::from(abi_fault_index(lm_abi::FaultCode::InvalidVmState)? + 1),
    );
    builder.ins().brif(in_range, present, &[], missing, &[]);

    builder.switch_to_block(missing);
    builder.ins().jump(done, &[zero.into(), invalid.into()]);

    builder.switch_to_block(present);
    let slots = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, image_slots),
    )?;
    let offset = builder.ins().imul_imm(
        slot_index,
        i64::try_from(mem::size_of::<NativeImageSlot>()).map_err(|_| CompileError::Backend)?,
    );
    let address = builder.ins().iadd(slots, offset);
    let kind = load_value(
        builder,
        types::I32,
        address,
        mem::offset_of!(NativeImageSlot, kind),
    )?;
    let target_offset = if constructor {
        mem::offset_of!(NativeImageSlot, second)
    } else {
        mem::offset_of!(NativeImageSlot, first)
    };
    let target = load_value(builder, types::I32, address, target_offset)?;
    let expected_kind = if constructor {
        IMAGE_SLOT_CLASS
    } else {
        IMAGE_SLOT_FUNCTION
    };
    let valid = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(expected_kind));
    let empty = builder
        .ins()
        .icmp_imm(IntCC::Equal, kind, i64::from(IMAGE_SLOT_EMPTY));
    let malformed = builder.ins().iconst(
        types::I32,
        i64::from(abi_fault_index(lm_abi::FaultCode::MalformedState)? + 1),
    );
    let fault = builder.ins().select(empty, invalid, malformed);
    let fault = builder.ins().select(valid, zero, fault);
    builder.ins().jump(done, &[target.into(), fault.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

fn abi_fault_index(fault: lm_abi::FaultCode) -> Result<u32, CompileError> {
    lm_abi::FAULT_CODES
        .iter()
        .position(|candidate| *candidate == fault)
        .and_then(|index| u32::try_from(index).ok())
        .ok_or(CompileError::Backend)
}

fn emit_generic_virtual_receiver_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    contract: VirtualReceiver,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let VirtualReceiver::Instance { class } = contract else {
        return Err(CompileError::Backend);
    };
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let (entry, actual) = emit_instance_entry(
        builder,
        values,
        receiver.bits,
        class,
        guard_point,
        ObjectGuard::Replay(stack),
        ObjectGuard::Replay(stack),
    )?;
    let environment = load_value(builder, types::I32, entry, JIT_INSTANCE_ENV_OFFSET)?;
    let class_key = builder.ins().uextend(types::I64, actual);
    let environment_key = builder.ins().uextend(types::I64, environment);
    let environment_key = builder.ins().ishl_imm(environment_key, 32);
    Ok(builder.ins().bor(environment_key, class_key))
}

fn emit_call_value_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    callable: NativeValue,
    target: ValueCallTarget,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<NativeCallTarget, CompileError> {
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    match target {
        ValueCallTarget::Closure => {
            let wrong_tag =
                builder
                    .ins()
                    .icmp_imm(IntCC::NotEqual, callable.tag, ValueTag::Obj as u64 as i64);
            emit_interpreter_replay(builder, values, wrong_tag, guard_point, stack)?;
            let entry = emit_object_entry(
                builder,
                values,
                callable.bits,
                JIT_OBJECT_CLOSURE,
                guard_point,
                ObjectGuard::Replay(stack),
            )?;
            let function = load_value(builder, types::I32, entry, JIT_CLOSURE_FUNCTION_OFFSET)?;
            let environment = load_value(builder, types::I32, entry, JIT_CLOSURE_ENV_OFFSET)?;
            let capture_data = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_DATA_OFFSET,
            )?;
            let capture_len = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_LEN_OFFSET,
            )?;
            Ok(NativeCallTarget {
                function,
                environment,
                capture_data,
                capture_len,
                fault: None,
            })
        }
        ValueCallTarget::Callback => {
            let closure = builder.create_block();
            let test_callback = builder.create_block();
            let callback = builder.create_block();
            let invalid = builder.create_block();
            let done = builder.create_block();
            builder.append_block_param(done, types::I32);
            builder.append_block_param(done, types::I32);
            builder.append_block_param(done, values.pointer_type);
            builder.append_block_param(done, values.pointer_type);
            let is_closure =
                builder
                    .ins()
                    .icmp_imm(IntCC::Equal, callable.tag, ValueTag::Obj as u64 as i64);
            builder
                .ins()
                .brif(is_closure, closure, &[], test_callback, &[]);

            builder.switch_to_block(test_callback);
            let is_callback = builder.ins().icmp_imm(
                IntCC::Equal,
                callable.tag,
                ValueTag::Callback as u64 as i64,
            );
            builder.ins().brif(is_callback, callback, &[], invalid, &[]);

            builder.switch_to_block(invalid);
            let retired = emit_retired(builder, values);
            let zero = builder.ins().iconst(types::I64, 0);
            emit_exit(
                builder,
                values,
                ExitEmission {
                    retired,
                    kind: EXIT_REPLAY,
                    block: point.block,
                    instruction: point.instruction,
                    result: NativeValue {
                        bits: zero,
                        tag: zero,
                    },
                },
                stack,
            )?;

            builder.switch_to_block(closure);
            let entry = emit_object_entry(
                builder,
                values,
                callable.bits,
                JIT_OBJECT_CLOSURE,
                guard_point,
                ObjectGuard::Replay(stack),
            )?;
            let closure_function =
                load_value(builder, types::I32, entry, JIT_CLOSURE_FUNCTION_OFFSET)?;
            let closure_environment =
                load_value(builder, types::I32, entry, JIT_CLOSURE_ENV_OFFSET)?;
            let closure_capture_data = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_DATA_OFFSET,
            )?;
            let closure_capture_len = load_value(
                builder,
                values.pointer_type,
                entry,
                JIT_CLOSURE_CAPTURES_OFFSET + VALUE_ARRAY_LEN_OFFSET,
            )?;
            builder.ins().jump(
                done,
                &[
                    closure_function.into(),
                    closure_environment.into(),
                    closure_capture_data.into(),
                    closure_capture_len.into(),
                ],
            );

            builder.switch_to_block(callback);
            let callback_target = emit_resolved_call_lookup(
                builder,
                values,
                function,
                point,
                callable.bits,
                callable,
                EXIT_CALLBACK_CALL,
                stack,
            )?;
            builder.ins().jump(
                done,
                &[
                    callback_target.function.into(),
                    callback_target.environment.into(),
                    callback_target.capture_data.into(),
                    callback_target.capture_len.into(),
                ],
            );

            builder.switch_to_block(done);
            let values = builder.block_params(done);
            Ok(NativeCallTarget {
                function: values[0],
                environment: values[1],
                capture_data: values[2],
                capture_len: values[3],
                fault: None,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_resolved_call_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function: u32,
    point: FaultPoint,
    receiver_key: ir::Value,
    receiver: NativeValue,
    exit_kind: u32,
    stack: &[NativeValue],
) -> Result<NativeCallTarget, CompileError> {
    let hit = builder.create_block();
    let miss = builder.create_block();
    builder.append_block_param(hit, types::I32);
    builder.append_block_param(hit, types::I32);
    builder.append_block_param(hit, values.pointer_type);
    builder.append_block_param(hit, values.pointer_type);
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
        mem::offset_of!(RawNativeActivation, resolved_calls),
    )?;
    let mask = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, resolved_call_mask),
    )?;
    let shifted_parent = builder.ins().ushr_imm(parent, 16);
    let parent_hash = builder.ins().bxor(parent, shifted_parent);
    let shifted_receiver = builder.ins().ushr_imm(receiver_key, 32);
    let receiver_hash = builder.ins().bxor(receiver_key, shifted_receiver);
    let receiver_hash = builder.ins().ireduce(types::I32, receiver_hash);
    let rotated_receiver = builder.ins().rotl_imm(receiver_hash, 7);
    let receiver_hash = builder.ins().bxor(receiver_hash, rotated_receiver);
    let site_hash =
        crate::activation::type_environment_site_hash(function, point.block, point.instruction);
    let site_hash = builder.ins().iconst(types::I32, i64::from(site_hash));
    let set = builder.ins().bxor(site_hash, parent_hash);
    let set = builder.ins().bxor(set, receiver_hash);
    let set = builder.ins().band(set, mask);
    let set = builder.ins().uextend(values.pointer_type, set);
    let first = builder.ins().imul_imm(
        set,
        (RESOLVED_CALL_CACHE_WAYS * mem::size_of::<RawResolvedCallCacheEntry>()) as i64,
    );
    let first = builder.ins().iadd(cache, first);
    for index in 0..RESOLVED_CALL_CACHE_WAYS {
        let next = builder.create_block();
        let entry_offset = index
            .checked_mul(mem::size_of::<RawResolvedCallCacheEntry>())
            .ok_or(CompileError::Backend)?;
        let entry_offset = i64::try_from(entry_offset).map_err(|_| CompileError::Backend)?;
        let entry = builder.ins().iadd_imm(first, entry_offset);
        let cached_store = atomic_load_field(
            builder,
            entry,
            types::I64,
            mem::offset_of!(RawResolvedCallCacheEntry, store),
        )?;
        let cached_function = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, function),
        )?;
        let cached_block = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, block),
        )?;
        let cached_instruction = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, instruction),
        )?;
        let cached_parent = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, parent),
        )?;
        let cached_receiver = atomic_load_field(
            builder,
            entry,
            types::I64,
            mem::offset_of!(RawResolvedCallCacheEntry, receiver),
        )?;
        let target = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, target),
        )?;
        let environment = atomic_load_field(
            builder,
            entry,
            types::I32,
            mem::offset_of!(RawResolvedCallCacheEntry, environment),
        )?;
        let capture_data = atomic_load_field(
            builder,
            entry,
            values.pointer_type,
            mem::offset_of!(RawResolvedCallCacheEntry, capture_data),
        )?;
        let capture_len = atomic_load_field(
            builder,
            entry,
            values.pointer_type,
            mem::offset_of!(RawResolvedCallCacheEntry, capture_len),
        )?;
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
        let same_receiver = builder
            .ins()
            .icmp(IntCC::Equal, cached_receiver, receiver_key);
        let matched = builder.ins().band(same_store, same_function);
        let matched = builder.ins().band(matched, same_block);
        let matched = builder.ins().band(matched, same_instruction);
        let matched = builder.ins().band(matched, same_parent);
        let matched = builder.ins().band(matched, same_receiver);
        builder.ins().brif(
            matched,
            hit,
            &[
                target.into(),
                environment.into(),
                capture_data.into(),
                capture_len.into(),
            ],
            next,
            &[],
        );
        builder.switch_to_block(next);
    }
    builder.ins().jump(miss, &[]);

    builder.switch_to_block(miss);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: exit_kind,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: receiver_key,
                tag: receiver.tag,
            },
        },
        stack,
    )?;

    builder.switch_to_block(hit);
    let values = builder.block_params(hit);
    Ok(NativeCallTarget {
        function: values[0],
        environment: values[1],
        capture_data: values[2],
        capture_len: values[3],
        fault: None,
    })
}

fn atomic_load_field(
    builder: &mut FunctionBuilder<'_>,
    base: ir::Value,
    ty: ir::Type,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    let address = builder.ins().iadd_imm(
        base,
        i64::try_from(offset).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().atomic_load(ty, MemFlags::new(), address))
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
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
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
        capture,
        fallback: fallback_kind,
        contract,
        local_kinds,
        boundary_kinds,
        block,
        instruction,
        successor_entry,
        successor,
    } = call;
    let NativeCallTarget {
        function: target,
        environment,
        capture_data,
        capture_len,
        fault,
    } = target;
    let argument_start = stack
        .len()
        .checked_sub(contract.params.len())
        .ok_or(CompileError::Backend)?;
    let caller_end = argument_start
        .checked_sub(usize::from(capture.is_some()))
        .ok_or(CompileError::Backend)?;
    let boundary_stack = stack.clone();
    let caller_stack = stack[..caller_end].to_vec();
    if boundary_kinds.len() != boundary_stack.len() {
        return Err(CompileError::Backend);
    }
    let caller_stack_kinds = &boundary_kinds[..caller_end];
    let arguments = stack[argument_start..].to_vec();
    let stack_limit_stack = if capture.is_some() {
        caller_stack
            .iter()
            .chain(arguments.iter())
            .copied()
            .collect::<Vec<_>>()
    } else {
        boundary_stack.clone()
    };
    if let Some(scalar) = contract.scalar_result.as_ref() {
        let scalar_path = builder.create_block();
        let native_path = builder.create_block();
        let ready = emit_scalar_replacement_guard(builder, values, scalar)?;
        builder
            .ins()
            .brif(ready, scalar_path, &[], native_path, &[]);

        builder.switch_to_block(scalar_path);
        emit_scalar_replacement(
            builder,
            values,
            scalar,
            &arguments,
            &caller_stack,
            successor,
        )?;

        builder.switch_to_block(native_path);
    }
    let hard_check = builder.create_block();
    let fuel_exit = builder.create_block();
    let lookup = builder.create_block();
    let fallback = builder.create_block();
    let root_check = builder.create_block();
    let grow_roots = builder.create_block();
    let stack_limit = builder.create_block();
    let capacity = builder.create_block();
    let storage = builder.create_block();
    let grow = builder.create_block();
    let invoke = builder.create_block();
    let returned = builder.create_block();
    let propagate = builder.create_block();
    let preflight_exit = builder.create_block();
    builder.append_block_param(preflight_exit, types::I32);
    builder.append_block_param(preflight_exit, types::I64);
    builder.append_block_param(preflight_exit, types::I64);

    builder.set_cold_block(hard_check);
    builder.set_cold_block(fuel_exit);
    builder.set_cold_block(preflight_exit);
    let fuel = builder.use_var(values.fuel);
    let has_fuel = builder
        .ins()
        .icmp_imm(IntCC::SignedGreaterThanOrEqual, fuel, 1);
    builder.ins().brif(has_fuel, lookup, &[], hard_check, &[]);

    builder.switch_to_block(hard_check);
    let retired = emit_retired(builder, values);
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let remaining = builder.ins().isub(hard_fuel, retired);
    let has_hard_fuel = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, remaining, 1);
    builder
        .ins()
        .brif(has_hard_fuel, lookup, &[], fuel_exit, &[]);

    builder.switch_to_block(fuel_exit);
    let kind = builder.ins().iconst(types::I32, i64::from(EXIT_FUEL));
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .jump(preflight_exit, &[kind.into(), zero.into(), zero.into()]);

    builder.switch_to_block(lookup);
    if let Some(fault) = fault {
        let invalid = builder.ins().icmp_imm(IntCC::NotEqual, fault, 0);
        let fault_block = builder.create_block();
        let valid_block = builder.create_block();
        builder
            .ins()
            .brif(invalid, fault_block, &[], valid_block, &[]);

        builder.switch_to_block(fault_block);
        emit_charge(builder, values, 1);
        let retired = emit_retired(builder, values);
        let code = builder.ins().iadd_imm(fault, -1);
        let code = builder.ins().uextend(types::I64, code);
        let zero = builder.ins().iconst(types::I64, 0);
        emit_exit(
            builder,
            values,
            ExitEmission {
                retired,
                kind: EXIT_GUEST_FAULT,
                block,
                instruction: instruction + 1,
                result: NativeValue {
                    bits: code,
                    tag: zero,
                },
            },
            &boundary_stack,
        )?;

        builder.switch_to_block(valid_block);
    }
    let entry_count = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, entry_count),
    )?;
    let target_in_range = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, entry_count, target);
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
    let target_index = builder.ins().uextend(values.pointer_type, target);
    let entry_offset = builder
        .ins()
        .imul_imm(target_index, mem::size_of::<usize>() as i64);
    let entry_address = builder.ins().iadd(entries, entry_offset);
    let cell = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), entry_address, 0);
    let code = builder
        .ins()
        .atomic_load(values.pointer_type, MemFlags::new(), cell);
    let published = builder.ins().icmp_imm(IntCC::NotEqual, code, 0);
    let limits = builder.create_block();
    builder
        .ins()
        .brif(published, root_check, &[], fallback, &[]);

    builder.switch_to_block(root_check);
    let required_roots = load_cell_u32(builder, cell, mem::offset_of!(NativeEntryCell, max_roots))?;
    let root_capacity = load_activation_u32(builder, values, RawActivationField::RootCapacity)?;
    let roots_fit = builder.ins().icmp(
        IntCC::UnsignedLessThanOrEqual,
        required_roots,
        root_capacity,
    );
    builder.ins().brif(roots_fit, limits, &[], grow_roots, &[]);

    builder.switch_to_block(grow_roots);
    let kind = builder.ins().iconst(types::I32, i64::from(EXIT_GROW_ROOTS));
    let required_roots = builder.ins().uextend(types::I64, required_roots);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(
        preflight_exit,
        &[kind.into(), required_roots.into(), zero.into()],
    );

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
    let caller_values = builder.ins().iadd_imm(
        active_values,
        -i64::try_from(
            contract
                .params
                .len()
                .checked_add(usize::from(capture.is_some()))
                .ok_or(CompileError::Backend)?,
        )
        .map_err(|_| CompileError::Backend)?,
    );
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
    let retired = emit_retired(builder, values);
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
        &stack_limit_stack,
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
    let compatible = match contract.local_count {
        Some(expected) => {
            let local_count_matches = builder.ins().icmp_imm(
                IntCC::Equal,
                local_count,
                i64::try_from(expected).map_err(|_| CompileError::Backend)?,
            );
            builder.ins().band(body_fits, local_count_matches)
        }
        None => body_fits,
    };
    builder.ins().brif(compatible, storage, &[], fallback, &[]);

    builder.switch_to_block(storage);
    let storage_fits = builder.ins().band(frame_fits, scalars_fit);
    builder.ins().brif(storage_fits, invoke, &[], grow, &[]);

    builder.switch_to_block(grow);
    let target_value = builder.ins().uextend(types::I64, target);
    let required_scalars = builder.ins().uextend(types::I64, scalar_end);
    let required_scalars = builder.ins().ishl_imm(required_scalars, 32);
    let growth = builder.ins().bor(required_scalars, target_value);
    let environment_tag = builder.ins().uextend(types::I64, environment);
    let kind = builder
        .ins()
        .iconst(types::I32, i64::from(EXIT_GROW_ACTIVATION));
    builder.ins().jump(
        preflight_exit,
        &[kind.into(), growth.into(), environment_tag.into()],
    );

    builder.switch_to_block(fallback);
    let (kind, result) = match fallback_kind {
        NativeCallFallback::Direct => {
            let target_value = builder.ins().uextend(types::I64, target);
            let environment_tag = builder.ins().uextend(types::I64, environment);
            (
                EXIT_CALL,
                NativeValue {
                    bits: target_value,
                    tag: environment_tag,
                },
            )
        }
        NativeCallFallback::Replay => {
            let zero = builder.ins().iconst(types::I64, 0);
            (
                EXIT_REPLAY,
                NativeValue {
                    bits: zero,
                    tag: zero,
                },
            )
        }
    };
    let kind = builder.ins().iconst(types::I32, i64::from(kind));
    builder.ins().jump(
        preflight_exit,
        &[kind.into(), result.bits.into(), result.tag.into()],
    );

    builder.switch_to_block(preflight_exit);
    let kind = builder.block_params(preflight_exit)[0];
    let result = NativeValue {
        bits: builder.block_params(preflight_exit)[1],
        tag: builder.block_params(preflight_exit)[2],
    };
    let retired = emit_retired(builder, values);
    let locals = capture_local_values(builder, values)?;
    emit_exit_with_locals_and_kind(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_FUEL,
            block,
            instruction,
            result,
        },
        kind,
        &locals,
        &boundary_stack,
    )?;

    builder.switch_to_block(invoke);
    emit_charge(builder, values, 1);
    let prior_changed = load_activation_u32(builder, values, RawActivationField::ChangedFrom)?;
    let caller_frame = emit_current_frame_pointer(builder, values)?;
    emit_spill_frame_roots(
        builder,
        values,
        caller_frame,
        local_kinds,
        caller_stack_kinds,
        &caller_stack,
    )?;
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let scalar_base = scalar_len;
    let scalar_base_pointer = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_byte_offset = builder.ins().ishl_imm(scalar_base_pointer, 3);
    let child_locals = builder.ins().iadd(scalars, scalar_byte_offset);
    let child_tags = builder.ins().iadd(tags, scalar_byte_offset);
    let child_states = builder.ins().iadd(states, scalar_base_pointer);
    let zero_i8 = builder.ins().iconst(types::I8, 0);
    match contract.local_count {
        Some(local_count) => {
            for slot in 0..local_count {
                let offset = i32::try_from(slot).map_err(|_| CompileError::Backend)?;
                builder
                    .ins()
                    .store(MemFlags::new(), zero_i8, child_states, offset);
            }
        }
        None => emit_clear_local_states(builder, child_states, local_count, zero_i8),
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
    store_i32_value(
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
    let capture_tag = capture.map_or_else(
        || {
            builder
                .ins()
                .iconst(types::I64, ValueTag::Uninit as u64 as i64)
        },
        |capture| capture.tag,
    );
    let capture_bits = capture.map_or_else(
        || builder.ins().iconst(types::I64, 0),
        |capture| capture.bits,
    );
    store_i64(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, capture_tag),
        capture_tag,
    )?;
    store_i64(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, capture_bits),
        capture_bits,
    )?;
    store_i64(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, capture_data),
        capture_data,
    )?;
    store_i64(
        builder,
        child_frame,
        mem::offset_of!(RawNativeFrame, capture_len),
        capture_len,
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
    if contract.virtual_result {
        let request = builder.ins().iconst(types::I32, 1);
        store_i32_value(
            builder,
            values.activation_pointer,
            mem::offset_of!(RawNativeActivation, virtual_request),
            request,
        )?;
    }
    let caller_retired = emit_retired(builder, values);
    let zero_entry = builder.ins().iconst(types::I32, 0);
    builder.ins().call_indirect(
        values.native_signature,
        code,
        &[values.activation_pointer, caller_retired, zero_entry],
    );
    let child_retired = load_value(
        builder,
        types::I64,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
    )?;
    let poll_deadline = load_activation_u64(builder, values, RawActivationField::PollDeadline)?;
    let remaining_fuel = builder.ins().isub(poll_deadline, child_retired);
    builder.def_var(values.fuel, remaining_fuel);
    builder.def_var(values.retired, child_retired);
    let total_retired = child_retired;
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
    stack.truncate(caller_end);
    stack.push(NativeValue {
        bits: result,
        tag: result_tag,
    });
    define_stack(builder, values, stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

fn emit_clear_local_states(
    builder: &mut FunctionBuilder<'_>,
    states: ir::Value,
    count: ir::Value,
    zero: ir::Value,
) {
    let test = builder.create_block();
    let clear = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(test, types::I32);
    builder.append_block_param(clear, types::I32);
    let first = builder.ins().iconst(types::I32, 0);
    builder.ins().jump(test, &[first.into()]);

    builder.switch_to_block(test);
    let index = builder.block_params(test)[0];
    let complete = builder.ins().icmp(IntCC::Equal, index, count);
    builder
        .ins()
        .brif(complete, done, &[], clear, &[index.into()]);

    builder.switch_to_block(clear);
    let index = builder.block_params(clear)[0];
    let pointer_type = builder.func.dfg.value_type(states);
    let offset = builder.ins().uextend(pointer_type, index);
    let address = builder.ins().iadd(states, offset);
    builder.ins().store(MemFlags::new(), zero, address, 0);
    let next = builder.ins().iadd_imm(index, 1);
    builder.ins().jump(test, &[next.into()]);

    builder.switch_to_block(done);
}

fn emit_scalar_replacement_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    scalar: &ScalarReplacement,
) -> Result<ir::Value, CompileError> {
    let instance = values
        .scalar_instances
        .get(scalar.site as usize)
        .ok_or(CompileError::Backend)?;
    let active = builder.use_var(instance.active);
    let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let base_frames = load_activation_u32(builder, values, RawActivationField::BaseFrames)?;
    let frames = builder.ins().iadd(base_frames, frame_len);
    let frames = builder
        .ins()
        .iadd_imm(frames, i64::from(scalar.frame_count));
    let max_frames = load_activation_u32(builder, values, RawActivationField::MaxFrames)?;
    let frames_fit = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, frames, max_frames);

    let scalar_len = load_activation_u32(builder, values, RawActivationField::ScalarLen)?;
    let required_values = builder
        .ins()
        .iadd_imm(scalar_len, i64::from(scalar.stack_values));
    let max_values = load_activation_u32(builder, values, RawActivationField::MaxStackValues)?;
    let values_fit =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThanOrEqual, required_values, max_values);

    let cost = scalar_instance_cost(scalar.fields.len())?;
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let cost_value = builder.ins().iconst(values.pointer_type, cost);
    let zero = builder.ins().iconst(values.pointer_type, 0);
    let additional = builder.ins().select(active, zero, cost_value);
    let next = builder.ins().iadd(used, additional);
    let no_overflow = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, next, used);
    let threshold = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let heap_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next, threshold);
    let heap_ready = builder.ins().bor(active, heap_fits);
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let record_bit = 1u64.checked_shl(scalar.site).ok_or(CompileError::Backend)?;
    let record = builder.ins().band_imm(available, record_bit as i64);
    let record = builder.ins().icmp_imm(IntCC::NotEqual, record, 0);
    let record_ready = builder.ins().bor(active, record);
    let limits_fit = builder.ins().band(frames_fit, values_fit);
    let heap_ready = builder.ins().band(no_overflow, heap_ready);
    let ready = builder.ins().band(heap_ready, record_ready);
    Ok(builder.ins().band(limits_fit, ready))
}

fn emit_scalar_replacement(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    scalar: &ScalarReplacement,
    arguments: &[NativeValue],
    caller_stack: &[NativeValue],
    successor: ir::Block,
) -> Result<(), CompileError> {
    let instance = values
        .scalar_instances
        .get(scalar.site as usize)
        .ok_or(CompileError::Backend)?;
    if instance.fields.len() != scalar.fields.len() {
        return Err(CompileError::Backend);
    }
    let active = builder.use_var(instance.active);
    let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
    let ready = builder.create_block();
    let reserve = builder.create_block();
    builder.ins().brif(active, ready, &[], reserve, &[]);

    builder.switch_to_block(reserve);
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let record_bit = 1u64.checked_shl(scalar.site).ok_or(CompileError::Backend)?;
    let available = builder.ins().band_imm(available, !(record_bit as i64));
    store_i64(
        builder,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
        available,
    )?;
    let cost = scalar_instance_cost(scalar.fields.len())?;
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let used = builder.ins().iadd_imm(used, cost);
    store_heap_value(builder, used_pointer, 0, used)?;
    let live_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, live)?;
    let one = builder.ins().iconst(types::I64, 1);
    builder.def_var(instance.active, one);
    builder.ins().jump(ready, &[]);

    builder.switch_to_block(ready);
    for (target, source) in instance.fields.iter().zip(&scalar.fields) {
        let value = match source {
            ScalarFieldSource::Parameter(parameter) => arguments
                .get(*parameter as usize)
                .copied()
                .ok_or(CompileError::Backend)?,
            ScalarFieldSource::Constant(value) => NativeValue {
                bits: builder.ins().iconst(types::I64, value.bits as i64),
                tag: builder.ins().iconst(types::I64, value.tag as i64),
            },
        };
        builder.def_var(target.bits, value.bits);
        builder.def_var(target.tag, value.tag);
    }
    let count = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, scalar_replaced_allocations),
    )?;
    let count = builder.ins().iadd_imm(count, 1);
    store_i64(
        builder,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, scalar_replaced_allocations),
        count,
    )?;
    emit_charge(
        builder,
        values,
        scalar
            .retired_cost
            .checked_add(1)
            .ok_or(CompileError::Backend)?,
    );
    let mut stack = caller_stack.to_vec();
    stack.push(NativeValue {
        bits: builder.ins().iconst(types::I64, instance.token as i64),
        tag: builder
            .ins()
            .iconst(types::I64, ValueTag::Obj as u64 as i64),
    });
    define_stack(builder, values, &stack)?;
    builder.ins().jump(successor, &[]);
    Ok(())
}

fn scalar_instance_cost(field_count: usize) -> Result<i64, CompileError> {
    let cost = field_count
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    i64::try_from(cost).map_err(|_| CompileError::Backend)
}

fn emit_virtual_target(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    receiver: NativeValue,
    contract: VirtualReceiver,
    selector: u32,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let guard_point = FaultPoint {
        block: point.block,
        instruction: point.instruction.saturating_add(1),
        prefix: point.prefix.saturating_add(1),
    };
    let class = match contract {
        VirtualReceiver::Immediate { class } => builder.ins().iconst(types::I32, i64::from(class)),
        VirtualReceiver::Object { tag, class } => {
            emit_object_entry(
                builder,
                values,
                receiver.bits,
                tag,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
            )?;
            builder.ins().iconst(types::I32, i64::from(class))
        }
        VirtualReceiver::Instance { class } => {
            let (_, actual) = emit_instance_entry(
                builder,
                values,
                receiver.bits,
                class,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
                ObjectGuard::Replay(deopt_stack),
            )?;
            actual
        }
        VirtualReceiver::Text { string, substring } => {
            let entry = emit_text_entry(
                builder,
                values,
                receiver.bits,
                guard_point,
                ObjectGuard::Replay(deopt_stack),
            )?;
            let tag = load_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
            let is_string = builder
                .ins()
                .icmp_imm(IntCC::Equal, tag, i64::from(JIT_OBJECT_STR));
            let string = builder.ins().iconst(types::I32, i64::from(string));
            let substring = builder.ins().iconst(types::I32, i64::from(substring));
            builder.ins().select(is_string, string, substring)
        }
    };

    let class_index = builder.ins().uextend(values.pointer_type, class);
    let row_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, dispatch_row_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, class_index, row_count);
    emit_interpreter_replay(builder, values, outside, guard_point, deopt_stack)?;
    let rows = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, dispatch_rows),
    )?;
    let row_offset = builder
        .ins()
        .imul_imm(class_index, mem::size_of::<NativeDispatchRow>() as i64);
    let row = builder.ins().iadd(rows, row_offset);
    let base = load_value(
        builder,
        types::I32,
        row,
        mem::offset_of!(NativeDispatchRow, base),
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        row,
        mem::offset_of!(NativeDispatchRow, len),
    )?;
    let start = load_value(
        builder,
        values.pointer_type,
        row,
        mem::offset_of!(NativeDispatchRow, start),
    )?;
    let selector = builder.ins().iconst(types::I32, i64::from(selector));
    let below = builder.ins().icmp(IntCC::UnsignedLessThan, selector, base);
    emit_interpreter_replay(builder, values, below, guard_point, deopt_stack)?;
    let offset = builder.ins().isub(selector, base);
    let offset = builder.ins().uextend(values.pointer_type, offset);
    let past = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, offset, len);
    emit_interpreter_replay(builder, values, past, guard_point, deopt_stack)?;
    let method_index = builder.ins().iadd(start, offset);
    let method_count = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, dispatch_method_count),
    )?;
    let method_outside = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        method_index,
        method_count,
    );
    emit_interpreter_replay(builder, values, method_outside, guard_point, deopt_stack)?;
    let methods = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, dispatch_methods),
    )?;
    let method_offset = builder
        .ins()
        .imul_imm(method_index, mem::size_of::<u32>() as i64);
    let method_address = builder.ins().iadd(methods, method_offset);
    let target = builder
        .ins()
        .load(types::I32, MemFlags::trusted(), method_address, 0);
    let missing = builder
        .ins()
        .icmp_imm(IntCC::Equal, target, u32::MAX as i64);
    emit_interpreter_replay(builder, values, missing, guard_point, deopt_stack)?;
    Ok(target)
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

fn emit_charge(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>, cost: u32) {
    let fuel = builder.use_var(values.fuel);
    let fuel = builder.ins().iadd_imm(fuel, -i64::from(cost));
    builder.def_var(values.fuel, fuel);
    let retired = builder.use_var(values.retired);
    let retired = builder.ins().iadd_imm(retired, i64::from(cost));
    builder.def_var(values.retired, retired);
}

fn emit_retired(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>) -> ir::Value {
    builder.use_var(values.retired)
}

fn emit_retired_with_prefix(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    prefix: u32,
) -> ir::Value {
    let retired = emit_retired(builder, values);
    builder.ins().iadd_imm(retired, i64::from(prefix))
}

fn emit_segment_charge(builder: &mut FunctionBuilder<'_>, values: NativeValues<'_>, cost: u32) {
    emit_charge(builder, values, cost);
}

fn emit_reservation_boundary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    segment: &Segment,
    continuation: ir::Block,
) -> Result<(), CompileError> {
    let check_poll = builder.create_block();
    let check_request = builder.create_block();
    let load_request = builder.create_block();
    let rearm = builder.create_block();
    let fuel_exit = builder.create_block();
    let poll_exit = builder.create_block();
    let yield_exit = builder.create_block();
    builder.append_block_param(yield_exit, types::I32);
    builder.set_cold_block(check_poll);
    builder.set_cold_block(check_request);
    builder.set_cold_block(load_request);
    builder.set_cold_block(rearm);
    builder.set_cold_block(fuel_exit);
    builder.set_cold_block(poll_exit);
    builder.set_cold_block(yield_exit);
    let retired = emit_retired(builder, values);
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let hard_remaining = builder.ins().isub(hard_fuel, retired);
    let has_hard_fuel = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        hard_remaining,
        i64::from(segment.fuel_reserve),
    );
    builder
        .ins()
        .brif(has_hard_fuel, check_poll, &[], fuel_exit, &[]);

    builder.switch_to_block(fuel_exit);
    let fuel_kind = builder.ins().iconst(types::I32, i64::from(EXIT_FUEL));
    builder.ins().jump(yield_exit, &[fuel_kind.into()]);

    builder.switch_to_block(check_poll);
    let deadline = load_activation_u64(builder, values, RawActivationField::PollDeadline)?;
    let due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, retired, deadline);
    builder
        .ins()
        .brif(due, check_request, &[], continuation, &[]);

    builder.switch_to_block(check_request);
    let requested = load_activation_pointer(builder, values, RawActivationField::PollRequested)?;
    let enabled = builder.ins().icmp_imm(IntCC::NotEqual, requested, 0);
    builder.ins().brif(enabled, load_request, &[], rearm, &[]);

    builder.switch_to_block(load_request);
    let request = builder
        .ins()
        .atomic_load(types::I32, MemFlags::new(), requested);
    let idle = builder.ins().icmp_imm(IntCC::Equal, request, 0);
    builder.ins().brif(idle, rearm, &[], poll_exit, &[]);

    builder.switch_to_block(rearm);
    emit_native_poll_rearm_values(builder, values, retired)?;
    builder.ins().jump(continuation, &[]);

    builder.switch_to_block(poll_exit);
    let poll_kind = builder.ins().iconst(types::I32, i64::from(EXIT_POLL));
    builder.ins().jump(yield_exit, &[poll_kind.into()]);

    builder.switch_to_block(yield_exit);
    let kind = builder.block_params(yield_exit)[0];
    emit_entry_exit_with_kind(builder, values, segment, EXIT_FUEL, kind)
}

fn emit_native_poll_rearm_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    retired: ir::Value,
) -> Result<(), CompileError> {
    let hard_fuel = load_activation_u64(builder, values, RawActivationField::HardFuel)?;
    let remaining = builder.ins().isub(hard_fuel, retired);
    let interval = load_activation_u32(builder, values, RawActivationField::PollInterval)?;
    let interval = builder.ins().uextend(types::I64, interval);
    let use_interval = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, interval, remaining);
    let next_fuel = builder.ins().select(use_interval, interval, remaining);
    let next_deadline = builder.ins().iadd(retired, next_fuel);
    store_activation_u64(
        builder,
        values,
        RawActivationField::PollDeadline,
        next_deadline,
    )?;
    builder.def_var(values.fuel, next_fuel);
    Ok(())
}

fn capture_local_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<Vec<NativeValue>, CompileError> {
    values
        .locals
        .iter()
        .copied()
        .enumerate()
        .map(|(slot, variable)| {
            Ok(NativeValue {
                bits: builder.use_var(variable),
                tag: emit_slot_tag(builder, values.local_tags[slot], values.local_kinds[slot])?,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn emit_deferred_integer_overflow_replay(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    overflow: ir::Value,
    block: u32,
    instruction: u32,
    retired_prefix: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let replay = builder.create_block();
    let success = builder.create_block();
    builder.set_cold_block(replay);
    builder.ins().brif(overflow, replay, &[], success, &[]);

    builder.switch_to_block(replay);
    let retired = emit_retired_with_prefix(builder, values, retired_prefix);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit_with_locals(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_REPLAY,
            block,
            instruction,
            result: NativeValue {
                bits: zero,
                tag: zero,
            },
        },
        locals,
        stack,
    )?;

    builder.switch_to_block(success);
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
    builder.set_cold_block(fault);
    builder.ins().brif(faulted, fault, &[], success, &[]);
    builder.switch_to_block(fault);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
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
    emission: LoadFieldEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let LoadFieldEmission {
        field,
        receiver_class,
        contract,
        allow_pending,
        exit,
    } = emission;
    let scalar_sites = values
        .plan
        .scalar_instances
        .iter()
        .enumerate()
        .filter_map(|(site, instance)| {
            (instance.class == receiver_class && field < instance.field_count).then_some(site)
        })
        .collect::<Vec<_>>();
    if !scalar_sites.is_empty() {
        let fallback = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I64);
        let mut test = None;
        for site in scalar_sites {
            if let Some(test) = test {
                builder.switch_to_block(test);
            }
            let scalar = values
                .scalar_instances
                .get(site)
                .ok_or(CompileError::Backend)?;
            let matched = builder
                .ins()
                .icmp_imm(IntCC::Equal, reference, scalar.token as i64);
            let hit = builder.create_block();
            let miss = builder.create_block();
            builder.ins().brif(matched, hit, &[], miss, &[]);

            builder.switch_to_block(hit);
            let value = scalar
                .fields
                .get(field as usize)
                .ok_or(CompileError::Backend)?;
            let bits = builder.use_var(value.bits);
            let tag = builder.use_var(value.tag);
            builder.ins().jump(done, &[bits.into(), tag.into()]);
            test = Some(miss);
        }
        builder.switch_to_block(test.ok_or(CompileError::Backend)?);
        builder.ins().jump(fallback, &[]);

        builder.switch_to_block(fallback);
        let value = emit_regular_load_field(
            builder,
            values,
            reference,
            field,
            receiver_class,
            contract,
            allow_pending,
            exit,
        )?;
        builder
            .ins()
            .jump(done, &[value.bits.into(), value.tag.into()]);

        builder.switch_to_block(done);
        return Ok(NativeValue {
            bits: builder.block_params(done)[0],
            tag: builder.block_params(done)[1],
        });
    }
    emit_regular_load_field(
        builder,
        values,
        reference,
        field,
        receiver_class,
        contract,
        allow_pending,
        exit,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_regular_load_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    field: u32,
    receiver_class: u32,
    contract: ValueContract,
    allow_pending: bool,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let value = if allow_pending {
        let storage = emit_instance_storage(
            builder,
            values,
            reference,
            Some(receiver_class),
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_instance_storage_field(
            builder,
            values,
            storage,
            field,
            exit.point,
            exit.fault_stack,
        )?
    } else {
        let (entry, _) = emit_instance_entry(
            builder,
            values,
            reference,
            receiver_class,
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
        emit_array_element(
            builder,
            values,
            entry,
            JIT_INSTANCE_FIELDS_OFFSET,
            field_index,
            exit.point,
            exit.fault_stack,
        )?
    };
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
    emit_loaded_value(
        builder,
        values,
        value,
        contract,
        exit.point,
        exit.deopt_stack,
    )
}

fn emit_load_capture(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    index: u32,
    result: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    let capture_data = load_value(
        builder,
        values.pointer_type,
        frame,
        mem::offset_of!(RawNativeFrame, capture_data),
    )?;
    let capture_len = load_value(
        builder,
        values.pointer_type,
        frame,
        mem::offset_of!(RawNativeFrame, capture_len),
    )?;
    let index = builder.ins().iconst(values.pointer_type, i64::from(index));
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, capture_len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let byte_offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let value = builder.ins().iadd(capture_data, byte_offset);
    emit_loaded_value(builder, values, value, result, exit.point, exit.deopt_stack)
}

fn emit_store_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    stored: NativeValue,
    allow_pending: bool,
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
    let address = if allow_pending {
        let storage = emit_instance_storage(
            builder,
            values,
            reference,
            Some(receiver_class),
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_mutable_flag_guard(builder, values, storage.frozen, exit)?;
        emit_instance_storage_field(
            builder,
            values,
            storage,
            field,
            exit.point,
            exit.fault_stack,
        )?
    } else {
        let (entry, _) = emit_instance_entry(
            builder,
            values,
            reference,
            receiver_class,
            exit.point,
            ObjectGuard::Fault(exit.fault_stack),
            ObjectGuard::Replay(exit.deopt_stack),
        )?;
        emit_mutable_guard(builder, values, entry, exit)?;
        let field_index = builder.ins().iconst(values.pointer_type, i64::from(field));
        emit_array_element(
            builder,
            values,
            entry,
            JIT_INSTANCE_FIELDS_OFFSET,
            field_index,
            exit.point,
            exit.fault_stack,
        )?
    };
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
    allow_pending: bool,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let entry = if allow_pending {
        emit_instance_storage(
            builder,
            values,
            reference,
            Some(class),
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?
        .frozen
    } else {
        emit_instance_entry(
            builder,
            values,
            reference,
            class,
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?
        .0
    };
    let frozen = builder.ins().iconst(types::I8, 1);
    if allow_pending {
        store_i8_value(builder, entry, 0, frozen)
    } else {
        store_i8_value(builder, entry, JIT_ENTRY_FROZEN_OFFSET, frozen)
    }
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

fn emit_list_pop(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    emission: ListOptionEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_LIST,
        emission.exit.point,
        ObjectGuard::Fault(emission.exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, emission.exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let present = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    let empty = builder.ins().icmp_imm(IntCC::Equal, len, 0);
    builder.ins().brif(empty, missing, &[], present, &[]);

    builder.switch_to_block(present);
    let last = builder.ins().iadd_imm(len, -1);
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, last)?;
    let result = emit_loaded_value(
        builder,
        values,
        address,
        emission.result,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    emit_list_epoch_bump(builder, values, entry, emission.exit)?;
    store_list_len(builder, entry, last)?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    emit_list_shrink_charge(builder, values, entry, one)?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(missing);
    let family = emit_option_family(
        builder,
        values,
        emission.function,
        emission.family_type,
        emission.resolve,
        emission.exit.deopt_stack,
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

fn emit_list_insert(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: ListInsertEmission<'_>,
) -> Result<(), CompileError> {
    emit_value_contract(
        builder,
        values,
        emission.stored.bits,
        emission.contract,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    let entry = emit_object_entry(
        builder,
        values,
        emission.reference,
        JIT_OBJECT_LIST,
        emission.exit.point,
        ObjectGuard::Fault(emission.exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, entry, emission.exit)?;
    let negative = builder
        .ins()
        .icmp_imm(IntCC::SignedLessThan, emission.index, 0);
    let native_index = native_size(builder, values, emission.index, emission.exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, native_index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(
        builder,
        values,
        invalid,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    emit_list_epoch_guard(builder, values, entry, emission.exit)?;
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
    let source = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, native_index)?;
    let destination = builder.ins().iadd_imm(source, VALUE_SIZE as i64);
    let moved = builder.ins().isub(len, native_index);
    let moved = builder.ins().imul_imm(moved, VALUE_SIZE as i64);
    builder.call_memmove(values.frontend_config, destination, source, moved);
    emit_store_value(builder, source, emission.stored, emission.contract.kind)?;
    let next_len = builder.ins().iadd_imm(len, 1);
    store_list_len(builder, entry, next_len)?;
    emit_list_epoch_bump(builder, values, entry, emission.exit)?;
    emit_list_growth_charge(builder, values, entry, next_used, used_pointer)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(slow_block);
    let status = emit_list_insert_call(
        builder,
        values,
        emission.reference,
        emission.index,
        emission.stored,
        emission.roots,
    )?;
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        emission.exit.point,
        emission.exit.fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(
        builder,
        values,
        replay,
        emission.exit.point,
        emission.exit.deopt_stack,
    )?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

fn emit_list_remove(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    result: ValueContract,
    swap: bool,
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
    emit_mutable_guard(builder, values, entry, exit)?;
    let index = emit_checked_list_index(builder, values, entry, index, exit)?;
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let address = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, index)?;
    let removed = emit_loaded_value(
        builder,
        values,
        address,
        result,
        exit.point,
        exit.deopt_stack,
    )?;
    emit_list_epoch_bump(builder, values, entry, exit)?;
    let last = builder.ins().iadd_imm(len, -1);
    let source_index = if swap {
        last
    } else {
        builder.ins().iadd_imm(index, 1)
    };
    let source = emit_array_address(builder, values, entry, JIT_LIST_ITEMS_OFFSET, source_index)?;
    let moved = if swap {
        builder.ins().iconst(values.pointer_type, VALUE_SIZE as i64)
    } else {
        let count = builder.ins().isub(last, index);
        builder.ins().imul_imm(count, VALUE_SIZE as i64)
    };
    builder.call_memmove(values.frontend_config, address, source, moved);
    store_list_len(builder, entry, last)?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    emit_list_shrink_charge(builder, values, entry, one)?;
    Ok(removed)
}

fn emit_list_truncate(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    length: ir::Value,
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
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, length, 0);
    emit_interpreter_replay(builder, values, negative, exit.point, exit.deopt_stack)?;
    let length = native_size(builder, values, length, exit)?;
    let current = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let changed = builder.ins().icmp(IntCC::UnsignedLessThan, length, current);
    let update = builder.create_block();
    let done = builder.create_block();
    builder.ins().brif(changed, update, &[], done, &[]);

    builder.switch_to_block(update);
    emit_list_epoch_bump(builder, values, entry, exit)?;
    store_list_len(builder, entry, length)?;
    let removed = builder.ins().isub(current, length);
    emit_list_shrink_charge(builder, values, entry, removed)?;
    builder.ins().jump(done, &[]);

    builder.switch_to_block(done);
    Ok(())
}

fn native_size(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    if values.pointer_type == types::I64 {
        return Ok(value);
    }
    let too_large = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, too_large, exit.point, exit.deopt_stack)?;
    Ok(builder.ins().ireduce(values.pointer_type, value))
}

fn emit_list_epoch_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let epoch = load_value(builder, types::I32, entry, JIT_LIST_EPOCH_OFFSET)?;
    let exhausted = builder
        .ins()
        .icmp_imm(IntCC::Equal, epoch, i64::from(u32::MAX));
    emit_interpreter_replay(builder, values, exhausted, exit.point, exit.deopt_stack)
}

fn emit_list_epoch_bump(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
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

fn store_list_len(
    builder: &mut FunctionBuilder<'_>,
    entry: ir::Value,
    len: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_LEN_OFFSET)
        .map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), len, entry, offset);
    Ok(())
}

fn emit_list_growth_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    next_used: ir::Value,
    used_pointer: ir::Value,
) -> Result<(), CompileError> {
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().iadd_imm(object_bytes, VALUE_SIZE as i64);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    builder
        .ins()
        .store(MemFlags::new(), next_used, used_pointer, 0);
    Ok(())
}

fn emit_list_shrink_charge(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    removed: ir::Value,
) -> Result<(), CompileError> {
    let bytes = builder.ins().imul_imm(removed, VALUE_SIZE as i64);
    let object_bytes = load_value(builder, values.pointer_type, entry, JIT_ENTRY_BYTES_OFFSET)?;
    let object_bytes = builder.ins().isub(object_bytes, bytes);
    let bytes_offset = i32::try_from(JIT_ENTRY_BYTES_OFFSET).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(MemFlags::new(), object_bytes, entry, bytes_offset);
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = builder
        .ins()
        .load(values.pointer_type, MemFlags::new(), used_pointer, 0);
    let used = builder.ins().isub(used, bytes);
    builder.ins().store(MemFlags::new(), used, used_pointer, 0);
    Ok(())
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

fn emit_map_len(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let len = load_value(builder, types::I32, entry, JIT_MAP_LIVE_OFFSET)?;
    Ok(builder.ins().uextend(types::I64, len))
}

fn emit_map_epoch(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_MAP_EPOCH_OFFSET)?;
    let unobserved = builder.ins().icmp_imm(IntCC::Equal, epoch, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let observed = builder.ins().select(unobserved, one, epoch);
    store_i32_value(builder, entry, JIT_MAP_EPOCH_OFFSET, observed)?;
    Ok(builder.ins().uextend(types::I64, observed))
}

fn emit_map_iter_len(
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
        JIT_OBJECT_MAP,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let epoch = load_value(builder, types::I32, entry, JIT_MAP_EPOCH_OFFSET)?;
    let expected_epoch = builder.ins().ireduce(types::I32, expected);
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let changed = builder.ins().icmp(IntCC::NotEqual, epoch, expected_epoch);
    let invalid = builder.ins().bor(negative, changed);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let len = load_value(builder, types::I32, entry, JIT_MAP_LIVE_OFFSET)?;
    Ok(builder.ins().uextend(types::I64, len))
}

fn emit_digest_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    left: ir::Value,
    right: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let left = emit_object_entry(
        builder,
        values,
        left,
        JIT_OBJECT_DIGEST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let right = emit_object_entry(
        builder,
        values,
        right,
        JIT_OBJECT_DIGEST,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    let mut equal = builder.ins().iconst(types::I8, 1);
    for word in 0..4 {
        let offset = JIT_DIGEST_BYTES_OFFSET
            .checked_add(word * mem::size_of::<u64>())
            .ok_or(CompileError::Backend)?;
        let left_word = load_value(builder, types::I64, left, offset)?;
        let right_word = load_value(builder, types::I64, right, offset)?;
        let word_equal = builder.ins().icmp(IntCC::Equal, left_word, right_word);
        equal = builder.ins().band(equal, word_equal);
    }
    Ok(equal)
}

fn emit_text_at_byte(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
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
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_BYTE_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;
    let data = load_value(builder, values.pointer_type, entry, JIT_TEXT_DATA_OFFSET)?;
    let address = builder.ins().iadd(data, native_index);
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let prefix = builder.ins().band_imm(first, 0xc0);
    let continuation = builder.ins().icmp_imm(IntCC::Equal, prefix, 0x80);
    emit_interpreter_replay(builder, values, continuation, point, deopt_stack)?;

    emit_utf8_at_address(builder, address)
}

fn emit_text_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
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
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_SCALAR_LEN_OFFSET,
    )?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, scalar_len);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)?;

    let data = load_value(builder, values.pointer_type, entry, JIT_TEXT_DATA_OFFSET)?;
    let byte_len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_BYTE_LEN_OFFSET,
    )?;
    let ascii = builder.ins().icmp(IntCC::Equal, byte_len, scalar_len);
    let ascii_block = builder.create_block();
    let scan = builder.create_block();
    let advance = builder.create_block();
    let found = builder.create_block();
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.ins().brif(
        ascii,
        ascii_block,
        &[],
        scan,
        &[native_index.into(), data.into()],
    );

    builder.switch_to_block(ascii_block);
    let address = builder.ins().iadd(data, native_index);
    builder.ins().jump(found, &[address.into()]);

    builder.switch_to_block(scan);
    let remaining = builder.block_params(scan)[0];
    let address = builder.block_params(scan)[1];
    let at_target = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    builder
        .ins()
        .brif(at_target, found, &[address.into()], advance, &[]);

    builder.switch_to_block(advance);
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let one = builder.ins().iconst(values.pointer_type, 1);
    let two = builder.ins().iconst(values.pointer_type, 2);
    let three = builder.ins().iconst(values.pointer_type, 3);
    let four = builder.ins().iconst(values.pointer_type, 4);
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0x80);
    let is_two = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xe0);
    let is_three = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xf0);
    let non_ascii_width = builder.ins().select(is_three, three, four);
    let multibyte_width = builder.ins().select(is_two, two, non_ascii_width);
    let width = builder.ins().select(is_ascii, one, multibyte_width);
    let next_address = builder.ins().iadd(address, width);
    let next_remaining = builder.ins().iadd_imm(remaining, -1);
    builder
        .ins()
        .jump(scan, &[next_remaining.into(), next_address.into()]);

    builder.switch_to_block(found);
    let address = builder.block_params(found)[0];
    emit_utf8_at_address(builder, address)
}

fn emit_utf8_at_address(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
) -> Result<ir::Value, CompileError> {
    let first = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);

    let ascii = builder.create_block();
    let two = builder.create_block();
    let three = builder.create_block();
    let four = builder.create_block();
    let after_ascii = builder.create_block();
    let after_two = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0x80);
    builder.ins().brif(is_ascii, ascii, &[], after_ascii, &[]);

    builder.switch_to_block(ascii);
    let scalar = builder.ins().uextend(types::I64, first);
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(after_ascii);
    let is_two = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xe0);
    builder.ins().brif(is_two, two, &[], after_two, &[]);

    builder.switch_to_block(two);
    let scalar = emit_utf8_scalar(builder, address, first, 2)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(after_two);
    let is_three = builder.ins().icmp_imm(IntCC::UnsignedLessThan, first, 0xf0);
    builder.ins().brif(is_three, three, &[], four, &[]);

    builder.switch_to_block(three);
    let scalar = emit_utf8_scalar(builder, address, first, 3)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(four);
    let scalar = emit_utf8_scalar(builder, address, first, 4)?;
    builder.ins().jump(done, &[scalar.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_utf8_scalar(
    builder: &mut FunctionBuilder<'_>,
    address: ir::Value,
    first: ir::Value,
    length: u8,
) -> Result<ir::Value, CompileError> {
    let lead_mask = match length {
        2 => 0x1f,
        3 => 0x0f,
        4 => 0x07,
        _ => return Err(CompileError::Backend),
    };
    let first = builder.ins().uextend(types::I64, first);
    let mut scalar = builder.ins().band_imm(first, lead_mask);
    for offset in 1..length {
        let byte = builder
            .ins()
            .load(types::I8, MemFlags::trusted(), address, i32::from(offset));
        let byte = builder.ins().uextend(types::I64, byte);
        let byte = builder.ins().band_imm(byte, 0x3f);
        scalar = builder.ins().ishl_imm(scalar, 6);
        scalar = builder.ins().bor(scalar, byte);
    }
    Ok(scalar)
}

fn emit_text_is_boundary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
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
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    emit_interpreter_replay(builder, values, negative, point, deopt_stack)?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_TEXT_BYTE_LEN_OFFSET,
    )?;
    let inside = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, native_index, len);
    let inspect = builder.create_block();
    let outside = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(inside, inspect, &[], outside, &[]);

    builder.switch_to_block(inspect);
    let data = load_value(builder, values.pointer_type, entry, JIT_TEXT_DATA_OFFSET)?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let prefix = builder.ins().band_imm(byte, 0xc0);
    let continuation = builder.ins().icmp_imm(IntCC::Equal, prefix, 0x80);
    let boundary = builder.ins().bxor_imm(continuation, 1);
    let boundary = builder.ins().uextend(types::I64, boundary);
    builder.ins().jump(done, &[boundary.into()]);

    builder.switch_to_block(outside);
    let boundary = builder.ins().icmp(IntCC::Equal, native_index, len);
    let boundary = builder.ins().uextend(types::I64, boundary);
    builder.ins().jump(done, &[boundary.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
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

fn emit_string_builder_append_text(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    source: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_entry = emit_text_entry(
        builder,
        values,
        source,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_len = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_BYTE_LEN_OFFSET,
    )?;
    let next_len = builder.ins().iadd(target_len, source_len);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let copy_block = builder.create_block();
    let copied_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let nonempty = builder.ins().icmp_imm(IntCC::NotEqual, source_len, 0);
    builder
        .ins()
        .brif(nonempty, copy_block, &[], copied_block, &[]);

    builder.switch_to_block(copy_block);
    let target_data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(target_data, target_len);
    let source_data = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_DATA_OFFSET,
    )?;
    builder.call_memmove(values.frontend_config, destination, source_data, source_len);
    builder.ins().jump(copied_block, &[]);

    builder.switch_to_block(copied_block);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let target_scalars = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let source_scalars = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_TEXT_SCALAR_LEN_OFFSET,
    )?;
    let next_scalars = builder.ins().iadd(target_scalars, source_scalars);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        next_scalars,
    )?;
    let target_ascii = load_value(
        builder,
        types::I8,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
    )?;
    let source_ascii = builder.ins().icmp(IntCC::Equal, source_len, source_scalars);
    let next_ascii = builder.ins().band(target_ascii, source_ascii);
    store_i8_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
        next_ascii,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, string_builder_append_text),
        [target, source, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_string_builder_append_bool(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let truth = builder.ins().icmp_imm(IntCC::NotEqual, value, 0);
    let true_len = builder.ins().iconst(values.pointer_type, 4);
    let false_len = builder.ins().iconst(values.pointer_type, 5);
    let added = builder.ins().select(truth, true_len, false_len);
    let next_len = builder.ins().iadd(target_len, added);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let true_block = builder.create_block();
    let false_block = builder.create_block();
    let written_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, target_len);
    builder.ins().brif(truth, true_block, &[], false_block, &[]);

    builder.switch_to_block(true_block);
    for (offset, byte) in b"true".iter().copied().enumerate() {
        let value = builder.ins().iconst(types::I8, i64::from(byte));
        store_i8_value(builder, destination, offset, value)?;
    }
    builder.ins().jump(written_block, &[]);

    builder.switch_to_block(false_block);
    for (offset, byte) in b"false".iter().copied().enumerate() {
        let value = builder.ins().iconst(types::I8, i64::from(byte));
        store_i8_value(builder, destination, offset, value)?;
    }
    builder.ins().jump(written_block, &[]);

    builder.switch_to_block(written_block);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd(scalar_len, added);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, string_builder_append_bool),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_string_builder_append_int(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;

    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, value, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let negated = builder.ins().isub(zero_i64, value);
    let magnitude = builder.ins().select(negative, negated, value);
    let count_digits = builder.create_block();
    let count_more = builder.create_block();
    let count_done = builder.create_block();
    builder.append_block_param(count_digits, types::I64);
    builder.append_block_param(count_digits, types::I64);
    builder.append_block_param(count_done, types::I64);
    let one_i64 = builder.ins().iconst(types::I64, 1);
    builder
        .ins()
        .jump(count_digits, &[magnitude.into(), one_i64.into()]);

    builder.switch_to_block(count_digits);
    let remaining = builder.block_params(count_digits)[0];
    let digits = builder.block_params(count_digits)[1];
    let has_more = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, remaining, 10);
    builder
        .ins()
        .brif(has_more, count_more, &[], count_done, &[digits.into()]);

    builder.switch_to_block(count_more);
    let remaining = builder.ins().udiv_imm(remaining, 10);
    let digits = builder.ins().iadd_imm(digits, 1);
    builder
        .ins()
        .jump(count_digits, &[remaining.into(), digits.into()]);

    builder.switch_to_block(count_done);
    let digits = builder.block_params(count_done)[0];
    let sign = builder.ins().uextend(types::I64, negative);
    let added_i64 = builder.ins().iadd(digits, sign);
    let added = if values.pointer_type == types::I64 {
        added_i64
    } else {
        builder.ins().ireduce(values.pointer_type, added_i64)
    };
    let next_len = builder.ins().iadd(target_len, added);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let sign_block = builder.create_block();
    let digits_block = builder.create_block();
    let digit_loop = builder.create_block();
    let written = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(digit_loop, types::I64);
    builder.append_block_param(digit_loop, values.pointer_type);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let cursor = builder.ins().iadd(data, next_len);
    builder
        .ins()
        .brif(negative, sign_block, &[], digits_block, &[]);

    builder.switch_to_block(sign_block);
    let sign_address = builder.ins().iadd(data, target_len);
    let minus = builder.ins().iconst(types::I8, i64::from(b'-'));
    store_i8_value(builder, sign_address, 0, minus)?;
    builder.ins().jump(digits_block, &[]);

    builder.switch_to_block(digits_block);
    builder
        .ins()
        .jump(digit_loop, &[magnitude.into(), cursor.into()]);

    builder.switch_to_block(digit_loop);
    let remaining = builder.block_params(digit_loop)[0];
    let cursor = builder.block_params(digit_loop)[1];
    let quotient = builder.ins().udiv_imm(remaining, 10);
    let digit = builder.ins().urem_imm(remaining, 10);
    let digit = builder.ins().iadd_imm(digit, i64::from(b'0'));
    let digit = builder.ins().ireduce(types::I8, digit);
    let cursor = builder.ins().iadd_imm(cursor, -1);
    store_i8_value(builder, cursor, 0, digit)?;
    let has_more = builder.ins().icmp_imm(IntCC::NotEqual, quotient, 0);
    builder.ins().brif(
        has_more,
        digit_loop,
        &[quotient.into(), cursor.into()],
        written,
        &[],
    );

    builder.switch_to_block(written);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd(scalar_len, added);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, string_builder_append_int),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_string_builder_append_char(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_STRING_BUILDER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_STRING_BUILDER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let above_unicode = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, 0x10ffff);
    let in_surrogate_tail =
        builder
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, value, 0xd800);
    let in_surrogate_head = builder
        .ins()
        .icmp_imm(IntCC::UnsignedLessThanOrEqual, value, 0xdfff);
    let surrogate = builder.ins().band(in_surrogate_tail, in_surrogate_head);
    let invalid = builder.ins().bor(above_unicode, surrogate);
    emit_fault_check(
        builder,
        values,
        invalid,
        EXIT_TYPE_MISMATCH,
        exit.point,
        exit.fault_stack,
    )?;
    let one = builder.ins().iconst(values.pointer_type, 1);
    let two = builder.ins().iconst(values.pointer_type, 2);
    let three = builder.ins().iconst(values.pointer_type, 3);
    let four = builder.ins().iconst(values.pointer_type, 4);
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
    let width = builder.ins().select(over_three, four, medium);
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let next_len = builder.ins().iadd(target_len, width);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let one_block = builder.create_block();
    let after_one = builder.create_block();
    let two_block = builder.create_block();
    let after_two = builder.create_block();
    let three_block = builder.create_block();
    let four_block = builder.create_block();
    let written = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, target_len);
    let is_one = builder.ins().icmp_imm(IntCC::Equal, width, 1);
    builder.ins().brif(is_one, one_block, &[], after_one, &[]);

    builder.switch_to_block(one_block);
    let byte = builder.ins().ireduce(types::I8, value);
    store_i8_value(builder, destination, 0, byte)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(after_one);
    let is_two = builder.ins().icmp_imm(IntCC::Equal, width, 2);
    builder.ins().brif(is_two, two_block, &[], after_two, &[]);

    builder.switch_to_block(two_block);
    let first = builder.ins().ushr_imm(value, 6);
    let first = builder.ins().bor_imm(first, 0xc0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().band_imm(value, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(after_two);
    let is_three = builder.ins().icmp_imm(IntCC::Equal, width, 3);
    builder
        .ins()
        .brif(is_three, three_block, &[], four_block, &[]);

    builder.switch_to_block(three_block);
    let first = builder.ins().ushr_imm(value, 12);
    let first = builder.ins().bor_imm(first, 0xe0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().ushr_imm(value, 6);
    let second = builder.ins().band_imm(second, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    let third = builder.ins().band_imm(value, 0x3f);
    let third = builder.ins().bor_imm(third, 0x80);
    let third = builder.ins().ireduce(types::I8, third);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    store_i8_value(builder, destination, 2, third)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(four_block);
    let first = builder.ins().ushr_imm(value, 18);
    let first = builder.ins().bor_imm(first, 0xf0);
    let first = builder.ins().ireduce(types::I8, first);
    let second = builder.ins().ushr_imm(value, 12);
    let second = builder.ins().band_imm(second, 0x3f);
    let second = builder.ins().bor_imm(second, 0x80);
    let second = builder.ins().ireduce(types::I8, second);
    let third = builder.ins().ushr_imm(value, 6);
    let third = builder.ins().band_imm(third, 0x3f);
    let third = builder.ins().bor_imm(third, 0x80);
    let third = builder.ins().ireduce(types::I8, third);
    let fourth = builder.ins().band_imm(value, 0x3f);
    let fourth = builder.ins().bor_imm(fourth, 0x80);
    let fourth = builder.ins().ireduce(types::I8, fourth);
    store_i8_value(builder, destination, 0, first)?;
    store_i8_value(builder, destination, 1, second)?;
    store_i8_value(builder, destination, 2, third)?;
    store_i8_value(builder, destination, 3, fourth)?;
    builder.ins().jump(written, &[]);

    builder.switch_to_block(written);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        next_len,
    )?;
    let scalar_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
    )?;
    let scalar_len = builder.ins().iadd_imm(scalar_len, 1);
    store_native_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_SCALAR_LEN_OFFSET,
        scalar_len,
    )?;
    let target_ascii = load_value(
        builder,
        types::I8,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
    )?;
    let is_ascii = builder.ins().icmp_imm(IntCC::UnsignedLessThan, value, 0x80);
    let ascii = builder.ins().band(target_ascii, is_ascii);
    store_i8_value(
        builder,
        target_entry,
        JIT_STRING_BUILDER_ASCII_OFFSET,
        ascii,
    )?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, string_builder_append_char),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_byte_buffer_append(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    value: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, value, 0);
    let too_large = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, value, i64::from(u8::MAX));
    let invalid = builder.ins().bor(negative, too_large);
    emit_fault_check(
        builder,
        values,
        invalid,
        EXIT_INTEGER_OVERFLOW,
        exit.point,
        exit.fault_stack,
    )?;
    let len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let fast = builder.ins().icmp(IntCC::UnsignedLessThan, len, capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(data, len);
    let byte = builder.ins().ireduce(types::I8, value);
    builder
        .ins()
        .store(MemFlags::trusted(), byte, destination, 0);
    let next_len = builder.ins().iadd_imm(len, 1);
    store_native_value(builder, target_entry, JIT_BYTE_BUFFER_LEN_OFFSET, next_len)?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, byte_buffer_append),
        [target, value, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_byte_buffer_extend(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    source: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_entry = emit_object_entry(
        builder,
        values,
        source,
        JIT_OBJECT_BYTES,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    let target_len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, capacity, target_len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let source_len = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_BYTES_LEN_OFFSET,
    )?;
    let next_len = builder.ins().iadd(target_len, source_len);
    let overflow = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next_len, target_len);
    let within_capacity = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_len, capacity);
    let no_overflow = builder.ins().bxor_imm(overflow, 1);
    let fast = builder.ins().band(no_overflow, within_capacity);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let copy_block = builder.create_block();
    let copied_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    let nonempty = builder.ins().icmp_imm(IntCC::NotEqual, source_len, 0);
    builder
        .ins()
        .brif(nonempty, copy_block, &[], copied_block, &[]);

    builder.switch_to_block(copy_block);
    let target_data = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let destination = builder.ins().iadd(target_data, target_len);
    let source_data = load_value(
        builder,
        values.pointer_type,
        source_entry,
        JIT_BYTES_DATA_OFFSET,
    )?;
    builder.call_memmove(values.frontend_config, destination, source_data, source_len);
    builder.ins().jump(copied_block, &[]);

    builder.switch_to_block(copied_block);
    store_native_value(builder, target_entry, JIT_BYTE_BUFFER_LEN_OFFSET, next_len)?;
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, byte_buffer_extend),
        [target, source, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_byte_buffer_reserve(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    target: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let runtime_additional = additional;
    let target_entry = emit_object_entry(
        builder,
        values,
        target,
        JIT_OBJECT_BYTE_BUFFER,
        exit.point,
        ObjectGuard::Fault(exit.fault_stack),
    )?;
    emit_mutable_guard(builder, values, target_entry, exit)?;
    emit_active_guard(
        builder,
        values,
        target_entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        exit.point,
        exit.deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, additional, 0);
    emit_fault_check(
        builder,
        values,
        negative,
        EXIT_INTEGER_OVERFLOW,
        exit.point,
        exit.fault_stack,
    )?;
    let additional = if values.pointer_type == types::I64 {
        additional
    } else {
        let too_large =
            builder
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, additional, i64::from(u32::MAX));
        emit_fault_check(
            builder,
            values,
            too_large,
            EXIT_INTEGER_OVERFLOW,
            exit.point,
            exit.fault_stack,
        )?;
        builder.ins().ireduce(values.pointer_type, additional)
    };
    let len = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let capacity = load_value(
        builder,
        values.pointer_type,
        target_entry,
        JIT_BYTE_BUFFER_CAPACITY_OFFSET,
    )?;
    let invalid_capacity = builder.ins().icmp(IntCC::UnsignedLessThan, capacity, len);
    emit_interpreter_replay(
        builder,
        values,
        invalid_capacity,
        exit.point,
        exit.deopt_stack,
    )?;
    let spare = builder.ins().isub(capacity, len);
    let fast = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, additional, spare);
    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(fast_block);
    builder.ins().jump(done, &[target.into()]);

    builder.switch_to_block(slow_block);
    let zero = builder.ins().iconst(types::I64, 0);
    let result = emit_heap_operation(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, byte_buffer_reserve),
        [target, runtime_additional, zero],
        roots,
        exit,
    )?;
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_builder_len(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    object_tag: u32,
    offsets: (usize, usize),
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let (active_offset, length_offset) = offsets;
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        object_tag,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    emit_active_guard(builder, values, entry, active_offset, point, deopt_stack)?;
    let length = load_value(builder, values.pointer_type, entry, length_offset)?;
    Ok(if values.pointer_type == types::I64 {
        length
    } else {
        builder.ins().uextend(types::I64, length)
    })
}

fn emit_builder_clear(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    string_builder: bool,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let (object_tag, active_offset, length_offset) = if string_builder {
        (
            JIT_OBJECT_STRING_BUILDER,
            JIT_STRING_BUILDER_ACTIVE_OFFSET,
            JIT_STRING_BUILDER_BYTE_LEN_OFFSET,
        )
    } else {
        (
            JIT_OBJECT_BYTE_BUFFER,
            JIT_BYTE_BUFFER_ACTIVE_OFFSET,
            JIT_BYTE_BUFFER_LEN_OFFSET,
        )
    };
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        object_tag,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, entry, exit)?;
    emit_active_guard(
        builder,
        values,
        entry,
        active_offset,
        exit.point,
        exit.deopt_stack,
    )?;
    let zero = builder.ins().iconst(values.pointer_type, 0);
    store_native_value(builder, entry, length_offset, zero)?;
    if string_builder {
        store_native_value(builder, entry, JIT_STRING_BUILDER_SCALAR_LEN_OFFSET, zero)?;
        let ascii = builder.ins().iconst(types::I8, 1);
        store_i8_value(builder, entry, JIT_STRING_BUILDER_ASCII_OFFSET, ascii)?;
    }
    Ok(())
}

fn emit_byte_buffer_at(
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
        JIT_OBJECT_BYTE_BUFFER,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
    emit_active_guard(
        builder,
        values,
        entry,
        JIT_BYTE_BUFFER_ACTIVE_OFFSET,
        point,
        deopt_stack,
    )?;
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let length = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_LEN_OFFSET,
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, native_index, length);
    let missing = builder.ins().bor(negative, outside);
    let load = builder.create_block();
    let absent = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.ins().brif(missing, absent, &[], load, &[]);

    builder.switch_to_block(load);
    let data = load_value(
        builder,
        values.pointer_type,
        entry,
        JIT_BYTE_BUFFER_DATA_OFFSET,
    )?;
    let address = builder.ins().iadd(data, native_index);
    let byte = builder
        .ins()
        .load(types::I8, MemFlags::trusted(), address, 0);
    let byte = builder.ins().uextend(types::I64, byte);
    builder.ins().jump(done, &[byte.into()]);

    builder.switch_to_block(absent);
    let missing = builder.ins().iconst(types::I64, -1);
    builder.ins().jump(done, &[missing.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_active_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    offset: usize,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let active = load_heap_value(builder, types::I8, entry, offset)?;
    let inactive = builder.ins().icmp_imm(IntCC::Equal, active, 0);
    emit_interpreter_replay(builder, values, inactive, point, deopt_stack)
}

fn emit_mutable_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let frozen = builder.ins().iadd_imm(
        entry,
        i64::try_from(JIT_ENTRY_FROZEN_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_mutable_flag_guard(builder, values, frozen, exit)
}

fn emit_mutable_flag_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frozen: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<(), CompileError> {
    let frozen = load_heap_value(builder, types::I8, frozen, 0)?;
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
    let len = load_heap_value(
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
    let len = load_heap_value(
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
    let cached = array_offset == JIT_LIST_ITEMS_OFFSET
        && values.heap_translations.borrow().use_cached_list_data;
    let data = if cached {
        local_heap_cache(values, entry)
            .and_then(|cache| cache.list_data)
            .map(|data| builder.use_var(data))
    } else {
        None
    };
    let data = if let Some(data) = data {
        data
    } else if matches!(
        array_offset,
        JIT_INSTANCE_FIELDS_OFFSET | JIT_TUPLE_ITEMS_OFFSET
    ) {
        load_immutable_heap_value(
            builder,
            values.pointer_type,
            entry,
            array_offset + VALUE_ARRAY_DATA_OFFSET,
        )?
    } else {
        load_heap_value(
            builder,
            values.pointer_type,
            entry,
            array_offset + VALUE_ARRAY_DATA_OFFSET,
        )?
    };
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
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
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
    emit_scalar_tag_guard(
        builder,
        values,
        value.tag,
        contract.kind,
        point,
        deopt_stack,
    )?;
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
    emit_scalar_tag_guard(builder, values, tag, contract.kind, point, deopt_stack)?;
    let payload = emit_value_payload(
        builder,
        values,
        address,
        tag,
        contract.kind,
        point,
        deopt_stack,
    )?;
    emit_value_contract(builder, values, payload, contract, point, deopt_stack)?;
    Ok(NativeValue { bits: payload, tag })
}

fn emit_scalar_tag_guard(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    tag: ir::Value,
    kind: ScalarKind,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let invalid = if matches!(kind, ScalarKind::Callback(_)) {
        let closure = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, ValueTag::Obj as u64 as i64);
        let callback = builder
            .ins()
            .icmp_imm(IntCC::Equal, tag, ValueTag::Callback as u64 as i64);
        let valid = builder.ins().bor(closure, callback);
        builder.ins().bxor_imm(valid, 1)
    } else if let Some(expected_tag) = value_tag(kind) {
        builder
            .ins()
            .icmp_imm(IntCC::NotEqual, tag, expected_tag as u64 as i64)
    } else {
        return Ok(());
    };
    emit_interpreter_replay(builder, values, invalid, point, deopt_stack)
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
    if let ObjectContract::Instance(class) = object {
        emit_instance_entry(
            builder,
            values,
            payload,
            class,
            point,
            ObjectGuard::Replay(deopt_stack),
            ObjectGuard::Replay(deopt_stack),
        )?;
        return Ok(());
    }
    let tag = match object {
        ObjectContract::Str => JIT_OBJECT_STR,
        ObjectContract::Text => unreachable!(),
        ObjectContract::Instance(_) => unreachable!(),
        ObjectContract::List => JIT_OBJECT_LIST,
        ObjectContract::Map => JIT_OBJECT_MAP,
        ObjectContract::Tuple => JIT_OBJECT_TUPLE,
        ObjectContract::Closure => JIT_OBJECT_CLOSURE,
        ObjectContract::Bytes => JIT_OBJECT_BYTES,
        ObjectContract::Digest => JIT_OBJECT_DIGEST,
        ObjectContract::StringBuilder => JIT_OBJECT_STRING_BUILDER,
        ObjectContract::ByteBuffer => JIT_OBJECT_BYTE_BUFFER,
    };
    emit_object_entry(
        builder,
        values,
        payload,
        tag,
        point,
        ObjectGuard::Replay(deopt_stack),
    )?;
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
    store_heap_value(builder, address, VALUE_TAG_OFFSET, tag)?;
    store_heap_value(builder, address, VALUE_PAYLOAD_OFFSET, value.bits)
}

fn emit_object_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    object_tag: u32,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let local_slot = values.heap_translations.borrow().local(reference);
    let local_cache = local_slot
        .and_then(|slot| values.local_heap_caches.get(slot))
        .copied()
        .flatten();
    let preloaded = object_tag == JIT_OBJECT_LIST
        && values.heap_translations.borrow().use_cached_list_data
        && local_cache.is_some_and(|cache| cache.preloaded_list_data);
    if preloaded {
        let cache = local_cache.ok_or(CompileError::Backend)?;
        let entry = builder.use_var(cache.entry);
        if let Some(slot) = local_slot {
            values
                .heap_translations
                .borrow_mut()
                .record_local(entry, slot);
        }
        return Ok(entry);
    }
    let expected = i64::from(object_tag) + 1;
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached_kind = builder.use_var(cache.object_kind);
        let proven = builder.ins().icmp_imm(IntCC::Equal, cached_kind, expected);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(miss);
        let entry = emit_heap_entry(builder, values, reference, point, guard)?;
        let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
        let wrong_kind = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(object_tag));
        emit_object_guard(builder, values, wrong_kind, point, guard)?;
        if object_tag == JIT_OBJECT_LIST {
            if let Some(list_data) = cache.list_data {
                let data = load_immutable_heap_value(
                    builder,
                    values.pointer_type,
                    entry,
                    JIT_LIST_ITEMS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
                )?;
                builder.def_var(list_data, data);
            }
        }
        let expected = builder.ins().iconst(types::I64, expected);
        builder.def_var(cache.object_kind, expected);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    } else {
        let entry = emit_heap_entry(builder, values, reference, point, guard)?;
        let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
        let wrong_kind = builder
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(object_tag));
        emit_object_guard(builder, values, wrong_kind, point, guard)?;
        entry
    };
    if let Some(slot) = local_slot {
        values
            .heap_translations
            .borrow_mut()
            .record_local(entry, slot);
    }
    Ok(entry)
}

fn emit_instance_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let local_cache = local_heap_cache(values, reference);
    let expected = i64::from(class) + 1;
    let (entry, actual) = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        builder.append_block_param(done, types::I32);
        let cached_class = builder.use_var(cache.class);
        let proven = builder.ins().icmp_imm(IntCC::Equal, cached_class, expected);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        let actual = builder.use_var(cache.actual_class);
        builder.ins().jump(done, &[entry.into(), actual.into()]);

        builder.switch_to_block(miss);
        let (entry, actual) = emit_instance_entry_miss(
            builder,
            values,
            reference,
            class,
            point,
            object_guard,
            class_guard,
        )?;
        let expected = builder.ins().iconst(types::I64, expected);
        builder.def_var(cache.class, expected);
        builder.def_var(cache.actual_class, actual);
        builder.ins().jump(done, &[entry.into(), actual.into()]);

        builder.switch_to_block(done);
        (builder.block_params(done)[0], builder.block_params(done)[1])
    } else {
        emit_instance_entry_miss(
            builder,
            values,
            reference,
            class,
            point,
            object_guard,
            class_guard,
        )?
    };
    Ok((entry, actual))
}

#[derive(Clone, Copy)]
struct NativeInstanceStorage {
    frozen: ir::Value,
    actual_class: ir::Value,
    data: ir::Value,
    len: ir::Value,
}

#[derive(Clone, Copy)]
struct PendingRecordLookup {
    record: ir::Value,
    record_index: ir::Value,
}

fn emit_pending_record_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    miss: ir::Block,
) -> Result<PendingRecordLookup, CompileError> {
    let check_record = builder.create_block();
    let use_record = builder.create_block();
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    let marker = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        slot,
        i64::from(PENDING_INSTANCE_SLOT_BASE),
    );
    let pending = builder.ins().band(outside, marker);
    builder.ins().brif(pending, check_record, &[], miss, &[]);

    builder.switch_to_block(check_record);
    let maximum = builder.ins().iconst(types::I32, i64::from(u32::MAX));
    let record_index = builder.ins().isub(maximum, slot);
    let record_outside = builder.ins().icmp_imm(
        IntCC::UnsignedGreaterThanOrEqual,
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_COUNT).map_err(|_| CompileError::Backend)?,
    );
    let record_index_pointer = builder.ins().uextend(values.pointer_type, record_index);
    let records = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let record_offset = builder.ins().imul_imm(
        record_index_pointer,
        i64::try_from(mem::size_of::<RawVirtualInstance>()).map_err(|_| CompileError::Backend)?,
    );
    let record = builder.ins().iadd(records, record_offset);
    builder
        .ins()
        .brif(record_outside, miss, &[], use_record, &[]);

    builder.switch_to_block(use_record);
    let active = load_value(
        builder,
        types::I32,
        record,
        mem::offset_of!(RawVirtualInstance, active),
    )?;
    let record_bits = load_value(
        builder,
        types::I64,
        record,
        mem::offset_of!(RawVirtualInstance, object_bits),
    )?;
    let inactive = builder.ins().icmp_imm(IntCC::Equal, active, 0);
    let wrong_record = builder.ins().icmp(IntCC::NotEqual, record_bits, reference);
    let invalid_record = builder.ins().bor(inactive, wrong_record);
    let valid_record = builder.create_block();
    builder
        .ins()
        .brif(invalid_record, miss, &[], valid_record, &[]);
    builder.switch_to_block(valid_record);
    Ok(PendingRecordLookup {
        record,
        record_index,
    })
}

fn emit_retain_pending_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
) -> Result<(), CompileError> {
    let done = builder.create_block();
    let lookup = emit_pending_record_lookup(builder, values, reference, done)?;
    let references = load_value(
        builder,
        types::I32,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, references),
    )?;
    let next = builder.ins().iadd_imm(references, 1);
    store_i32_value(
        builder,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, references),
        next,
    )?;
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
    Ok(())
}

fn emit_release_pending_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
) -> Result<(), CompileError> {
    let done = builder.create_block();
    let lookup = emit_pending_record_lookup(builder, values, reference, done)?;
    let references = load_value(
        builder,
        types::I32,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, references),
    )?;
    let next = builder.ins().iadd_imm(references, -1);
    store_i32_value(
        builder,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, references),
        next,
    )?;
    let retained = builder.ins().icmp_imm(IntCC::NotEqual, next, 0);
    let release = builder.create_block();
    builder.ins().brif(retained, done, &[], release, &[]);

    builder.switch_to_block(release);
    let field_count = load_value(
        builder,
        types::I32,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, field_count),
    )?;
    let field_count = builder.ins().uextend(values.pointer_type, field_count);
    let bytes = builder.ins().imul_imm(
        field_count,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let bytes = builder.ins().iadd_imm(
        bytes,
        i64::try_from(MIN_OBJECT_COST).map_err(|_| CompileError::Backend)?,
    );
    let dead = builder.ins().iconst(types::I32, 0);
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().isub(used, bytes);
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, -1);
    store_heap_value(builder, live_pointer, 0, next_live)?;

    store_i32_value(
        builder,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, active),
        dead,
    )?;
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let one = builder.ins().iconst(types::I64, 1);
    let record_index = builder.ins().uextend(types::I64, lookup.record_index);
    let bit = builder.ins().ishl(one, record_index);
    let available = builder.ins().bor(available, bit);
    let available_offset = i32::try_from(mem::offset_of!(RawNativeActivation, virtual_available))
        .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        available,
        values.activation_pointer,
        available_offset,
    );
    let releases = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, pending_instance_releases),
    )?;
    let releases = builder.ins().iadd_imm(releases, 1);
    let releases_offset = i32::try_from(mem::offset_of!(
        RawNativeActivation,
        pending_instance_releases
    ))
    .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        releases,
        values.activation_pointer,
        releases_offset,
    );
    builder.ins().jump(done, &[]);
    builder.switch_to_block(done);
    Ok(())
}

fn emit_instance_storage(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: Option<u32>,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<NativeInstanceStorage, CompileError> {
    let canonical = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);

    let lookup = emit_pending_record_lookup(builder, values, reference, canonical)?;
    let actual = load_value(
        builder,
        types::I32,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, class),
    )?;
    let len = load_value(
        builder,
        types::I32,
        lookup.record,
        mem::offset_of!(RawVirtualInstance, field_count),
    )?;
    let len = builder.ins().uextend(values.pointer_type, len);
    let fields = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    let record_index = builder
        .ins()
        .uextend(values.pointer_type, lookup.record_index);
    let data_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_FIELDS.saturating_mul(VALUE_SIZE))
            .map_err(|_| CompileError::Backend)?,
    );
    let data = builder.ins().iadd(fields, data_offset);
    let frozen = builder.ins().iadd_imm(
        lookup.record,
        i64::try_from(mem::offset_of!(RawVirtualInstance, frozen))
            .map_err(|_| CompileError::Backend)?,
    );
    builder.ins().jump(
        done,
        &[actual.into(), data.into(), len.into(), frozen.into()],
    );

    builder.switch_to_block(canonical);
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        point,
        object_guard,
    )?;
    let actual = load_heap_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let data = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
    )?;
    let len = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
    )?;
    let frozen = builder.ins().iadd_imm(
        entry,
        i64::try_from(JIT_ENTRY_FROZEN_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    builder.ins().jump(
        done,
        &[actual.into(), data.into(), len.into(), frozen.into()],
    );

    builder.switch_to_block(done);
    let actual_class = builder.block_params(done)[0];
    if let Some(class) = class {
        let matches = emit_class_matches(builder, values, actual_class, class)?;
        let mismatch = builder.ins().bxor_imm(matches, 1);
        emit_object_guard(builder, values, mismatch, point, class_guard)?;
    }
    Ok(NativeInstanceStorage {
        frozen: builder.block_params(done)[3],
        actual_class,
        data: builder.block_params(done)[1],
        len: builder.block_params(done)[2],
    })
}

fn emit_instance_storage_field(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    storage: NativeInstanceStorage,
    field: u32,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let index = builder.ins().iconst(values.pointer_type, i64::from(field));
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, storage.len);
    emit_fault_check(
        builder,
        values,
        outside,
        EXIT_TYPE_MISMATCH,
        point,
        fault_stack,
    )?;
    let offset = builder.ins().imul_imm(
        index,
        i64::try_from(VALUE_SIZE).map_err(|_| CompileError::Backend)?,
    );
    Ok(builder.ins().iadd(storage.data, offset))
}

fn emit_instance_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    class: u32,
    point: FaultPoint,
    object_guard: ObjectGuard<'_>,
    class_guard: ObjectGuard<'_>,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_INSTANCE,
        point,
        object_guard,
    )?;
    let actual = load_heap_value(builder, types::I32, entry, JIT_INSTANCE_CLASS_OFFSET)?;
    let matches = emit_class_matches(builder, values, actual, class)?;
    let mismatch = builder.ins().bxor_imm(matches, 1);
    emit_object_guard(builder, values, mismatch, point, class_guard)?;
    Ok((entry, actual))
}

fn emit_text_entry(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    const TEXT_PROOF: i64 = (u32::MAX as i64) + 2;
    let local_cache = local_heap_cache(values, reference);
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached_kind = builder.use_var(cache.object_kind);
        let proven = builder
            .ins()
            .icmp_imm(IntCC::Equal, cached_kind, TEXT_PROOF);
        builder.ins().brif(proven, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        let entry = builder.use_var(cache.entry);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(miss);
        let entry = emit_text_entry_miss(builder, values, reference, point, guard)?;
        let proof = builder.ins().iconst(types::I64, TEXT_PROOF);
        builder.def_var(cache.object_kind, proof);
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    } else {
        emit_text_entry_miss(builder, values, reference, point, guard)?
    };
    Ok(entry)
}

fn emit_text_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let entry = emit_heap_entry(builder, values, reference, point, guard)?;
    let kind = load_heap_value(builder, types::I32, entry, JIT_ENTRY_OBJECT_TAG_OFFSET)?;
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
    let local_cache = local_heap_cache(values, reference);
    let entry = if let Some(cache) = local_cache {
        let hit = builder.create_block();
        let miss = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, values.pointer_type);
        let cached = builder.use_var(cache.entry);
        let present = builder.ins().icmp_imm(IntCC::NotEqual, cached, 0);
        builder.ins().brif(present, hit, &[], miss, &[]);

        builder.switch_to_block(hit);
        builder.ins().jump(done, &[cached.into()]);

        builder.switch_to_block(miss);
        let entry = emit_heap_entry_miss(builder, values, reference, point, guard)?;
        builder.ins().jump(done, &[entry.into()]);

        builder.switch_to_block(done);
        let entry = builder.block_params(done)[0];
        builder.def_var(cache.entry, entry);
        entry
    } else {
        emit_heap_entry_miss(builder, values, reference, point, guard)?
    };
    Ok(entry)
}

fn emit_heap_entry_miss(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    point: FaultPoint,
    guard: ObjectGuard<'_>,
) -> Result<ir::Value, CompileError> {
    let slot = builder.ins().ireduce(types::I32, reference);
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let slot_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_slot_count),
    )?;
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, slot_index, slot_count);
    emit_object_guard(builder, values, outside, point, guard)?;

    let pages = load_vmctx_value(
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
        .load(values.pointer_type, table_mem_flags(), page_address, 0);
    let within_page = builder.ins().band_imm(slot_index, i64::from(JIT_PAGE_MASK));
    let entry_offset = builder.ins().imul_imm(
        within_page,
        i64::try_from(JIT_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(page, entry_offset);
    let expected_generation = builder.ins().ushr_imm(reference, 32);
    let expected_generation = builder.ins().ireduce(types::I32, expected_generation);
    let generation = load_heap_value(builder, types::I32, entry, JIT_ENTRY_GENERATION_OFFSET)?;
    let live = load_heap_value(builder, types::I32, entry, JIT_ENTRY_LIVE_OFFSET)?;
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

fn local_heap_cache(values: NativeValues<'_>, reference: ir::Value) -> Option<LocalHeapCache> {
    let slot = values.heap_translations.borrow().local(reference)?;
    values.local_heap_caches.get(slot).copied().flatten()
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
        ObjectGuard::Branch(target) => {
            let success = builder.create_block();
            builder.ins().brif(invalid, target, &[], success, &[]);
            builder.switch_to_block(success);
            Ok(())
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
        ScalarKind::Callback(_) => return None,
        ScalarKind::Operation => ValueTag::Op,
    })
}

fn emit_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    tag: ir::Value,
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
        ScalarKind::Int | ScalarKind::Object(_) | ScalarKind::Callback(_) => {
            load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?
        }
        ScalarKind::Tagged(_) => {
            emit_tagged_value_payload(builder, values, value, tag, point, deopt_stack)?
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

fn emit_tagged_value_payload(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    value: ir::Value,
    tag: ir::Value,
    point: FaultPoint,
    deopt_stack: &[NativeValue],
) -> Result<ir::Value, CompileError> {
    let unit = builder.create_block();
    let boolean = builder.create_block();
    let narrow = builder.create_block();
    let full = builder.create_block();
    let float = builder.create_block();
    let invalid = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);

    let mut dispatch = Switch::new();
    dispatch.set_entry(ValueTag::Unit as u128, unit);
    dispatch.set_entry(ValueTag::Bool as u128, boolean);
    dispatch.set_entry(ValueTag::Char as u128, narrow);
    dispatch.set_entry(ValueTag::Op as u128, narrow);
    dispatch.set_entry(ValueTag::Int as u128, full);
    dispatch.set_entry(ValueTag::Obj as u128, full);
    dispatch.set_entry(ValueTag::Callback as u128, full);
    dispatch.set_entry(ValueTag::EmptyCase as u128, full);
    dispatch.set_entry(ValueTag::Float as u128, float);
    dispatch.emit(builder, tag, invalid);

    builder.switch_to_block(unit);
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(boolean);
    let payload = load_value(builder, types::I8, value, VALUE_PAYLOAD_OFFSET)?;
    let payload = builder.ins().uextend(types::I64, payload);
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(narrow);
    let payload = load_value(builder, types::I32, value, VALUE_PAYLOAD_OFFSET)?;
    let payload = builder.ins().uextend(types::I64, payload);
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(full);
    let payload = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(float);
    let payload = load_value(builder, types::I64, value, VALUE_PAYLOAD_OFFSET)?;
    emit_canonical_float_guard(builder, values, payload, point, deopt_stack)?;
    builder.ins().jump(done, &[payload.into()]);

    builder.switch_to_block(invalid);
    let reject = builder.ins().iconst(types::I8, 1);
    emit_interpreter_replay(builder, values, reject, point, deopt_stack)?;
    let zero = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
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
        if is_root_kind(kind) {
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
        if is_root_kind(kind) {
            roots.push(NativeRoot {
                bits: builder.use_var(variable),
                tag: emit_slot_tag(builder, values.local_tags[slot], kind)?,
                state: Some(emit_local_state(builder, values, slot)?),
            });
        }
    }
    extend_stack_roots(&mut roots, stack_kinds, stack)?;
    Ok(roots)
}

fn collect_capture_allocation_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
    capture_count: usize,
) -> Result<(Vec<NativeRoot>, usize), CompileError> {
    if stack_kinds.len() != stack.len() || capture_count > stack.len() {
        return Err(CompileError::Backend);
    }
    let mut roots = Vec::new();
    for (slot, (kind, variable)) in local_kinds
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
    let stack_start = roots.len();
    roots.extend(stack.iter().copied().map(|value| NativeRoot {
        bits: value.bits,
        tag: value.tag,
        state: None,
    }));
    let capture_start = stack_start
        .checked_add(stack.len() - capture_count)
        .ok_or(CompileError::Backend)?;
    Ok((roots, capture_start))
}

fn emit_capture_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: CaptureAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let CaptureAllocationEmission {
        function,
        environment,
        capture_start,
        capture_count,
        roots,
        callback,
        point,
        replay_stack,
        fault_stack,
    } = emission;
    let capture_end = capture_start
        .checked_add(capture_count)
        .ok_or(CompileError::Backend)?;
    let capture_roots = roots
        .get(capture_start..capture_end)
        .ok_or(CompileError::Backend)?;
    let fast_root_count = emit_runtime_roots(builder, values, capture_roots)?;
    let function = builder.ins().iconst(types::I32, i64::from(function));
    let fast_capture_start = builder.ins().iconst(types::I32, 0);
    let slow_capture_start = builder.ins().iconst(
        types::I32,
        i64::try_from(capture_start).map_err(|_| CompileError::Backend)?,
    );
    let capture_count = builder.ins().iconst(
        types::I32,
        i64::try_from(capture_count).map_err(|_| CompileError::Backend)?,
    );
    let function_offset = if callback {
        mem::offset_of!(RawNativeFunctions, allocate_callback)
    } else {
        mem::offset_of!(RawNativeFunctions, allocate_closure)
    };
    let allocation = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.capture_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            function,
            environment,
            no_collection,
            fast_capture_start,
            capture_count,
            fast_root_count,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let status = if callback {
        fast_status
    } else {
        let retry = builder.create_block();
        let done = builder.create_block();
        builder.append_block_param(done, types::I32);
        let collection_required = builder.ins().icmp_imm(
            IntCC::Equal,
            fast_status,
            i64::from(RUNTIME_COLLECTION_REQUIRED),
        );
        builder
            .ins()
            .brif(collection_required, retry, &[], done, &[fast_status.into()]);

        builder.switch_to_block(retry);
        let root_count = emit_runtime_roots(builder, values, roots)?;
        let allow_collection = builder.ins().iconst(types::I32, 1);
        let slow_call = builder.ins().call_indirect(
            values.capture_allocation_signature,
            allocation,
            &[
                values.runtime_context,
                function,
                environment,
                allow_collection,
                slow_capture_start,
                capture_count,
                root_count,
                values.allocation_result_pointer,
            ],
        );
        let slow_status = builder.inst_results(slow_call)[0];
        builder.ins().jump(done, &[slow_status.into()]);

        builder.switch_to_block(done);
        builder.block_params(done)[0]
    };
    let limit_status = if callback {
        RUNTIME_STACK_LIMIT
    } else {
        RUNTIME_HEAP_LIMIT
    };
    let limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(limit_status));
    let limit_exit = if callback {
        EXIT_STACK_LIMIT
    } else {
        EXIT_HEAP_LIMIT
    };
    emit_fault_check(builder, values, limit, limit_exit, point, fault_stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

fn emit_value_array_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: ValueArrayAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let ValueArrayAllocationEmission {
        kind,
        item_start,
        item_count,
        roots,
        point,
        replay_stack,
        fault_stack,
    } = emission;
    let item_end = item_start
        .checked_add(item_count)
        .ok_or(CompileError::Backend)?;
    let item_roots = roots
        .get(item_start..item_end)
        .ok_or(CompileError::Backend)?;
    let fast_root_count = emit_runtime_roots(builder, values, item_roots)?;
    let fast_item_start = builder.ins().iconst(types::I32, 0);
    let slow_item_start = builder.ins().iconst(
        types::I32,
        i64::try_from(item_start).map_err(|_| CompileError::Backend)?,
    );
    let item_count = builder.ins().iconst(
        types::I32,
        i64::try_from(item_count).map_err(|_| CompileError::Backend)?,
    );
    let function_offset = match kind {
        ValueArrayAllocationKind::Tuple => {
            mem::offset_of!(RawNativeFunctions, allocate_tuple)
        }
        ValueArrayAllocationKind::List => mem::offset_of!(RawNativeFunctions, allocate_list),
        ValueArrayAllocationKind::Map => mem::offset_of!(RawNativeFunctions, allocate_map),
    };
    let allocation = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.value_array_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            no_collection,
            fast_item_start,
            item_count,
            fast_root_count,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let retry = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    let collection_required = builder.ins().icmp_imm(
        IntCC::Equal,
        fast_status,
        i64::from(RUNTIME_COLLECTION_REQUIRED),
    );
    builder
        .ins()
        .brif(collection_required, retry, &[], done, &[fast_status.into()]);

    builder.switch_to_block(retry);
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let allow_collection = builder.ins().iconst(types::I32, 1);
    let slow_call = builder.ins().call_indirect(
        values.value_array_allocation_signature,
        allocation,
        &[
            values.runtime_context,
            allow_collection,
            slow_item_start,
            item_count,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let slow_status = builder.inst_results(slow_call)[0];
    builder.ins().jump(done, &[slow_status.into()]);

    builder.switch_to_block(done);
    let status = builder.block_params(done)[0];
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        point,
        fault_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

fn emit_allocate_instance(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    emission: InstanceAllocationEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let InstanceAllocationEmission {
        roots,
        allow_pending,
        exit,
    } = emission;
    let (status, result) = if allow_pending {
        emit_requested_instance_allocation(builder, values, class, field_count, environment, roots)?
    } else {
        emit_instance_allocation(builder, values, class, field_count, environment, roots)?
    };
    let heap_limit = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_HEAP_LIMIT));
    emit_fault_check(
        builder,
        values,
        heap_limit,
        EXIT_HEAP_LIMIT,
        exit.point,
        exit.deopt_stack,
    )?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    Ok(result)
}

fn instance_field_count(input: &FunctionInput<'_>, class: u32) -> Option<u32> {
    let source_class = match input.root.class_relocation {
        Some(classes) => classes.iter().position(|relocated| *relocated == class)?,
        None => class as usize,
    };
    let count = input.root.source.classes.get(source_class)?.fields.len();
    u32::try_from(count).ok()
}

fn emit_requested_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let request = load_value(
        builder,
        types::I32,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_request),
    )?;
    let zero = builder.ins().iconst(types::I32, 0);
    store_i32_value(
        builder,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_request),
        zero,
    )?;
    let requested = builder.ins().icmp_imm(IntCC::NotEqual, request, 0);
    let pending = builder.create_block();
    let canonical = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(requested, pending, &[], canonical, &[]);

    builder.switch_to_block(pending);
    let (status, result) = match field_count {
        Some(field_count) if field_count as usize <= VIRTUAL_INSTANCE_FIELDS => {
            emit_pending_instance_allocation(builder, values, class, field_count, environment)?
        }
        _ => {
            let status = builder
                .ins()
                .iconst(types::I32, i64::from(RUNTIME_COLLECTION_REQUIRED));
            let result = builder.ins().iconst(types::I64, 0);
            (status, result)
        }
    };
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(canonical);
    let (status, result) =
        emit_instance_allocation(builder, values, class, field_count, environment, roots)?;
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

fn emit_pending_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: u32,
    environment: ir::Value,
) -> Result<(ir::Value, ir::Value), CompileError> {
    let cost = (field_count as usize)
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    let cost = i64::try_from(cost).map_err(|_| CompileError::Backend)?;
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, cost);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let charge_blocked = builder.ins().bor(charge_overflow, collection_due);

    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let has_record = builder.ins().icmp_imm(IntCC::NotEqual, available, 0);
    let charge_ready = builder.ins().bxor_imm(charge_blocked, 1);
    let ready = builder.ins().band(has_record, charge_ready);

    let allocate = builder.create_block();
    let unavailable = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(ready, allocate, &[], unavailable, &[]);

    builder.switch_to_block(unavailable);
    let status = builder
        .ins()
        .iconst(types::I32, i64::from(RUNTIME_COLLECTION_REQUIRED));
    let result = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(allocate);
    let record_index = builder.ins().ctz(available);
    let next_available = builder.ins().iadd_imm(available, -1);
    let next_available = builder.ins().band(available, next_available);
    let available_offset = i32::try_from(mem::offset_of!(RawNativeActivation, virtual_available))
        .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        next_available,
        values.activation_pointer,
        available_offset,
    );
    let instances = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let record_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(mem::size_of::<RawVirtualInstance>()).map_err(|_| CompileError::Backend)?,
    );
    let record = builder.ins().iadd(instances, record_offset);
    let fields = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    let field_record_offset = builder.ins().imul_imm(
        record_index,
        i64::try_from(VIRTUAL_INSTANCE_FIELDS.saturating_mul(VALUE_SIZE))
            .map_err(|_| CompileError::Backend)?,
    );
    let field_data = builder.ins().iadd(fields, field_record_offset);
    let uninit = builder
        .ins()
        .iconst(types::I64, ValueTag::Uninit as u64 as i64);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    for field in 0..field_count as usize {
        let offset =
            i32::try_from(field.saturating_mul(VALUE_SIZE)).map_err(|_| CompileError::Backend)?;
        store_i64(
            builder,
            field_data,
            offset as usize + VALUE_TAG_OFFSET,
            uninit,
        )?;
        store_i64(
            builder,
            field_data,
            offset as usize + VALUE_PAYLOAD_OFFSET,
            zero_i64,
        )?;
    }
    let record_i32 = builder.ins().ireduce(types::I32, record_index);
    let maximum = builder.ins().iconst(types::I32, i64::from(u32::MAX));
    let token = builder.ins().isub(maximum, record_i32);
    let result = builder.ins().uextend(types::I64, token);
    let class_value = builder.ins().iconst(types::I32, i64::from(class));
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, next_live)?;
    let allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, inline_allocations),
    )?;
    let allocations = builder.ins().iadd_imm(allocations, 1);
    let allocations_offset =
        i32::try_from(mem::offset_of!(RawNativeActivation, inline_allocations))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        allocations,
        values.activation_pointer,
        allocations_offset,
    );
    let pending_allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, pending_instance_allocations),
    )?;
    let pending_allocations = builder.ins().iadd_imm(pending_allocations, 1);
    let pending_allocations_offset = i32::try_from(mem::offset_of!(
        RawNativeActivation,
        pending_instance_allocations
    ))
    .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        pending_allocations,
        values.activation_pointer,
        pending_allocations_offset,
    );

    let one_i32 = builder.ins().iconst(types::I32, 1);
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, active),
        one_i32,
    )?;
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, references),
        one_i32,
    )?;
    store_i64(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, object_bits),
        result,
    )?;
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, class),
        class_value,
    )?;
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, environment),
        environment,
    )?;
    let count = builder.ins().iconst(types::I32, i64::from(field_count));
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, field_count),
        count,
    )?;
    let zero_i32 = builder.ins().iconst(types::I32, 0);
    store_i32_value(
        builder,
        record,
        mem::offset_of!(RawVirtualInstance, frozen),
        zero_i32,
    )?;
    let status = builder.ins().iconst(types::I32, i64::from(RUNTIME_OK));
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

fn emit_instance_allocation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    field_count: Option<u32>,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let Some(field_count) = field_count else {
        return emit_allocation_call(builder, values, class, environment, roots);
    };
    let cost = (field_count as usize)
        .checked_mul(VALUE_SIZE)
        .and_then(|fields| MIN_OBJECT_COST.checked_add(fields))
        .ok_or(CompileError::Backend)?;
    let cost = i64::try_from(cost).map_err(|_| CompileError::Backend)?;

    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, cost);
    let charge_overflow = builder.ins().icmp(IntCC::UnsignedLessThan, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let collection_due = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThan, next_used, threshold);
    let charge_blocked = builder.ins().bor(charge_overflow, collection_due);

    let free = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_free),
    )?;
    let free_len = load_heap_value(builder, values.pointer_type, free, OWNED_ARRAY_LEN_OFFSET)?;
    let has_free = builder.ins().icmp_imm(IntCC::NotEqual, free_len, 0);
    let slots_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_slots),
    )?;
    let slots = load_heap_value(builder, values.pointer_type, slots_pointer, 0)?;
    let page_count = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_page_count),
    )?;
    let page_capacity = builder
        .ins()
        .ishl_imm(page_count, i64::from(JIT_PAGE_SHIFT));
    let has_fresh = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, slots, page_capacity);
    let has_slot = builder.ins().bor(has_free, has_fresh);
    let charge_ready = builder.ins().bxor_imm(charge_blocked, 1);
    let fast = builder.ins().band(has_slot, charge_ready);

    let fast_block = builder.create_block();
    let slow_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(fast, fast_block, &[], slow_block, &[]);

    builder.switch_to_block(slow_block);
    let (slow_status, slow_result) =
        emit_allocation_call(builder, values, class, environment, roots)?;
    builder
        .ins()
        .jump(done, &[slow_status.into(), slow_result.into()]);

    builder.switch_to_block(fast_block);
    let fields_ready = builder.create_block();
    builder.append_block_param(fields_ready, values.pointer_type);
    builder.append_block_param(fields_ready, values.pointer_type);
    builder.append_block_param(fields_ready, values.pointer_type);
    if field_count == 0 {
        let data = builder.ins().iconst(
            values.pointer_type,
            i64::try_from(VALUE_ARRAY_EMPTY_DATA).map_err(|_| CompileError::Backend)?,
        );
        let zero = builder.ins().iconst(values.pointer_type, 0);
        builder
            .ins()
            .jump(fields_ready, &[data.into(), zero.into(), zero.into()]);
    } else {
        let prepare = load_value(
            builder,
            values.pointer_type,
            values.runtime_functions,
            mem::offset_of!(RawNativeFunctions, prepare_instance_fields),
        )?;
        let count = builder.ins().iconst(types::I32, i64::from(field_count));
        let call = builder.ins().call_indirect(
            values.instance_fields_signature,
            prepare,
            &[count, values.allocation_result_pointer],
        );
        let status = builder.inst_results(call)[0];
        let data = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            0,
        );
        let len = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            8,
        );
        let capacity = builder.ins().load(
            values.pointer_type,
            MemFlags::new(),
            values.allocation_result_pointer,
            16,
        );
        let prepared = builder
            .ins()
            .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
        let failed = builder.create_block();
        builder.ins().brif(
            prepared,
            fields_ready,
            &[data.into(), len.into(), capacity.into()],
            failed,
            &[],
        );
        builder.switch_to_block(failed);
        let zero = builder.ins().iconst(types::I64, 0);
        builder.ins().jump(done, &[status.into(), zero.into()]);
    }

    builder.switch_to_block(fields_ready);
    let fields_data = builder.block_params(fields_ready)[0];
    let fields_len = builder.block_params(fields_ready)[1];
    let fields_capacity = builder.block_params(fields_ready)[2];
    let recycled = builder.create_block();
    let fresh = builder.create_block();
    let slot_ready = builder.create_block();
    builder.append_block_param(slot_ready, types::I32);
    builder.ins().brif(has_free, recycled, &[], fresh, &[]);

    builder.switch_to_block(recycled);
    let free_data = load_heap_value(builder, values.pointer_type, free, OWNED_ARRAY_DATA_OFFSET)?;
    let next_free_len = builder.ins().iadd_imm(free_len, -1);
    let free_offset = builder.ins().imul_imm(next_free_len, 4);
    let free_slot = builder.ins().iadd(free_data, free_offset);
    let recycled_slot = builder
        .ins()
        .load(types::I32, heap_mem_flags(), free_slot, 0);
    store_heap_value(builder, free, OWNED_ARRAY_LEN_OFFSET, next_free_len)?;
    builder.ins().jump(slot_ready, &[recycled_slot.into()]);

    builder.switch_to_block(fresh);
    let fresh_slot = builder.ins().ireduce(types::I32, slots);
    let next_slots = builder.ins().iadd_imm(slots, 1);
    store_heap_value(builder, slots_pointer, 0, next_slots)?;
    let slot_count_offset = i32::try_from(mem::offset_of!(RawNativeActivation, heap_slot_count))
        .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        next_slots,
        values.activation_pointer,
        slot_count_offset,
    );
    builder.ins().jump(slot_ready, &[fresh_slot.into()]);

    builder.switch_to_block(slot_ready);
    let slot = builder.block_params(slot_ready)[0];
    let slot_index = builder.ins().uextend(values.pointer_type, slot);
    let pages = load_vmctx_value(
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
        .load(values.pointer_type, table_mem_flags(), page_address, 0);
    let within_page = builder.ins().band_imm(slot_index, i64::from(JIT_PAGE_MASK));
    let entry_offset = builder.ins().imul_imm(
        within_page,
        i64::try_from(JIT_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(page, entry_offset);
    let generation = load_heap_value(builder, types::I32, entry, JIT_ENTRY_GENERATION_OFFSET)?;

    let zero_i64 = builder.ins().iconst(types::I64, 0);
    let cost_value = builder.ins().iconst(values.pointer_type, cost);
    let object_tag = builder
        .ins()
        .iconst(types::I32, i64::from(JIT_OBJECT_INSTANCE));
    let class_value = builder.ins().iconst(types::I32, i64::from(class));
    store_heap_value(builder, entry, JIT_ENTRY_FROZEN_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_BYTES_OFFSET, cost_value)?;
    store_heap_value(builder, entry, JIT_ENTRY_SHARED_PRESENT_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_SHARED_KEY_OFFSET, zero_i64)?;
    store_heap_value(builder, entry, JIT_ENTRY_OBJECT_TAG_OFFSET, object_tag)?;
    store_heap_value(builder, entry, JIT_INSTANCE_CLASS_OFFSET, class_value)?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_DATA_OFFSET,
        fields_data,
    )?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_LEN_OFFSET,
        fields_len,
    )?;
    store_heap_value(
        builder,
        entry,
        JIT_INSTANCE_FIELDS_OFFSET + VALUE_ARRAY_CAPACITY_OFFSET,
        fields_capacity,
    )?;
    store_heap_value(builder, entry, JIT_INSTANCE_ENV_OFFSET, environment)?;
    let live_tag = builder
        .ins()
        .iconst(types::I32, i64::from(JIT_ENTRY_LIVE_TAG));
    store_heap_value(builder, entry, JIT_ENTRY_LIVE_OFFSET, live_tag)?;
    store_heap_value(builder, used_pointer, 0, next_used)?;
    let live_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
    let next_live = builder.ins().iadd_imm(live, 1);
    store_heap_value(builder, live_pointer, 0, next_live)?;
    let allocations = load_vmctx_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, inline_allocations),
    )?;
    let allocations = builder.ins().iadd_imm(allocations, 1);
    let allocations_offset =
        i32::try_from(mem::offset_of!(RawNativeActivation, inline_allocations))
            .map_err(|_| CompileError::Backend)?;
    builder.ins().store(
        vmctx_mem_flags(),
        allocations,
        values.activation_pointer,
        allocations_offset,
    );

    let generation = builder.ins().uextend(types::I64, generation);
    let generation = builder.ins().ishl_imm(generation, 32);
    let slot = builder.ins().uextend(types::I64, slot);
    let result = builder.ins().bor(generation, slot);
    let status = builder.ins().iconst(types::I32, i64::from(RUNTIME_OK));
    builder.ins().jump(done, &[status.into(), result.into()]);

    builder.switch_to_block(done);
    Ok((builder.block_params(done)[0], builder.block_params(done)[1]))
}

fn emit_allocation_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    class: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
) -> Result<(ir::Value, ir::Value), CompileError> {
    let class = builder.ins().iconst(types::I32, i64::from(class));
    let allocate_instance = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, allocate_instance),
    )?;
    let no_roots = builder.ins().iconst(types::I32, 0);
    let no_collection = builder.ins().iconst(types::I32, 0);
    let fast_call = builder.ins().call_indirect(
        values.allocation_signature,
        allocate_instance,
        &[
            values.runtime_context,
            class,
            environment,
            no_collection,
            no_roots,
            values.allocation_result_pointer,
        ],
    );
    let fast_status = builder.inst_results(fast_call)[0];
    let retry = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I32);
    let collection_required = builder.ins().icmp_imm(
        IntCC::Equal,
        fast_status,
        i64::from(RUNTIME_COLLECTION_REQUIRED),
    );
    builder
        .ins()
        .brif(collection_required, retry, &[], done, &[fast_status.into()]);

    builder.switch_to_block(retry);
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let allow_collection = builder.ins().iconst(types::I32, 1);
    let slow_call = builder.ins().call_indirect(
        values.allocation_signature,
        allocate_instance,
        &[
            values.runtime_context,
            class,
            environment,
            allow_collection,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let slow_status = builder.inst_results(slow_call)[0];
    builder.ins().jump(done, &[slow_status.into()]);

    builder.switch_to_block(done);
    let status = builder.block_params(done)[0];
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    Ok((status, result))
}

fn emit_graph_digest(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    ty: u32,
    environment: ir::Value,
    roots: &[NativeRoot],
    exit: ReplayEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let ty = builder.ins().iconst(types::I32, i64::from(ty));
    let collection = builder.ins().iconst(types::I32, 1);
    let digest = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, digest_value),
    )?;
    let call = builder.ins().call_indirect(
        values.digest_signature,
        digest,
        &[
            values.runtime_context,
            reference,
            ty,
            environment,
            collection,
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, exit.point, exit.deopt_stack)?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

fn emit_heap_operation(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    arguments: [ir::Value; 3],
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.heap_operation_signature,
        function,
        &[
            values.runtime_context,
            arguments[0],
            arguments[1],
            arguments[2],
            root_count,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
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
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
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

fn emit_list_insert_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    stored: NativeValue,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let insert_list = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, insert_list),
    )?;
    let call = builder.ins().call_indirect(
        values.list_insert_signature,
        insert_list,
        &[
            values.runtime_context,
            reference,
            index,
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

fn emit_map_reserve_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    additional: ir::Value,
    roots: &[NativeRoot],
) -> Result<ir::Value, CompileError> {
    let root_count = emit_runtime_roots(builder, values, roots)?;
    let reserve_map = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, map_reserve),
    )?;
    let call = builder.ins().call_indirect(
        values.list_reserve_signature,
        reserve_map,
        &[values.runtime_context, reference, additional, root_count],
    );
    Ok(builder.inst_results(call)[0])
}

fn emit_raw_map_value_call(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    first: ir::Value,
    second: ir::Value,
) -> Result<(ir::Value, NativeValue), CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        function,
        &[
            values.runtime_context,
            reference,
            first,
            second,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    Ok((status, NativeValue { bits, tag }))
}

#[allow(clippy::too_many_arguments)]
fn emit_optional_map_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    key: NativeValue,
    family: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let (status, found_value) = emit_raw_map_value_call(
        builder,
        values,
        function_offset,
        reference,
        key.bits,
        key.tag,
    )?;
    emit_runtime_fault_status(builder, values, status, exit.point, exit.fault_stack)?;
    let found = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
    let missing = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_MAP_VACANT));
    let valid = builder.ins().bor(found, missing);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;

    let found_block = builder.create_block();
    let missing_block = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder
        .ins()
        .brif(found, found_block, &[], missing_block, &[]);

    builder.switch_to_block(found_block);
    emit_native_value_contract(
        builder,
        values,
        found_value,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(done, &[found_value.bits.into(), found_value.tag.into()]);

    builder.switch_to_block(missing_block);
    let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let bits = builder.ins().bor(family, arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(done, &[bits.into(), tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_map_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    first: ir::Value,
    second: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let (status, result) =
        emit_raw_map_value_call(builder, values, function_offset, reference, first, second)?;
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

fn emit_object_binary_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    argument: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_binary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            argument,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = NativeValue {
        bits: builder.ins().load(
            types::I64,
            MemFlags::new(),
            values.allocation_result_pointer,
            0,
        ),
        tag: builder.ins().load(
            types::I64,
            MemFlags::new(),
            values.allocation_result_pointer,
            8,
        ),
    };
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

fn emit_object_unary_runtime_value(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_unary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = NativeValue {
        bits: builder.ins().load(
            types::I64,
            MemFlags::new(),
            values.allocation_result_pointer,
            0,
        ),
        tag: builder.ins().load(
            types::I64,
            MemFlags::new(),
            values.allocation_result_pointer,
            8,
        ),
    };
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

fn emit_map_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: MapLookupEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let Some(key_kind) = direct_map_key_kind(emission.key_contract) else {
        return emit_map_lookup_slow(builder, values, emission);
    };

    let entry = emit_object_entry(
        builder,
        values,
        emission.reference,
        JIT_OBJECT_MAP,
        emission.exit.point,
        ObjectGuard::Replay(emission.exit.deopt_stack),
    )?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let key = emit_direct_map_key(builder, values, emission.key, key_kind, emission.exit)?;
    let probe_start = builder.create_block();
    builder.ins().brif(key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        entry,
        entry_count,
        emission.key,
        key,
        emission.exit,
    )?;
    match emission.result {
        MapLookupResult::Has => {
            let found = builder.ins().uextend(types::I64, probe.found);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::Bool as u64 as i64);
            builder.ins().jump(done, &[found.into(), tag.into()]);
        }
        MapLookupResult::At => {
            let hit = builder.create_block();
            builder.ins().brif(probe.found, hit, &[], slow, &[]);
            builder.switch_to_block(hit);
            builder
                .ins()
                .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);
        }
        MapLookupResult::Get { family, value } => {
            let hit = builder.create_block();
            let missing = builder.create_block();
            builder.ins().brif(probe.found, hit, &[], missing, &[]);

            builder.switch_to_block(hit);
            emit_native_value_contract(
                builder,
                values,
                probe.value,
                value,
                emission.exit.point,
                emission.exit.deopt_stack,
            )?;
            builder
                .ins()
                .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);

            builder.switch_to_block(missing);
            let arm = builder.ins().iconst(types::I64, 1_i64 << 32);
            let bits = builder.ins().bor(family, arm);
            let tag = builder
                .ins()
                .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
            builder.ins().jump(done, &[bits.into(), tag.into()]);
        }
    }

    builder.switch_to_block(slow);
    let result = emit_map_lookup_slow(builder, values, emission)?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_map_remove(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    key_contract: ValueContract,
    option_family: ir::Value,
    value_contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let Some(key_kind) = direct_map_key_kind(key_contract) else {
        return emit_optional_map_value(
            builder,
            values,
            mem::offset_of!(RawNativeFunctions, map_remove),
            reference,
            key,
            option_family,
            value_contract,
            exit,
        );
    };
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, map_entry, exit)?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, map_entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let index_ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.ins().brif(index_ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let direct_key = emit_direct_map_key(builder, values, key, key_kind, exit)?;
    let probe_start = builder.create_block();
    builder
        .ins()
        .brif(direct_key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        map_entry,
        entry_count,
        key,
        direct_key,
        exit,
    )?;
    let hit = builder.create_block();
    let missing = builder.create_block();
    builder.ins().brif(probe.found, hit, &[], missing, &[]);

    builder.switch_to_block(missing);
    let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let none_bits = builder.ins().bor(option_family, none_arm);
    let none_tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder
        .ins()
        .jump(done, &[none_bits.into(), none_tag.into()]);

    builder.switch_to_block(hit);
    emit_native_value_contract(
        builder,
        values,
        probe.value,
        value_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let live = load_heap_value(builder, types::I32, map_entry, JIT_MAP_LIVE_OFFSET)?;
    let has_live_entry = builder.ins().icmp_imm(IntCC::NotEqual, live, 0);
    let next_live = builder.ins().iadd_imm(live, -1);
    let next_live_native = builder.ins().uextend(values.pointer_type, next_live);
    let tombstones = builder.ins().isub(entry_count, next_live_native);
    let compaction_floor = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, tombstones, 8);
    let weighted_tombstones = builder.ins().imul_imm(tombstones, 3);
    let compaction_ratio =
        builder
            .ins()
            .icmp(IntCC::UnsignedGreaterThan, weighted_tombstones, entry_count);
    let needs_compaction = builder.ins().band(compaction_floor, compaction_ratio);
    let no_compaction = builder.ins().bxor_imm(needs_compaction, 1);
    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch_ready = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, epoch, i64::from(u32::MAX));
    let fast = builder.ins().band(has_live_entry, no_compaction);
    let fast = builder.ins().band(fast, epoch_ready);
    let commit = builder.create_block();
    builder.ins().brif(fast, commit, &[], slow, &[]);

    builder.switch_to_block(commit);
    let zero = builder.ins().iconst(types::I64, 0);
    let uninit = builder
        .ins()
        .iconst(types::I64, ValueTag::Uninit as u64 as i64);
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
        zero,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
        uninit,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_VALUE_OFFSET + VALUE_PAYLOAD_OFFSET,
        zero,
    )?;
    store_heap_value(
        builder,
        probe.entry,
        MAP_ENTRY_VALUE_OFFSET + VALUE_TAG_OFFSET,
        uninit,
    )?;
    store_heap_value(builder, probe.entry, MAP_ENTRY_SEMANTIC_HASH_OFFSET, zero)?;
    let epoch_tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let incremented_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder
        .ins()
        .select(epoch_tracked, incremented_epoch, epoch);
    store_heap_value(builder, map_entry, JIT_MAP_LIVE_OFFSET, next_live)?;
    store_heap_value(builder, map_entry, JIT_MAP_EPOCH_OFFSET, next_epoch)?;
    builder
        .ins()
        .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);

    builder.switch_to_block(slow);
    let result = emit_optional_map_value(
        builder,
        values,
        mem::offset_of!(RawNativeFunctions, map_remove),
        reference,
        key,
        option_family,
        value_contract,
        exit,
    )?;
    builder
        .ins()
        .jump(done, &[result.bits.into(), result.tag.into()]);

    builder.switch_to_block(done);
    Ok(NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    })
}

fn emit_map_next_index(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    cursor: ir::Value,
    expected: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch = builder.ins().uextend(types::I64, epoch);
    let negative_epoch = builder.ins().icmp_imm(IntCC::SignedLessThan, expected, 0);
    let wrong_epoch = builder.ins().icmp(IntCC::NotEqual, epoch, expected);
    let invalid_epoch = builder.ins().bor(negative_epoch, wrong_epoch);
    emit_interpreter_replay(builder, values, invalid_epoch, exit.point, exit.deopt_stack)?;
    let negative_cursor = builder.ins().icmp_imm(IntCC::SignedLessThan, cursor, 0);
    emit_interpreter_replay(
        builder,
        values,
        negative_cursor,
        exit.point,
        exit.deopt_stack,
    )?;

    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let scan = builder.create_block();
    let found = builder.create_block();
    let missing = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(scan, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.append_block_param(done, types::I64);
    let in_range = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, cursor, count_i64);
    let cursor_native = if values.pointer_type == types::I64 {
        cursor
    } else {
        builder.ins().ireduce(values.pointer_type, cursor)
    };
    builder
        .ins()
        .brif(in_range, scan, &[cursor_native.into()], missing, &[]);

    builder.switch_to_block(scan);
    let position = builder.block_params(scan)[0];
    let byte_offset = builder.ins().imul_imm(
        position,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, byte_offset);
    let tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let live = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, tag, ValueTag::Uninit as u64 as i64);
    let next = builder.ins().iadd_imm(position, 1);
    let more = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, next, entry_count);
    let tombstone = builder.ins().bxor_imm(live, 1);
    let continue_scan = builder.ins().band(tombstone, more);
    let next_or_missing = builder.create_block();
    builder
        .ins()
        .brif(live, found, &[position.into()], next_or_missing, &[]);

    builder.switch_to_block(next_or_missing);
    builder
        .ins()
        .brif(continue_scan, scan, &[next.into()], missing, &[]);

    builder.switch_to_block(found);
    let position = builder.block_params(found)[0];
    let position = if values.pointer_type == types::I64 {
        position
    } else {
        builder.ins().uextend(types::I64, position)
    };
    builder.ins().jump(done, &[position.into()]);

    builder.switch_to_block(missing);
    let none = builder.ins().iconst(types::I64, -1);
    builder.ins().jump(done, &[none.into()]);

    builder.switch_to_block(done);
    let result = builder.block_params(done)[0];
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::Int as u64 as i64);
    Ok(NativeValue { bits: result, tag })
}

fn emit_map_entry_at(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    index: ir::Value,
    load_stored_value: bool,
    contract: ValueContract,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let negative = builder.ins().icmp_imm(IntCC::SignedLessThan, index, 0);
    let outside = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, index, count_i64);
    let invalid = builder.ins().bor(negative, outside);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    let native_index = if values.pointer_type == types::I64 {
        index
    } else {
        builder.ins().ireduce(values.pointer_type, index)
    };
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let byte_offset = builder.ins().imul_imm(
        native_index,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, byte_offset);
    let key_tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let tombstone = builder
        .ins()
        .icmp_imm(IntCC::Equal, key_tag, ValueTag::Uninit as u64 as i64);
    emit_interpreter_replay(builder, values, tombstone, exit.point, exit.deopt_stack)?;
    let offset = if load_stored_value {
        MAP_ENTRY_VALUE_OFFSET
    } else {
        MAP_ENTRY_KEY_OFFSET
    };
    let result = NativeValue {
        bits: load_heap_value(builder, types::I64, entry, offset + VALUE_PAYLOAD_OFFSET)?,
        tag: load_heap_value(builder, types::I64, entry, offset + VALUE_TAG_OFFSET)?,
    };
    emit_native_value_contract(
        builder,
        values,
        result,
        contract,
        exit.point,
        exit.deopt_stack,
    )?;
    Ok(result)
}

fn emit_map_lookup_slow(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    emission: MapLookupEmission<'_>,
) -> Result<NativeValue, CompileError> {
    match emission.result {
        MapLookupResult::Has => emit_runtime_value_lookup(
            builder,
            values,
            mem::offset_of!(RawNativeFunctions, map_has),
            emission.reference,
            emission.key,
            emission.exit,
        ),
        MapLookupResult::At => emit_runtime_value_lookup(
            builder,
            values,
            mem::offset_of!(RawNativeFunctions, map_at),
            emission.reference,
            emission.key,
            emission.exit,
        ),
        MapLookupResult::Get { family, value } => emit_optional_map_value(
            builder,
            values,
            mem::offset_of!(RawNativeFunctions, map_get),
            emission.reference,
            emission.key,
            family,
            value,
            emission.exit,
        ),
    }
}

#[derive(Clone, Copy)]
struct DirectMapProbe {
    found: ir::Value,
    value: NativeValue,
    entry: ir::Value,
    vacant_slot: ir::Value,
}

#[derive(Clone, Copy)]
enum DirectMapKeyKind {
    Scalar(ScalarKind),
    Str,
    Text,
    Bytes,
}

#[derive(Clone, Copy)]
struct DirectMapKey {
    kind: DirectMapKeyKind,
    semantic_hash: ir::Value,
    lookup_hash: ir::Value,
    object_entry: Option<ir::Value>,
    ready: ir::Value,
}

fn direct_map_key_kind(contract: ValueContract) -> Option<DirectMapKeyKind> {
    match (contract.kind, contract.object) {
        (
            kind @ (ScalarKind::Unit
            | ScalarKind::Bool
            | ScalarKind::Int
            | ScalarKind::Float
            | ScalarKind::Char),
            None,
        ) => Some(DirectMapKeyKind::Scalar(kind)),
        (ScalarKind::Object(_), Some(ObjectContract::Str)) => Some(DirectMapKeyKind::Str),
        (ScalarKind::Object(_), Some(ObjectContract::Text)) => Some(DirectMapKeyKind::Text),
        (ScalarKind::Object(_), Some(ObjectContract::Bytes)) => Some(DirectMapKeyKind::Bytes),
        _ => None,
    }
}

fn emit_direct_map_key(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    key: NativeValue,
    kind: DirectMapKeyKind,
    exit: HeapExitEmission<'_>,
) -> Result<DirectMapKey, CompileError> {
    let (semantic_hash, lookup_hash, object_entry, ready) = match kind {
        DirectMapKeyKind::Scalar(kind) => {
            let semantic_hash = emit_scalar_map_semantic_hash(builder, key.bits, kind);
            let lookup_key = load_vmctx_value(
                builder,
                types::I64,
                values.activation_pointer,
                mem::offset_of!(RawNativeActivation, lookup_hash_key),
            )?;
            let lookup_hash = builder.ins().bxor(semantic_hash, lookup_key);
            let lookup_hash = emit_stable_hash_mix(builder, lookup_hash);
            let ready = builder.ins().iconst(types::I8, 1);
            (semantic_hash, lookup_hash, None, ready)
        }
        DirectMapKeyKind::Str | DirectMapKeyKind::Text | DirectMapKeyKind::Bytes => {
            let entry = match kind {
                DirectMapKeyKind::Str => emit_object_entry(
                    builder,
                    values,
                    key.bits,
                    JIT_OBJECT_STR,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Text => emit_text_entry(
                    builder,
                    values,
                    key.bits,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Bytes => emit_object_entry(
                    builder,
                    values,
                    key.bits,
                    JIT_OBJECT_BYTES,
                    exit.point,
                    ObjectGuard::Replay(exit.deopt_stack),
                )?,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let offset = match kind {
                DirectMapKeyKind::Str | DirectMapKeyKind::Text => JIT_TEXT_LOOKUP_HASH_OFFSET,
                DirectMapKeyKind::Bytes => JIT_BYTES_LOOKUP_HASH_OFFSET,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let semantic_offset = match kind {
                DirectMapKeyKind::Str | DirectMapKeyKind::Text => JIT_TEXT_SEMANTIC_HASH_OFFSET,
                DirectMapKeyKind::Bytes => JIT_BYTES_SEMANTIC_HASH_OFFSET,
                DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
            };
            let semantic_hash = load_heap_value(builder, types::I64, entry, semantic_offset)?;
            let lookup_hash = load_heap_value(builder, types::I64, entry, offset)?;
            let ready = builder.ins().icmp_imm(IntCC::NotEqual, lookup_hash, 0);
            (semantic_hash, lookup_hash, Some(entry), ready)
        }
    };
    Ok(DirectMapKey {
        kind,
        semantic_hash,
        lookup_hash,
        object_entry,
        ready,
    })
}

fn emit_direct_map_probe(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    map_entry: ir::Value,
    entry_count: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<DirectMapProbe, CompileError> {
    let lookup_hash = direct_key.lookup_hash;
    let slots = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_DATA_OFFSET,
    )?;
    let slot_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_LEN_OFFSET,
    )?;
    let empty = builder.create_block();
    let start = builder.create_block();
    let probe = builder.create_block();
    let candidate = builder.create_block();
    let advance = builder.create_block();
    let found = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(empty, values.pointer_type);
    builder.append_block_param(probe, values.pointer_type);
    builder.append_block_param(probe, values.pointer_type);
    builder.append_block_param(candidate, values.pointer_type);
    builder.append_block_param(candidate, values.pointer_type);
    builder.append_block_param(advance, values.pointer_type);
    builder.append_block_param(advance, values.pointer_type);
    builder.append_block_param(found, values.pointer_type);
    builder.append_block_param(done, types::I8);
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, types::I64);
    builder.append_block_param(done, values.pointer_type);
    builder.append_block_param(done, values.pointer_type);

    let has_slots = builder.ins().icmp_imm(IntCC::NotEqual, slot_count, 0);
    let zero_pointer = builder.ins().iconst(values.pointer_type, 0);
    builder
        .ins()
        .brif(has_slots, start, &[], empty, &[zero_pointer.into()]);

    builder.switch_to_block(empty);
    let vacant_slot = builder.block_params(empty)[0];
    let zero_i8 = builder.ins().iconst(types::I8, 0);
    let zero_i64 = builder.ins().iconst(types::I64, 0);
    builder.ins().jump(
        done,
        &[
            zero_i8.into(),
            zero_i64.into(),
            zero_i64.into(),
            zero_pointer.into(),
            vacant_slot.into(),
        ],
    );

    builder.switch_to_block(start);
    let right = builder.ins().rotr_imm(lookup_hash, 25);
    let left = builder.ins().rotl_imm(lookup_hash, 17);
    let mixed = builder.ins().bxor(lookup_hash, right);
    let mixed = builder.ins().bxor(mixed, left);
    let mask = builder.ins().iadd_imm(slot_count, -1);
    let first = builder.ins().band(mixed, mask);
    builder
        .ins()
        .jump(probe, &[first.into(), slot_count.into()]);

    builder.switch_to_block(probe);
    let slot = builder.block_params(probe)[0];
    let remaining = builder.block_params(probe)[1];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let entry_index = load_heap_value(builder, types::I32, slot_address, MAP_SLOT_ENTRY_OFFSET)?;
    let occupied = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, entry_index, u32::MAX as i64);
    builder.ins().brif(
        occupied,
        candidate,
        &[slot.into(), remaining.into()],
        empty,
        &[slot_address.into()],
    );

    builder.switch_to_block(candidate);
    let slot = builder.block_params(candidate)[0];
    let remaining = builder.block_params(candidate)[1];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let stored_hash = load_heap_value(builder, types::I64, slot_address, MAP_SLOT_HASH_OFFSET)?;
    let same_hash = builder.ins().icmp(IntCC::Equal, stored_hash, lookup_hash);
    builder.ins().brif(
        same_hash,
        found,
        &[slot.into()],
        advance,
        &[slot.into(), remaining.into()],
    );

    builder.switch_to_block(found);
    let slot = builder.block_params(found)[0];
    let slot_offset = builder.ins().imul_imm(
        slot,
        i64::try_from(MAP_SLOT_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let slot_address = builder.ins().iadd(slots, slot_offset);
    let entry_index = load_heap_value(builder, types::I32, slot_address, MAP_SLOT_ENTRY_OFFSET)?;
    let entry_index = builder.ins().uextend(values.pointer_type, entry_index);
    let invalid = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, entry_index, entry_count);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;
    let entry_offset = builder.ins().imul_imm(
        entry_index,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let entry = builder.ins().iadd(entries, entry_offset);
    let equal = emit_direct_map_key_equal(builder, values, entry, key, direct_key, exit)?;
    let equal_block = builder.create_block();
    builder.ins().brif(
        equal,
        equal_block,
        &[],
        advance,
        &[slot.into(), remaining.into()],
    );

    builder.switch_to_block(equal_block);
    let value = NativeValue {
        bits: load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_VALUE_OFFSET + VALUE_PAYLOAD_OFFSET,
        )?,
        tag: load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_VALUE_OFFSET + VALUE_TAG_OFFSET,
        )?,
    };
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(
        done,
        &[
            one.into(),
            value.bits.into(),
            value.tag.into(),
            entry.into(),
            zero_pointer.into(),
        ],
    );

    builder.switch_to_block(advance);
    let slot = builder.block_params(advance)[0];
    let remaining = builder.block_params(advance)[1];
    let next = builder.ins().iadd_imm(slot, 1);
    let next = builder.ins().band(next, mask);
    let remaining = builder.ins().iadd_imm(remaining, -1);
    let continue_probe = builder.ins().icmp_imm(IntCC::NotEqual, remaining, 0);
    builder.ins().brif(
        continue_probe,
        probe,
        &[next.into(), remaining.into()],
        empty,
        &[zero_pointer.into()],
    );

    builder.switch_to_block(done);
    Ok(DirectMapProbe {
        found: builder.block_params(done)[0],
        value: NativeValue {
            bits: builder.block_params(done)[1],
            tag: builder.block_params(done)[2],
        },
        entry: builder.block_params(done)[3],
        vacant_slot: builder.block_params(done)[4],
    })
}

fn emit_scalar_map_semantic_hash(
    builder: &mut FunctionBuilder<'_>,
    bits: ir::Value,
    kind: ScalarKind,
) -> ir::Value {
    match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool | ScalarKind::Int | ScalarKind::Char => bits,
        ScalarKind::Float => {
            let shifted = builder.ins().ishl_imm(bits, 1);
            let zero = builder.ins().icmp_imm(IntCC::Equal, shifted, 0);
            let zero_bits = builder.ins().iconst(types::I64, 0);
            builder.ins().select(zero, zero_bits, bits)
        }
        _ => bits,
    }
}

fn emit_direct_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    entry: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    match direct_key.kind {
        DirectMapKeyKind::Scalar(kind) => emit_scalar_map_key_equal(builder, entry, key, kind),
        DirectMapKeyKind::Str | DirectMapKeyKind::Text | DirectMapKeyKind::Bytes => {
            emit_object_map_key_equal(builder, values, entry, key, direct_key, exit)
        }
    }
}

fn emit_object_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    map_entry: ir::Value,
    key: NativeValue,
    direct_key: DirectMapKey,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let key_entry = direct_key.object_entry.ok_or(CompileError::Backend)?;
    let stored_tag = load_heap_value(
        builder,
        types::I64,
        map_entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let stored_bits = load_heap_value(
        builder,
        types::I64,
        map_entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
    )?;
    let matching_tag =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, stored_tag, ValueTag::Obj as u64 as i64);
    let identical = builder.ins().icmp(IntCC::Equal, stored_bits, key.bits);
    let identical = builder.ins().band(matching_tag, identical);
    let matched = builder.create_block();
    let inspect = builder.create_block();
    let compare = builder.create_block();
    let missed = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I8);
    builder.ins().brif(identical, matched, &[], inspect, &[]);

    builder.switch_to_block(inspect);
    builder.ins().brif(matching_tag, compare, &[], missed, &[]);

    builder.switch_to_block(compare);
    let stored_entry = match direct_key.kind {
        DirectMapKeyKind::Str => emit_object_entry(
            builder,
            values,
            stored_bits,
            JIT_OBJECT_STR,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Text => emit_text_entry(
            builder,
            values,
            stored_bits,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Bytes => emit_object_entry(
            builder,
            values,
            stored_bits,
            JIT_OBJECT_BYTES,
            exit.point,
            ObjectGuard::Replay(exit.deopt_stack),
        )?,
        DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
    };
    let (data_offset, length_offset) = match direct_key.kind {
        DirectMapKeyKind::Str | DirectMapKeyKind::Text => {
            (JIT_TEXT_DATA_OFFSET, JIT_TEXT_BYTE_LEN_OFFSET)
        }
        DirectMapKeyKind::Bytes => (JIT_BYTES_DATA_OFFSET, JIT_BYTES_LEN_OFFSET),
        DirectMapKeyKind::Scalar(_) => return Err(CompileError::Backend),
    };
    let key_length = load_heap_value(builder, values.pointer_type, key_entry, length_offset)?;
    let stored_length = load_heap_value(builder, values.pointer_type, stored_entry, length_offset)?;
    let same_length = builder.ins().icmp(IntCC::Equal, key_length, stored_length);
    let compare_bytes = builder.create_block();
    builder
        .ins()
        .brif(same_length, compare_bytes, &[], missed, &[]);

    builder.switch_to_block(compare_bytes);
    let key_data = load_heap_value(builder, values.pointer_type, key_entry, data_offset)?;
    let stored_data = load_heap_value(builder, values.pointer_type, stored_entry, data_offset)?;
    let bytes_equal = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, bytes_equal),
    )?;
    let call = builder.ins().call_indirect(
        values.bytes_equal_signature,
        bytes_equal,
        &[key_data, stored_data, key_length],
    );
    let equal = builder.inst_results(call)[0];
    let equal = builder.ins().icmp_imm(IntCC::NotEqual, equal, 0);
    builder.ins().brif(equal, matched, &[], missed, &[]);

    builder.switch_to_block(matched);
    let one = builder.ins().iconst(types::I8, 1);
    builder.ins().jump(done, &[one.into()]);

    builder.switch_to_block(missed);
    let zero = builder.ins().iconst(types::I8, 0);
    builder.ins().jump(done, &[zero.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_scalar_map_key_equal(
    builder: &mut FunctionBuilder<'_>,
    entry: ir::Value,
    key: NativeValue,
    kind: ScalarKind,
) -> Result<ir::Value, CompileError> {
    let expected_tag = value_tag(kind).ok_or(CompileError::Backend)?;
    let stored_tag = load_heap_value(
        builder,
        types::I64,
        entry,
        MAP_ENTRY_KEY_OFFSET + VALUE_TAG_OFFSET,
    )?;
    let valid = builder
        .ins()
        .icmp_imm(IntCC::Equal, stored_tag, expected_tag as u64 as i64);
    let stored_bits = match kind {
        ScalarKind::Unit => builder.ins().iconst(types::I64, 0),
        ScalarKind::Bool => {
            let bits = load_heap_value(
                builder,
                types::I8,
                entry,
                MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
            )?;
            builder.ins().uextend(types::I64, bits)
        }
        ScalarKind::Char => {
            let bits = load_heap_value(
                builder,
                types::I32,
                entry,
                MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
            )?;
            builder.ins().uextend(types::I64, bits)
        }
        ScalarKind::Int | ScalarKind::Float => load_heap_value(
            builder,
            types::I64,
            entry,
            MAP_ENTRY_KEY_OFFSET + VALUE_PAYLOAD_OFFSET,
        )?,
        _ => return Err(CompileError::Backend),
    };
    let equal = if kind == ScalarKind::Float {
        let left = float_value(builder, stored_bits);
        let right = float_value(builder, key.bits);
        let equal = builder.ins().fcmp(FloatCC::Equal, left, right);
        let left_nan = builder.ins().fcmp(FloatCC::Unordered, left, left);
        let right_nan = builder.ins().fcmp(FloatCC::Unordered, right, right);
        let both_nan = builder.ins().band(left_nan, right_nan);
        builder.ins().bor(equal, both_nan)
    } else {
        builder.ins().icmp(IntCC::Equal, stored_bits, key.bits)
    };
    Ok(builder.ins().band(valid, equal))
}

fn emit_runtime_value_lookup(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    argument: NativeValue,
    exit: HeapExitEmission<'_>,
) -> Result<NativeValue, CompileError> {
    let lookup = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        lookup,
        &[
            values.runtime_context,
            reference,
            argument.bits,
            argument.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    Ok(NativeValue { bits, tag })
}

fn emit_value_equal(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    left: NativeValue,
    right: NativeValue,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let matching_tags = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    builder.append_block_param(done, types::I64);
    let same_tag = builder.ins().icmp(IntCC::Equal, left.tag, right.tag);
    let zero = builder.ins().iconst(types::I64, 0);
    builder
        .ins()
        .brif(same_tag, matching_tags, &[], done, &[zero.into()]);

    builder.switch_to_block(matching_tags);
    let is_object = builder
        .ins()
        .icmp_imm(IntCC::Equal, left.tag, ValueTag::Obj as u64 as i64);
    let mut is_simple =
        builder
            .ins()
            .icmp_imm(IntCC::Equal, left.tag, ValueTag::Unit as u64 as i64);
    for tag in [
        ValueTag::Bool,
        ValueTag::Int,
        ValueTag::Char,
        ValueTag::Op,
        ValueTag::EmptyCase,
    ] {
        let matches = builder
            .ins()
            .icmp_imm(IntCC::Equal, left.tag, tag as u64 as i64);
        is_simple = builder.ins().bor(is_simple, matches);
    }
    let same_bits = builder.ins().icmp(IntCC::Equal, left.bits, right.bits);
    let same_object = builder.ins().band(is_object, same_bits);
    let fast = builder.ins().bor(same_object, is_simple);
    let fast_result = builder.ins().uextend(types::I64, same_bits);
    builder
        .ins()
        .brif(fast, done, &[fast_result.into()], slow, &[]);

    builder.switch_to_block(slow);
    let equal = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, value_equal),
    )?;
    let call = builder.ins().call_indirect(
        values.value_equal_signature,
        equal,
        &[
            values.runtime_context,
            left.bits,
            left.tag,
            right.bits,
            right.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    let result = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    builder.ins().jump(done, &[result.into()]);

    builder.switch_to_block(done);
    Ok(builder.block_params(done)[0])
}

fn emit_typed_object_binary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    left: ir::Value,
    right: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_binary_signature,
        function,
        &[
            values.runtime_context,
            left,
            right,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

fn emit_typed_object_unary(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    function_offset: usize,
    reference: ir::Value,
    exit: HeapExitEmission<'_>,
) -> Result<ir::Value, CompileError> {
    let function = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        function_offset,
    )?;
    let call = builder.ins().call_indirect(
        values.object_unary_signature,
        function,
        &[
            values.runtime_context,
            reference,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    ))
}

#[allow(clippy::too_many_arguments)]
fn emit_map_put(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    key_contract: ValueContract,
    stored: NativeValue,
    option_family: Option<ir::Value>,
    previous_contract: ValueContract,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<Option<NativeValue>, CompileError> {
    let Some(key_kind) = direct_map_key_kind(key_contract) else {
        return emit_map_put_slow(
            builder,
            values,
            reference,
            key,
            stored,
            option_family,
            previous_contract,
            roots,
            exit,
        );
    };
    let map_entry = emit_object_entry(
        builder,
        values,
        reference,
        JIT_OBJECT_MAP,
        exit.point,
        ObjectGuard::Replay(exit.deopt_stack),
    )?;
    emit_mutable_guard(builder, values, map_entry, exit)?;
    let entry_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_LEN_OFFSET,
    )?;
    let built = load_heap_value(builder, types::I32, map_entry, JIT_MAP_INDEX_BUILT_OFFSET)?;
    let built = builder.ins().uextend(values.pointer_type, built);
    let index_ready = builder.ins().icmp(IntCC::Equal, built, entry_count);
    let direct = builder.create_block();
    let slow = builder.create_block();
    let done = builder.create_block();
    if option_family.is_some() {
        builder.append_block_param(done, types::I64);
        builder.append_block_param(done, types::I64);
    }
    builder.ins().brif(index_ready, direct, &[], slow, &[]);

    builder.switch_to_block(direct);
    let direct_key = emit_direct_map_key(builder, values, key, key_kind, exit)?;
    let probe_start = builder.create_block();
    builder
        .ins()
        .brif(direct_key.ready, probe_start, &[], slow, &[]);

    builder.switch_to_block(probe_start);
    let probe = emit_direct_map_probe(
        builder,
        values,
        map_entry,
        entry_count,
        key,
        direct_key,
        exit,
    )?;
    let replace = builder.create_block();
    let insert = builder.create_block();
    builder.ins().brif(probe.found, replace, &[], insert, &[]);

    builder.switch_to_block(replace);
    emit_native_value_contract(
        builder,
        values,
        probe.value,
        previous_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    let value_address = builder.ins().iadd_imm(
        probe.entry,
        i64::try_from(MAP_ENTRY_VALUE_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, value_address, stored, previous_contract.kind)?;
    if option_family.is_some() {
        builder
            .ins()
            .jump(done, &[probe.value.bits.into(), probe.value.tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(insert);
    let entry_capacity = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_CAPACITY_OFFSET,
    )?;
    let has_entry_capacity =
        builder
            .ins()
            .icmp(IntCC::UnsignedLessThan, entry_count, entry_capacity);
    let max_entry_count = builder
        .ins()
        .iconst(values.pointer_type, i64::from(u32::MAX));
    let count_fits = builder
        .ins()
        .icmp(IntCC::UnsignedLessThan, entry_count, max_entry_count);
    let next_count = builder.ins().iadd_imm(entry_count, 1);

    let live = load_heap_value(builder, types::I32, map_entry, JIT_MAP_LIVE_OFFSET)?;
    let live_native = builder.ins().uextend(values.pointer_type, live);
    let live_valid = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, live_native, entry_count);
    let live_fits = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, live, i64::from(u32::MAX));
    let next_live = builder.ins().iadd_imm(live, 1);

    let slot_count = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_INDEX_SLOTS_LEN_OFFSET,
    )?;
    let has_vacant_slot = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, probe.vacant_slot, 0);
    let count_i64 = if values.pointer_type == types::I64 {
        entry_count
    } else {
        builder.ins().uextend(types::I64, entry_count)
    };
    let slots_i64 = if values.pointer_type == types::I64 {
        slot_count
    } else {
        builder.ins().uextend(types::I64, slot_count)
    };
    let required_slots = builder.ins().iadd_imm(count_i64, 1);
    let required_slots = builder.ins().imul_imm(required_slots, 3);
    let available_slots = builder.ins().imul_imm(slots_i64, 2);
    let load_factor_ready = builder.ins().icmp(
        IntCC::UnsignedLessThanOrEqual,
        required_slots,
        available_slots,
    );

    let epoch = load_heap_value(builder, types::I32, map_entry, JIT_MAP_EPOCH_OFFSET)?;
    let epoch_ready = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, epoch, i64::from(u32::MAX));
    let epoch_tracked = builder.ins().icmp_imm(IntCC::NotEqual, epoch, 0);
    let incremented_epoch = builder.ins().iadd_imm(epoch, 1);
    let next_epoch = builder
        .ins()
        .select(epoch_tracked, incremented_epoch, epoch);

    let object_bytes = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_ENTRY_BYTES_OFFSET,
    )?;
    let next_object_bytes = builder
        .ins()
        .iadd_imm(object_bytes, JIT_MAP_ENTRY_COST as i64);
    let object_charge_ready = builder.ins().icmp(
        IntCC::UnsignedGreaterThanOrEqual,
        next_object_bytes,
        object_bytes,
    );
    let used_pointer = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let used = load_value(builder, values.pointer_type, used_pointer, 0)?;
    let next_used = builder.ins().iadd_imm(used, JIT_MAP_ENTRY_COST as i64);
    let heap_charge_ready = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, next_used, used);
    let threshold = load_vmctx_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_collection_threshold),
    )?;
    let below_threshold = builder
        .ins()
        .icmp(IntCC::UnsignedLessThanOrEqual, next_used, threshold);

    let mut fast = builder.ins().band(has_entry_capacity, count_fits);
    fast = builder.ins().band(fast, live_valid);
    fast = builder.ins().band(fast, live_fits);
    fast = builder.ins().band(fast, has_vacant_slot);
    fast = builder.ins().band(fast, load_factor_ready);
    fast = builder.ins().band(fast, epoch_ready);
    fast = builder.ins().band(fast, object_charge_ready);
    fast = builder.ins().band(fast, heap_charge_ready);
    fast = builder.ins().band(fast, below_threshold);
    let commit = builder.create_block();
    builder.ins().brif(fast, commit, &[], slow, &[]);

    builder.switch_to_block(commit);
    let entries = load_heap_value(
        builder,
        values.pointer_type,
        map_entry,
        JIT_MAP_ENTRIES_DATA_OFFSET,
    )?;
    let entry_offset = builder.ins().imul_imm(
        entry_count,
        i64::try_from(MAP_ENTRY_SIZE).map_err(|_| CompileError::Backend)?,
    );
    let entry = builder.ins().iadd(entries, entry_offset);
    let key_address = builder.ins().iadd_imm(
        entry,
        i64::try_from(MAP_ENTRY_KEY_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, key_address, key, key_contract.kind)?;
    let value_address = builder.ins().iadd_imm(
        entry,
        i64::try_from(MAP_ENTRY_VALUE_OFFSET).map_err(|_| CompileError::Backend)?,
    );
    emit_store_value(builder, value_address, stored, previous_contract.kind)?;
    store_heap_value(
        builder,
        entry,
        MAP_ENTRY_SEMANTIC_HASH_OFFSET,
        direct_key.semantic_hash,
    )?;
    store_heap_value(
        builder,
        probe.vacant_slot,
        MAP_SLOT_HASH_OFFSET,
        direct_key.lookup_hash,
    )?;
    let entry_index = builder.ins().ireduce(types::I32, entry_count);
    store_heap_value(
        builder,
        probe.vacant_slot,
        MAP_SLOT_ENTRY_OFFSET,
        entry_index,
    )?;
    store_heap_value(builder, map_entry, JIT_MAP_EPOCH_OFFSET, next_epoch)?;
    store_heap_value(
        builder,
        map_entry,
        JIT_ENTRY_BYTES_OFFSET,
        next_object_bytes,
    )?;
    store_heap_value(builder, used_pointer, 0, next_used)?;
    store_heap_value(builder, map_entry, JIT_MAP_ENTRIES_LEN_OFFSET, next_count)?;
    store_heap_value(builder, map_entry, JIT_MAP_LIVE_OFFSET, next_live)?;
    let next_built = builder.ins().ireduce(types::I32, next_count);
    store_heap_value(builder, map_entry, JIT_MAP_INDEX_BUILT_OFFSET, next_built)?;
    if let Some(option_family) = option_family {
        let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
        let bits = builder.ins().bor(option_family, none_arm);
        let tag = builder
            .ins()
            .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
        builder.ins().jump(done, &[bits.into(), tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(slow);
    let result = emit_map_put_slow(
        builder,
        values,
        reference,
        key,
        stored,
        option_family,
        previous_contract,
        roots,
        exit,
    )?;
    if let Some(result) = result {
        builder
            .ins()
            .jump(done, &[result.bits.into(), result.tag.into()]);
    } else {
        builder.ins().jump(done, &[]);
    }

    builder.switch_to_block(done);
    Ok(option_family.map(|_| NativeValue {
        bits: builder.block_params(done)[0],
        tag: builder.block_params(done)[1],
    }))
}

#[allow(clippy::too_many_arguments)]
fn emit_map_put_slow(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    reference: ir::Value,
    key: NativeValue,
    stored: NativeValue,
    option_family: Option<ir::Value>,
    previous_contract: ValueContract,
    roots: &[NativeRoot],
    exit: HeapExitEmission<'_>,
) -> Result<Option<NativeValue>, CompileError> {
    let Some(option_family) = option_family else {
        let root_count = emit_runtime_roots(builder, values, roots)?;
        let discard = load_value(
            builder,
            values.pointer_type,
            values.runtime_functions,
            mem::offset_of!(RawNativeFunctions, map_put_discard),
        )?;
        let call = builder.ins().call_indirect(
            values.map_put_discard_signature,
            discard,
            &[
                values.runtime_context,
                reference,
                key.bits,
                key.tag,
                stored.bits,
                stored.tag,
                root_count,
            ],
        );
        let status = builder.inst_results(call)[0];
        emit_runtime_status(
            builder,
            values,
            status,
            exit.point,
            exit.fault_stack,
            exit.deopt_stack,
        )?;
        return Ok(None);
    };

    let probe = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, map_put_probe),
    )?;
    let call = builder.ins().call_indirect(
        values.map_lookup_signature,
        probe,
        &[
            values.runtime_context,
            reference,
            key.bits,
            key.tag,
            values.allocation_result_pointer,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_fault_status(builder, values, status, exit.point, exit.fault_stack)?;
    let existing = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_OK));
    let vacant = builder
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(RUNTIME_MAP_VACANT));
    let valid = builder.ins().bor(existing, vacant);
    let invalid = builder.ins().bxor_imm(valid, 1);
    emit_interpreter_replay(builder, values, invalid, exit.point, exit.deopt_stack)?;

    let token = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        16,
    );
    let entry_count = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        24,
    );
    let existing_block = builder.create_block();
    let vacant_block = builder.create_block();
    let ready = builder.create_block();
    builder.append_block_param(ready, types::I64);
    builder.append_block_param(ready, types::I64);
    builder
        .ins()
        .brif(vacant, vacant_block, &[], existing_block, &[]);

    builder.switch_to_block(existing_block);
    let bits = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        0,
    );
    let tag = builder.ins().load(
        types::I64,
        MemFlags::new(),
        values.allocation_result_pointer,
        8,
    );
    let previous = NativeValue { bits, tag };
    emit_native_value_contract(
        builder,
        values,
        previous,
        previous_contract,
        exit.point,
        exit.deopt_stack,
    )?;
    builder
        .ins()
        .jump(ready, &[previous.bits.into(), previous.tag.into()]);

    builder.switch_to_block(vacant_block);
    let none_arm = builder.ins().iconst(types::I64, 1_i64 << 32);
    let bits = builder.ins().bor(option_family, none_arm);
    let tag = builder
        .ins()
        .iconst(types::I64, ValueTag::EmptyCase as u64 as i64);
    builder.ins().jump(ready, &[bits.into(), tag.into()]);

    builder.switch_to_block(ready);
    let result = NativeValue {
        bits: builder.block_params(ready)[0],
        tag: builder.block_params(ready)[1],
    };

    let root_count = emit_runtime_roots(builder, values, roots)?;
    let commit = load_value(
        builder,
        values.pointer_type,
        values.runtime_functions,
        mem::offset_of!(RawNativeFunctions, map_put_commit),
    )?;
    let zero = builder.ins().iconst(types::I32, 0);
    let one = builder.ins().iconst(types::I32, 1);
    let vacant = builder.ins().select(vacant, one, zero);
    let call = builder.ins().call_indirect(
        values.map_put_commit_signature,
        commit,
        &[
            values.runtime_context,
            reference,
            key.bits,
            key.tag,
            stored.bits,
            stored.tag,
            token,
            entry_count,
            vacant,
            root_count,
        ],
    );
    let status = builder.inst_results(call)[0];
    emit_runtime_status(
        builder,
        values,
        status,
        exit.point,
        exit.fault_stack,
        exit.deopt_stack,
    )?;
    Ok(Some(result))
}

fn emit_runtime_status(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    status: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
    replay_stack: &[NativeValue],
) -> Result<(), CompileError> {
    emit_runtime_fault_status(builder, values, status, point, fault_stack)?;
    let replay = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, status, i64::from(RUNTIME_OK));
    emit_interpreter_replay(builder, values, replay, point, replay_stack)
}

fn emit_runtime_fault_status(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    status: ir::Value,
    point: FaultPoint,
    fault_stack: &[NativeValue],
) -> Result<(), CompileError> {
    let fault = builder
        .ins()
        .band_imm(status, i64::from(RUNTIME_FAULT_FLAG));
    let fault = builder.ins().icmp_imm(IntCC::NotEqual, fault, 0);
    let fault_block = builder.create_block();
    let checked = builder.create_block();
    builder.ins().brif(fault, fault_block, &[], checked, &[]);

    builder.switch_to_block(fault_block);
    let retired = emit_retired_with_prefix(builder, values, point.prefix);
    let code = builder
        .ins()
        .band_imm(status, i64::from(!RUNTIME_FAULT_FLAG));
    let code = builder.ins().uextend(types::I64, code);
    let zero = builder.ins().iconst(types::I64, 0);
    emit_exit(
        builder,
        values,
        ExitEmission {
            retired,
            kind: EXIT_GUEST_FAULT,
            block: point.block,
            instruction: point.instruction,
            result: NativeValue {
                bits: code,
                tag: zero,
            },
        },
        fault_stack,
    )?;

    builder.switch_to_block(checked);
    Ok(())
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
    _point: FaultPoint,
    _stack: &[NativeValue],
) -> Result<(), CompileError> {
    let replay_block = values.replay_blocks.first().ok_or(CompileError::Backend)?;
    replay_block.used.set(true);
    let success = builder.create_block();
    builder
        .ins()
        .brif(replay, replay_block.block, &[], success, &[]);
    builder.switch_to_block(success);
    Ok(())
}

fn emit_pending_instance_barrier(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    point: FaultPoint,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let available = load_value(
        builder,
        types::I64,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_available),
    )?;
    let pending = builder.ins().icmp_imm(IntCC::NotEqual, available, -1);
    emit_interpreter_replay(builder, values, pending, point, stack)
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
    let locals = capture_local_values(builder, values)?;
    emit_exit_with_locals(builder, values, exit, &locals, stack)
}

fn emit_exit_with_locals(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let kind = builder.ins().iconst(types::I32, i64::from(exit.kind));
    emit_exit_with_locals_and_kind(builder, values, exit, kind, locals, stack)
}

fn emit_exit_with_locals_and_kind(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    exit: ExitEmission,
    kind: ir::Value,
    locals: &[NativeValue],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if exit.kind == EXIT_RETURN {
        emit_release_scalar_charges(builder, values)?;
    } else {
        emit_scalar_deopt_records(builder, values)?;
    }
    let storage = reload_active_frame_storage(builder, values)?;
    let stack_kinds = crate::decode_exit_kind(exit.kind)
        .and_then(|kind| {
            values
                .plan
                .materialization_operand_kinds(kind, exit.block, exit.instruction)
        })
        .filter(|kinds| kinds.len() == stack.len());
    emit_spill_frame_values(
        builder,
        storage,
        exit.block,
        exit.instruction,
        locals,
        stack,
        stack_kinds,
    )?;
    store_i64(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, retired),
        exit.retired,
    )?;
    store_i32_value(
        builder,
        values.exit_pointer,
        mem::offset_of!(RawExit, kind),
        kind,
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

fn emit_scalar_deopt_records(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<(), CompileError> {
    if values.scalar_instances.is_empty() {
        return Ok(());
    }
    let records = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_instances),
    )?;
    let field_values = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, virtual_values),
    )?;
    for (site, (plan, instance)) in values
        .plan
        .scalar_instances
        .iter()
        .zip(values.scalar_instances)
        .enumerate()
    {
        if plan.field_count as usize != instance.fields.len() {
            return Err(CompileError::Backend);
        }
        let active = builder.use_var(instance.active);
        let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
        let write = builder.create_block();
        let next = builder.create_block();
        builder.ins().brif(active, write, &[], next, &[]);

        builder.switch_to_block(write);
        let record_offset = site
            .checked_mul(mem::size_of::<RawVirtualInstance>())
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(CompileError::Backend)?;
        let record = builder.ins().iadd_imm(records, record_offset);
        let values_offset = site
            .checked_mul(VIRTUAL_INSTANCE_FIELDS)
            .and_then(|offset| offset.checked_mul(VALUE_SIZE))
            .and_then(|offset| i64::try_from(offset).ok())
            .ok_or(CompileError::Backend)?;
        let fields = builder.ins().iadd_imm(field_values, values_offset);
        for (field, value) in instance.fields.iter().enumerate() {
            let offset = field.checked_mul(VALUE_SIZE).ok_or(CompileError::Backend)?;
            let bits = builder.use_var(value.bits);
            let tag = builder.use_var(value.tag);
            store_i64(builder, fields, offset + VALUE_PAYLOAD_OFFSET, bits)?;
            store_i64(builder, fields, offset + VALUE_TAG_OFFSET, tag)?;
        }
        let one = builder.ins().iconst(types::I32, 1);
        let zero = builder.ins().iconst(types::I32, 0);
        let token = builder.ins().iconst(types::I64, instance.token as i64);
        let class = builder.ins().iconst(types::I32, i64::from(plan.class));
        let field_count = builder
            .ins()
            .iconst(types::I32, i64::from(plan.field_count));
        let frozen = if plan.frozen { one } else { zero };
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, references),
            one,
        )?;
        store_i64(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, object_bits),
            token,
        )?;
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, class),
            class,
        )?;
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, environment),
            zero,
        )?;
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, field_count),
            field_count,
        )?;
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, frozen),
            frozen,
        )?;
        store_i32_value(
            builder,
            record,
            mem::offset_of!(RawVirtualInstance, active),
            one,
        )?;
        builder.ins().jump(next, &[]);
        builder.switch_to_block(next);
    }
    Ok(())
}

fn emit_release_scalar_charges(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
) -> Result<(), CompileError> {
    if values.scalar_instances.is_empty() {
        return Ok(());
    }
    let used_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_used_bytes),
    )?;
    let live_pointer = load_value(
        builder,
        values.pointer_type,
        values.activation_pointer,
        mem::offset_of!(RawNativeActivation, heap_live),
    )?;
    for instance in values.scalar_instances {
        let active = builder.use_var(instance.active);
        let active = builder.ins().icmp_imm(IntCC::NotEqual, active, 0);
        let release = builder.create_block();
        let next = builder.create_block();
        builder.ins().brif(active, release, &[], next, &[]);

        builder.switch_to_block(release);
        let used = load_heap_value(builder, values.pointer_type, used_pointer, 0)?;
        let cost = scalar_instance_cost(instance.fields.len())?;
        let used = builder.ins().iadd_imm(used, -cost);
        store_heap_value(builder, used_pointer, 0, used)?;
        let live = load_heap_value(builder, values.pointer_type, live_pointer, 0)?;
        let live = builder.ins().iadd_imm(live, -1);
        store_heap_value(builder, live_pointer, 0, live)?;
        builder.ins().jump(next, &[]);
        builder.switch_to_block(next);
    }
    Ok(())
}

fn reload_active_frame_storage<'a>(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'a>,
) -> Result<NativeValues<'a>, CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    let scalar_base = load_cell_u32(builder, frame, mem::offset_of!(RawNativeFrame, scalar_base))?;
    let scalar_base = builder.ins().uextend(values.pointer_type, scalar_base);
    let scalar_offset = builder.ins().ishl_imm(scalar_base, 3);
    let scalars = load_activation_pointer(builder, values, RawActivationField::Scalars)?;
    let tags = load_activation_pointer(builder, values, RawActivationField::Tags)?;
    let states = load_activation_pointer(builder, values, RawActivationField::States)?;
    let local_pointer = builder.ins().iadd(scalars, scalar_offset);
    let local_tag_pointer = builder.ins().iadd(tags, scalar_offset);
    let local_state_pointer = builder.ins().iadd(states, scalar_base);
    let local_bytes = i64::try_from(
        values
            .locals
            .len()
            .checked_mul(8)
            .ok_or(CompileError::Backend)?,
    )
    .map_err(|_| CompileError::Backend)?;
    let stack_pointer = builder.ins().iadd_imm(local_pointer, local_bytes);
    let stack_tag_pointer = builder.ins().iadd_imm(local_tag_pointer, local_bytes);
    Ok(NativeValues {
        local_pointer,
        local_tag_pointer,
        local_state_pointer,
        stack_pointer,
        stack_tag_pointer,
        ..values
    })
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
    let frame_len = load_activation_u32(builder, values, RawActivationField::FrameLen)?;
    let has_parent = builder
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, frame_len, 1);
    builder.ins().brif(has_parent, direct, &[], normal, &[]);

    builder.switch_to_block(direct);
    let retired = emit_retired(builder, values);
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

    builder.switch_to_block(normal);
    let retired = emit_retired(builder, values);
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

fn emit_spill_frame_values(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    block: u32,
    instruction: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
    stack_kinds: Option<&[ScalarKind]>,
) -> Result<(), CompileError> {
    let frame = emit_current_frame_pointer(builder, values)?;
    emit_spill_frame_values_to(
        builder,
        values,
        frame,
        block,
        instruction,
        locals,
        stack,
        stack_kinds,
    )
}

fn emit_spill_frame_roots(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    local_kinds: &[ScalarKind],
    stack_kinds: &[ScalarKind],
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    if local_kinds.len() != values.locals.len() || stack_kinds.len() != stack.len() {
        return Err(CompileError::Backend);
    }
    // Keep every scanned tag canonical when a later call reuses this frame window.
    for (slot, kind) in local_kinds.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        let tag = emit_slot_tag(builder, values.local_tags[slot], kind)?;
        builder
            .ins()
            .store(MemFlags::new(), tag, values.local_tag_pointer, offset);
        if is_root_kind(kind) {
            let bits = builder.use_var(values.locals[slot]);
            builder
                .ins()
                .store(MemFlags::new(), bits, values.local_pointer, offset);
        }
    }
    for (slot, (kind, value)) in stack_kinds
        .iter()
        .copied()
        .zip(stack.iter().copied())
        .enumerate()
    {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value.tag, values.stack_tag_pointer, offset);
        if is_root_kind(kind) {
            builder
                .ins()
                .store(MemFlags::new(), value.bits, values.stack_pointer, offset);
        }
    }
    store_i32_constant(
        builder,
        frame,
        mem::offset_of!(RawNativeFrame, operand_len),
        u32::try_from(stack.len()).map_err(|_| CompileError::Backend)?,
    )
}

fn emit_spill_frame_to(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    block: u32,
    instruction: u32,
    stack: &[NativeValue],
) -> Result<(), CompileError> {
    let locals = capture_local_values(builder, values)?;
    let stack_kinds = values
        .plan
        .suspended_operand_kinds(block, instruction)
        .filter(|kinds| kinds.len() == stack.len());
    emit_spill_frame_values_to(
        builder,
        values,
        frame,
        block,
        instruction,
        &locals,
        stack,
        stack_kinds,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_spill_frame_values_to(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    frame: ir::Value,
    block: u32,
    instruction: u32,
    locals: &[NativeValue],
    stack: &[NativeValue],
    stack_kinds: Option<&[ScalarKind]>,
) -> Result<(), CompileError> {
    if locals.len() != values.locals.len()
        || values
            .dirty_locals
            .is_some_and(|dirty_locals| dirty_locals.len() != locals.len())
    {
        return Err(CompileError::Backend);
    }
    for (slot, (kind, value)) in values
        .local_kinds
        .iter()
        .copied()
        .zip(locals.iter().copied())
        .enumerate()
    {
        if values
            .dirty_locals
            .is_some_and(|dirty_locals| !dirty_locals[slot])
        {
            continue;
        }
        let local_offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder.ins().store(
            MemFlags::new(),
            value.bits,
            values.local_pointer,
            local_offset,
        );
        if value_tag(kind).is_none() {
            builder.ins().store(
                MemFlags::new(),
                value.tag,
                values.local_tag_pointer,
                local_offset,
            );
        }
    }
    for (slot, value) in stack.iter().copied().enumerate() {
        let offset = i32::try_from(slot.checked_mul(8).ok_or(CompileError::Backend)?)
            .map_err(|_| CompileError::Backend)?;
        builder
            .ins()
            .store(MemFlags::new(), value.bits, values.stack_pointer, offset);
        if stack_kinds
            .and_then(|kinds| kinds.get(slot).copied())
            .and_then(value_tag)
            .is_none()
        {
            builder
                .ins()
                .store(MemFlags::new(), value.tag, values.stack_tag_pointer, offset);
        }
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
    for (slot, (variable, value)) in values
        .stack
        .iter()
        .copied()
        .zip(stack.iter().copied())
        .enumerate()
    {
        builder.def_var(variable, value.bits);
        if let Some(tag) = values.stack_tags[slot] {
            builder.def_var(tag, value.tag);
        }
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
    MaxStackValues,
    BaseFrames,
    MaxFrames,
    RootCapacity,
    LiteralValues,
    LiteralCount,
    PollRequested,
    HardFuel,
    PollDeadline,
    PollInterval,
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
            RawActivationField::MaxStackValues => {
                mem::offset_of!(RawNativeActivation, max_stack_values)
            }
            RawActivationField::BaseFrames => {
                mem::offset_of!(RawNativeActivation, base_frames)
            }
            RawActivationField::MaxFrames => mem::offset_of!(RawNativeActivation, max_frames),
            RawActivationField::RootCapacity => {
                mem::offset_of!(RawNativeActivation, root_capacity)
            }
            RawActivationField::LiteralValues => {
                mem::offset_of!(RawNativeActivation, literal_values)
            }
            RawActivationField::LiteralCount => {
                mem::offset_of!(RawNativeActivation, literal_count)
            }
            RawActivationField::PollRequested => {
                mem::offset_of!(RawNativeActivation, poll_requested)
            }
            RawActivationField::HardFuel => mem::offset_of!(RawNativeActivation, hard_fuel),
            RawActivationField::PollDeadline => {
                mem::offset_of!(RawNativeActivation, poll_deadline)
            }
            RawActivationField::PollInterval => {
                mem::offset_of!(RawNativeActivation, poll_interval)
            }
        }
    }

    fn immutable(self) -> bool {
        !matches!(
            self,
            RawActivationField::ScalarLen
                | RawActivationField::FrameLen
                | RawActivationField::ChangedFrom
                | RawActivationField::PollDeadline
        )
    }
}

fn load_activation_u32(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
) -> Result<ir::Value, CompileError> {
    let flags = if field.immutable() {
        immutable_vmctx_mem_flags()
    } else {
        vmctx_mem_flags()
    };
    load_value_with_flags(
        builder,
        types::I32,
        values.activation_pointer,
        field.offset(),
        flags,
    )
}

fn load_activation_u64(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
) -> Result<ir::Value, CompileError> {
    let flags = if field.immutable() {
        immutable_vmctx_mem_flags()
    } else {
        vmctx_mem_flags()
    };
    load_value_with_flags(
        builder,
        types::I64,
        values.activation_pointer,
        field.offset(),
        flags,
    )
}

fn load_activation_pointer(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
) -> Result<ir::Value, CompileError> {
    let flags = if field.immutable() {
        immutable_vmctx_mem_flags()
    } else {
        vmctx_mem_flags()
    };
    load_value_with_flags(
        builder,
        values.pointer_type,
        values.activation_pointer,
        field.offset(),
        flags,
    )
}

fn store_activation_u32(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
    value: ir::Value,
) -> Result<(), CompileError> {
    store_i32_value_with_flags(
        builder,
        values.activation_pointer,
        field.offset(),
        value,
        vmctx_mem_flags(),
    )
}

fn store_activation_u64(
    builder: &mut FunctionBuilder<'_>,
    values: NativeValues<'_>,
    field: RawActivationField,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(field.offset()).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(vmctx_mem_flags(), value, values.activation_pointer, offset);
    Ok(())
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

fn store_native_value(
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
    load_value_with_flags(builder, ty, pointer, offset, MemFlags::new())
}

fn load_heap_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(builder, ty, pointer, offset, heap_mem_flags())
}

fn load_immutable_heap_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(
        builder,
        ty,
        pointer,
        offset,
        heap_mem_flags().with_readonly().with_can_move(),
    )
}

fn load_vmctx_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(builder, ty, pointer, offset, vmctx_mem_flags())
}

fn load_value_with_flags(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
    flags: MemFlags,
) -> Result<ir::Value, CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    Ok(builder.ins().load(ty, flags, pointer, offset))
}

fn store_i32_value_with_flags(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
    flags: MemFlags,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(flags, value, pointer, offset);
    Ok(())
}

const fn vmctx_mem_flags() -> MemFlags {
    MemFlags::trusted().with_alias_region(Some(AliasRegion::Vmctx))
}

const fn immutable_vmctx_mem_flags() -> MemFlags {
    vmctx_mem_flags().with_readonly().with_can_move()
}

const fn heap_mem_flags() -> MemFlags {
    MemFlags::trusted().with_alias_region(Some(AliasRegion::Heap))
}

const fn table_mem_flags() -> MemFlags {
    MemFlags::trusted()
        .with_readonly()
        .with_alias_region(Some(AliasRegion::Table))
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

fn store_heap_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder
        .ins()
        .store(heap_mem_flags(), value, pointer, offset);
    Ok(())
}
