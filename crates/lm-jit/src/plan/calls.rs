//! Call, constructor, and scalar representation planning.

use super::*;

pub(super) fn call_contracts(
    input: &FunctionInput<'_>,
) -> Result<HashMap<u32, CallSignature>, UnsupportedReason> {
    let mut contracts = HashMap::new();
    let definitions = std::iter::once(input.root).chain(input.direct_callees.iter().copied());
    for definition in definitions {
        let source_func = definition
            .source
            .funcs
            .get(definition.source_function as usize)
            .ok_or(UnsupportedReason::MissingSource)?;
        if source_func.params.len() != definition.runtime.params.len()
            || source_func.local_types.len() != definition.runtime.local_types.len()
        {
            return Err(UnsupportedReason::InvalidControlFlow);
        }
        let params = source_func
            .params
            .iter()
            .map(|ty| call_value_kind(definition.source, *ty))
            .collect::<Result<Vec<_>, _>>()?;
        let result = call_value_kind(definition.source, source_func.ret)?;
        contracts.insert(
            definition.function,
            CallSignature {
                virtual_params: virtual_parameters(definition),
                params,
                local_count: source_func.local_types.len(),
                result,
                virtual_constructor: virtual_constructor(definition),
            },
        );
    }
    Ok(contracts)
}

pub(super) fn virtual_parameters(definition: FunctionDefinition<'_>) -> Vec<bool> {
    let mut parameters = vec![false; definition.runtime.params.len()];
    if source_accepts_virtual_receiver(definition.source, definition.source_function)
        && !parameters.is_empty()
    {
        parameters[0] = true;
    }
    parameters
}

pub(super) fn source_accepts_virtual_receiver(source: &Module, function: u32) -> bool {
    source.bindings.iter().any(|binding| {
        if binding.func != function || binding.class != lm_bytecode::NO_CLASS {
            return false;
        }
        let Some(class_key) = binding.key.strip_suffix(".init") else {
            return false;
        };
        source
            .classes
            .iter()
            .any(|class| class.key == class_key && class.has_init)
    })
}

pub(super) fn virtual_constructor(
    definition: FunctionDefinition<'_>,
) -> Option<VirtualConstructor> {
    let binding = definition.source.bindings.iter().find(|binding| {
        binding.func == definition.source_function && binding.class != lm_bytecode::NO_CLASS
    })?;
    let source_class = definition.source.classes.get(binding.class as usize)?;
    let field_count = u32::try_from(source_class.fields.len()).ok()?;
    if field_count == 0 || field_count as usize > crate::activation::VIRTUAL_INSTANCE_FIELDS {
        return None;
    }
    let class = relocate_class(binding.class, definition.class_relocation).ok()?;
    let function = definition.runtime;
    let source_function = definition
        .source
        .funcs
        .get(definition.source_function as usize)?;
    let object_local = u32::try_from(function.params.len()).ok()?;
    if function.local_types.len() != function.params.len().checked_add(1)? {
        return None;
    }
    let [block] = function.blocks.as_slice() else {
        return None;
    };
    let [source_block] = source_function.blocks.as_slice() else {
        return None;
    };
    if source_block.len() != block.len() {
        return None;
    }
    for (instruction, source_instruction) in block.iter().zip(source_block) {
        match instruction {
            Instr::Call(_) | Instr::CallG { .. } => {
                let target = match source_instruction {
                    Instr::Call(target) | Instr::CallG { func: target, .. } => *target,
                    _ => return None,
                };
                if !source_accepts_virtual_receiver(definition.source, target) {
                    return None;
                }
            }
            instruction
                if matches!(
                    crate::instruction_treatment(instruction).class(),
                    crate::TreatmentClass::Call
                ) =>
            {
                return None;
            }
            _ => {}
        }
    }
    let creates_class = match block.first()? {
        Instr::New(created) | Instr::NewG { class: created, .. } => *created == class,
        _ => false,
    };
    if !creates_class {
        return None;
    }
    if block.get(1) != Some(&Instr::StoreLocal(object_local)) {
        return None;
    }
    if block.last() != Some(&Instr::Return) {
        return None;
    }
    let returns_object = block.get(block.len().checked_sub(2)?);
    let returns_object = if returns_object == Some(&Instr::Extended(ExtendedInstr::SealInstance)) {
        block.get(block.len().checked_sub(3)?)
    } else {
        returns_object
    };
    if returns_object != Some(&Instr::LoadLocal(object_local)) {
        return None;
    }
    Some(VirtualConstructor {
        class,
        field_count,
        object_local,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConstructorSymbol {
    Unknown,
    Parameter(u32),
    Constant(ScalarConstant),
    Object,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ConstructorExecution {
    result: ConstructorSymbol,
    retired_cost: u32,
    frame_count: u32,
    stack_values: u32,
}

pub(super) fn scalar_constructor_summary(
    input: &FunctionInput<'_>,
    target: u32,
) -> Result<Option<ScalarReplacement>, UnsupportedReason> {
    let Some(definition) = input.definition(target) else {
        return Ok(None);
    };
    let Some(constructor) = virtual_constructor(definition) else {
        return Ok(None);
    };
    let Some(binding) = definition.source.bindings.iter().find(|binding| {
        binding.func == definition.source_function && binding.class != lm_bytecode::NO_CLASS
    }) else {
        return Ok(None);
    };
    let Some(source_class) = definition.source.classes.get(binding.class as usize) else {
        return Ok(None);
    };
    if source_class.type_params != 0 {
        return Ok(None);
    }
    let arguments = (0..definition.runtime.params.len())
        .map(|index| {
            u32::try_from(index)
                .map(ConstructorSymbol::Parameter)
                .map_err(|_| UnsupportedReason::RegionLimit)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut fields = vec![ConstructorSymbol::Unknown; constructor.field_count as usize];
    let mut active = Vec::new();
    let Some(execution) = execute_scalar_constructor(
        input,
        definition,
        &arguments,
        &mut fields,
        &mut active,
        Some(constructor.class),
    )?
    else {
        return Ok(None);
    };
    if execution.result != ConstructorSymbol::Object {
        return Ok(None);
    }
    let fields = fields
        .into_iter()
        .map(|field| match field {
            ConstructorSymbol::Parameter(parameter) => Ok(ScalarFieldSource::Parameter(parameter)),
            ConstructorSymbol::Constant(value) => Ok(ScalarFieldSource::Constant(value)),
            ConstructorSymbol::Unknown | ConstructorSymbol::Object => {
                Err(UnsupportedReason::InvalidControlFlow)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(ScalarReplacement {
        site: 0,
        class: constructor.class,
        frozen: source_class.is_frozen,
        fields,
        retired_cost: execution.retired_cost,
        frame_count: execution.frame_count,
        stack_values: execution.stack_values,
    }))
}

pub(super) fn execute_scalar_constructor(
    input: &FunctionInput<'_>,
    definition: FunctionDefinition<'_>,
    arguments: &[ConstructorSymbol],
    fields: &mut [ConstructorSymbol],
    active: &mut Vec<u32>,
    new_class: Option<u32>,
) -> Result<Option<ConstructorExecution>, UnsupportedReason> {
    if active.contains(&definition.function) || active.len() >= 8 {
        return Ok(None);
    }
    let [code] = definition.runtime.blocks.as_slice() else {
        return Ok(None);
    };
    if arguments.len() != definition.runtime.params.len() {
        return Err(UnsupportedReason::InvalidControlFlow);
    }
    active.push(definition.function);
    let mut locals = vec![ConstructorSymbol::Unknown; definition.runtime.local_types.len()];
    locals[..arguments.len()].copy_from_slice(arguments);
    let mut stack = Vec::new();
    let mut max_stack = 0usize;
    let mut nested_cost = 0u32;
    let mut nested_frames = 0u32;
    let mut nested_values = 0u32;
    let mut returned = None;
    for instruction in code.iter().copied() {
        match instruction {
            Instr::ConstUnit => stack.push(constant_symbol(ValueTag::Unit, 0)),
            Instr::ConstBool(value) => {
                stack.push(constant_symbol(ValueTag::Bool, u64::from(value)))
            }
            Instr::ConstInt(value) => stack.push(constant_symbol(ValueTag::Int, value as u64)),
            Instr::ConstFloat(value) => stack.push(constant_symbol(ValueTag::Float, value)),
            Instr::ConstChar(value) => {
                stack.push(constant_symbol(ValueTag::Char, u64::from(value)))
            }
            Instr::LoadLocal(slot) => {
                let value = locals
                    .get(slot as usize)
                    .copied()
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                if value == ConstructorSymbol::Unknown {
                    active.pop();
                    return Ok(None);
                }
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
            Instr::New(class) if Some(class) == new_class => {
                stack.push(ConstructorSymbol::Object);
            }
            Instr::LoadField(field) => {
                if stack.pop() != Some(ConstructorSymbol::Object) {
                    active.pop();
                    return Ok(None);
                }
                let value = fields
                    .get(field as usize)
                    .copied()
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                if value == ConstructorSymbol::Unknown {
                    active.pop();
                    return Ok(None);
                }
                stack.push(value);
            }
            Instr::StoreField(field) => {
                let value = stack.pop().ok_or(UnsupportedReason::InvalidStack)?;
                if stack.pop() != Some(ConstructorSymbol::Object) {
                    active.pop();
                    return Ok(None);
                }
                let target = fields
                    .get_mut(field as usize)
                    .ok_or(UnsupportedReason::InvalidControlFlow)?;
                *target = value;
            }
            Instr::Extended(ExtendedInstr::SealInstance) => {
                if stack.last() != Some(&ConstructorSymbol::Object) {
                    active.pop();
                    return Ok(None);
                }
            }
            Instr::Call(target) => {
                let Some(callee) = input.definition(target) else {
                    active.pop();
                    return Ok(None);
                };
                if !source_accepts_virtual_receiver(callee.source, callee.source_function) {
                    active.pop();
                    return Ok(None);
                }
                let count = callee.runtime.params.len();
                if stack.len() < count {
                    return Err(UnsupportedReason::InvalidStack);
                }
                let call_arguments = stack.split_off(stack.len() - count);
                let Some(execution) = execute_scalar_constructor(
                    input,
                    callee,
                    &call_arguments,
                    fields,
                    active,
                    None,
                )?
                else {
                    active.pop();
                    return Ok(None);
                };
                nested_cost = nested_cost
                    .checked_add(execution.retired_cost)
                    .ok_or(UnsupportedReason::RegionLimit)?;
                nested_frames = nested_frames.max(execution.frame_count);
                nested_values = nested_values.max(execution.stack_values);
                stack.push(execution.result);
            }
            Instr::Return => {
                returned = stack.pop();
                break;
            }
            _ => {
                active.pop();
                return Ok(None);
            }
        }
        max_stack = max_stack.max(stack.len());
    }
    active.pop();
    let Some(result) = returned else {
        return Ok(None);
    };
    let own_values = definition
        .runtime
        .local_types
        .len()
        .checked_add(max_stack)
        .ok_or(UnsupportedReason::RegionLimit)?;
    let own_values = u32::try_from(own_values).map_err(|_| UnsupportedReason::RegionLimit)?;
    let retired_cost = u32::try_from(code.len())
        .map_err(|_| UnsupportedReason::RegionLimit)?
        .checked_add(nested_cost)
        .ok_or(UnsupportedReason::RegionLimit)?;
    Ok(Some(ConstructorExecution {
        result,
        retired_cost,
        frame_count: 1u32
            .checked_add(nested_frames)
            .ok_or(UnsupportedReason::RegionLimit)?,
        stack_values: own_values
            .checked_add(nested_values)
            .ok_or(UnsupportedReason::RegionLimit)?,
    }))
}

pub(super) fn constant_symbol(tag: ValueTag, bits: u64) -> ConstructorSymbol {
    ConstructorSymbol::Constant(ScalarConstant {
        bits,
        tag: tag as u64,
    })
}

pub(super) fn call_value_kind(
    module: &Module,
    ty: u32,
) -> Result<CallValueKind, UnsupportedReason> {
    match module.types.get(ty as usize) {
        Some(BcType::Var(variable)) => Ok(CallValueKind::Variable(*variable)),
        _ => scalar_kind(module, ty).map(CallValueKind::Fixed),
    }
}

pub(super) fn instantiate_call(
    signature: &CallSignature,
    caller: &Module,
    app: Option<u32>,
) -> Result<CallContract, UnsupportedReason> {
    let application = match app {
        Some(app) => Some(
            caller
                .apps
                .get(app as usize)
                .ok_or(UnsupportedReason::InvalidControlFlow)?,
        ),
        None => None,
    };
    let instantiate = |value: CallValueKind| match value {
        CallValueKind::Fixed(kind) => Ok(kind),
        CallValueKind::Variable(variable) => {
            let ty = application
                .and_then(|application| application.types.get(variable as usize))
                .copied()
                .ok_or(UnsupportedReason::InvalidControlFlow)?;
            scalar_kind(caller, ty)
        }
    };
    Ok(CallContract {
        params: signature
            .params
            .iter()
            .copied()
            .map(instantiate)
            .collect::<Result<Vec<_>, _>>()?,
        virtual_params: signature.virtual_params.clone(),
        local_count: Some(signature.local_count),
        result: instantiate(signature.result)?,
        receiver: None,
        value_target: None,
        virtual_result: signature.virtual_constructor.is_some(),
        scalar_result: None,
    })
}

pub(super) fn scalar_kind(
    module: &lm_bytecode::Module,
    ty: u32,
) -> Result<ScalarKind, UnsupportedReason> {
    scalar_kind_in(module, &module.types, ty)
}

pub(super) fn scalar_kind_in(
    module: &lm_bytecode::Module,
    types: &[BcType],
    ty: u32,
) -> Result<ScalarKind, UnsupportedReason> {
    match types.get(ty as usize) {
        Some(BcType::Unit) => Ok(ScalarKind::Unit),
        Some(BcType::Bool) => Ok(ScalarKind::Bool),
        Some(BcType::Int) => Ok(ScalarKind::Int),
        Some(BcType::Float) => Ok(ScalarKind::Float),
        Some(
            BcType::Str
            | BcType::Map(_, _)
            | BcType::Fn(_, _, _, _)
            | BcType::Digest
            | BcType::Fault
            | BcType::Request
            | BcType::PolicyTable
            | BcType::Vm
            | BcType::Run(_)
            | BcType::Wait(_)
            | BcType::PendingCall(_, _)
            | BcType::Handle(_, _)
            | BcType::VmSnapshot
            | BcType::RunSnapshot(_)
            | BcType::FileHandle
            | BcType::ResourceHandle
            | BcType::HostResource,
        ) => Ok(ScalarKind::Object(ty)),
        Some(BcType::Callback(_, _, _, _)) => Ok(ScalarKind::Callback(ty)),
        Some(BcType::Class(class)) => {
            let core = lm_bytecode::corepin::declared_layout(module);
            if core.char_value == Some(*class) {
                Ok(ScalarKind::Char)
            } else {
                Ok(ScalarKind::Object(ty))
            }
        }
        Some(BcType::Inst(class, _)) if is_option_class(module, *class) => {
            Ok(ScalarKind::Tagged(ty))
        }
        Some(BcType::Inst(_, _) | BcType::List(_) | BcType::Tuple(_) | BcType::Bytes) => {
            Ok(ScalarKind::Object(ty))
        }
        Some(BcType::Op(_, _)) => Ok(ScalarKind::Operation),
        Some(BcType::Never | BcType::Var(_) | BcType::Projection { .. }) => {
            Ok(ScalarKind::Tagged(ty))
        }
        None => Err(UnsupportedReason::MissingSource),
    }
}

/// Return true when one bytecode type has a native value representation.
pub fn type_has_native_representation(ty: &BcType) -> bool {
    match ty {
        BcType::Unit
        | BcType::Never
        | BcType::Bool
        | BcType::Int
        | BcType::Float
        | BcType::Str
        | BcType::Class(_)
        | BcType::Inst(_, _)
        | BcType::List(_)
        | BcType::Map(_, _)
        | BcType::Tuple(_)
        | BcType::Fn(_, _, _, _)
        | BcType::Callback(_, _, _, _)
        | BcType::Var(_)
        | BcType::Projection { .. }
        | BcType::Fault
        | BcType::Request
        | BcType::PolicyTable
        | BcType::Vm
        | BcType::Run(_)
        | BcType::Wait(_)
        | BcType::PendingCall(_, _)
        | BcType::Handle(_, _)
        | BcType::Op(_, _)
        | BcType::Digest
        | BcType::VmSnapshot
        | BcType::RunSnapshot(_)
        | BcType::Bytes
        | BcType::FileHandle
        | BcType::ResourceHandle
        | BcType::HostResource => true,
    }
}

pub(super) fn is_option_class(module: &Module, class: u32) -> bool {
    let core = lm_bytecode::corepin::declared_layout(module);
    if [core.option_some, core.option_none].contains(&Some(class)) {
        return true;
    }
    [core.option_some, core.option_none]
        .into_iter()
        .flatten()
        .filter_map(|arm| module.classes.get(arm as usize))
        .filter_map(lm_bytecode::BcClass::parent)
        .any(|parent| parent == class)
}

pub(crate) fn is_root_kind(kind: ScalarKind) -> bool {
    matches!(
        kind,
        ScalarKind::Object(_) | ScalarKind::Tagged(_) | ScalarKind::Callback(_)
    )
}
