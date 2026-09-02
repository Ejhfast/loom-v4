//! Heap and value contract planning.

use super::*;

pub(super) fn virtual_receiver(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<VirtualReceiver, UnsupportedReason> {
    let fixed = |class: Option<u32>| {
        class
            .map(|class| VirtualReceiver::Immediate { class })
            .ok_or(UnsupportedReason::MissingSource)
    };
    match receiver {
        ScalarKind::Unit => fixed(context.runtime_core.unit),
        ScalarKind::Bool => fixed(context.runtime_core.boolean),
        ScalarKind::Int => fixed(context.runtime_core.int),
        ScalarKind::Float => fixed(context.runtime_core.float),
        ScalarKind::Char => fixed(context.runtime_core.char_value),
        ScalarKind::Object(ty) => {
            let source_core = lm_bytecode::corepin::declared_layout(context.module);
            let object = |tag, class: Option<u32>| {
                class
                    .map(|class| VirtualReceiver::Object { tag, class })
                    .ok_or(UnsupportedReason::MissingSource)
            };
            match context.verified_types.get(ty as usize) {
                Some(BcType::Str) => object(lm_heap::JIT_OBJECT_STR, context.runtime_core.string),
                Some(BcType::List(_)) => {
                    object(lm_heap::JIT_OBJECT_LIST, context.runtime_core.list)
                }
                Some(BcType::Map(_, _)) => {
                    object(lm_heap::JIT_OBJECT_MAP, context.runtime_core.map)
                }
                Some(BcType::Tuple(items)) => {
                    let class = context
                        .runtime_core
                        .tuples
                        .get(items.len())
                        .copied()
                        .flatten();
                    object(lm_heap::JIT_OBJECT_TUPLE, class)
                }
                Some(BcType::Bytes) => {
                    object(lm_heap::JIT_OBJECT_BYTES, context.runtime_core.bytes)
                }
                Some(BcType::Class(class) | BcType::Inst(class, _))
                    if source_core.text == Some(*class) =>
                {
                    match (context.runtime_core.string, context.runtime_core.substring) {
                        (Some(string), Some(substring)) => {
                            Ok(VirtualReceiver::Text { string, substring })
                        }
                        _ => Err(UnsupportedReason::MissingSource),
                    }
                }
                Some(BcType::Class(class) | BcType::Inst(class, _))
                    if source_core.string == Some(*class) =>
                {
                    object(lm_heap::JIT_OBJECT_STR, context.runtime_core.string)
                }
                Some(BcType::Class(class) | BcType::Inst(class, _))
                    if source_core.substring == Some(*class) =>
                {
                    object(
                        lm_heap::JIT_OBJECT_SUBSTRING,
                        context.runtime_core.substring,
                    )
                }
                Some(BcType::Class(class) | BcType::Inst(class, _))
                    if source_core.string_builder == Some(*class) =>
                {
                    object(
                        lm_heap::JIT_OBJECT_STRING_BUILDER,
                        context.runtime_core.string_builder,
                    )
                }
                Some(BcType::Class(class) | BcType::Inst(class, _))
                    if source_core.byte_buffer == Some(*class) =>
                {
                    object(
                        lm_heap::JIT_OBJECT_BYTE_BUFFER,
                        context.runtime_core.byte_buffer,
                    )
                }
                Some(BcType::Class(class) | BcType::Inst(class, _)) => {
                    Ok(VirtualReceiver::Instance {
                        class: relocate_class(*class, context.class_relocation)?,
                    })
                }
                _ => Err(UnsupportedReason::NonScalarType),
            }
        }
        ScalarKind::Tagged(_) | ScalarKind::Callback(_) | ScalarKind::Operation => {
            Err(UnsupportedReason::NonScalarType)
        }
    }
}

pub(super) fn stack_from_end(
    stack: &[ScalarKind],
    offset: usize,
) -> Result<ScalarKind, UnsupportedReason> {
    let index = offset
        .checked_add(1)
        .and_then(|count| stack.len().checked_sub(count))
        .ok_or(UnsupportedReason::InvalidStack)?;
    stack
        .get(index)
        .copied()
        .ok_or(UnsupportedReason::InvalidStack)
}

pub(super) fn stack_type_from_end(stack: &[u32], offset: usize) -> Result<u32, UnsupportedReason> {
    let index = offset
        .checked_add(1)
        .and_then(|count| stack.len().checked_sub(count))
        .ok_or(UnsupportedReason::InvalidStack)?;
    stack
        .get(index)
        .copied()
        .ok_or(UnsupportedReason::InvalidStack)
}

pub(super) fn field_contract(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
    field: u32,
) -> Result<(u32, ValueContract), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let Some(BcType::Class(class) | BcType::Inst(class, _)) =
        context.verified_types.get(ty as usize)
    else {
        return Err(UnsupportedReason::NonScalarType);
    };
    let field_type = context
        .module
        .classes
        .get(*class as usize)
        .and_then(|class| class.fields.get(field as usize))
        .map(|(_, ty)| *ty)
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    Ok((
        relocate_class(*class, context.class_relocation)?,
        value_contract(context, field_type)?,
    ))
}

pub(super) fn tuple_element_contract(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
    index: u32,
) -> Result<ValueContract, UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let Some(BcType::Tuple(elements)) = context.verified_types.get(ty as usize) else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let element = elements
        .get(index as usize)
        .copied()
        .ok_or(UnsupportedReason::InvalidControlFlow)?;
    value_contract(context, element)
}

pub(super) fn list_element_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<u32, UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match context.verified_types.get(ty as usize) {
        Some(BcType::List(element)) => Ok(*element),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn map_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(u32, u32), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match context.verified_types.get(ty as usize) {
        Some(BcType::Map(key, value)) => Ok((*key, *value)),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn digest_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match context.verified_types.get(ty as usize) {
        Some(BcType::Digest) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn function_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match context.verified_types.get(ty as usize) {
        Some(BcType::Fn(_, _, _, _)) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn option_argument_type(
    context: &SegmentAnalysisContext<'_>,
    ty: u32,
) -> Result<u32, UnsupportedReason> {
    let Some(BcType::Inst(class, arguments)) = context.verified_types.get(ty as usize) else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let core = lm_bytecode::corepin::declared_layout(context.module);
    if ![core.option, core.option_some, core.option_none].contains(&Some(*class))
        || arguments.len() != 1
    {
        return Err(UnsupportedReason::InvalidStack);
    }
    Ok(arguments[0])
}

pub(super) fn bytes_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    match context.verified_types.get(ty as usize) {
        Some(BcType::Bytes) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn text_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let contract = value_contract(context, ty)?;
    match contract.object {
        Some(ObjectContract::Str | ObjectContract::Text) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn string_builder_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let contract = value_contract(context, ty)?;
    match contract.object {
        Some(ObjectContract::StringBuilder) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn byte_buffer_type(
    context: &SegmentAnalysisContext<'_>,
    receiver: ScalarKind,
) -> Result<(), UnsupportedReason> {
    let ScalarKind::Object(ty) = receiver else {
        return Err(UnsupportedReason::InvalidStack);
    };
    let contract = value_contract(context, ty)?;
    match contract.object {
        Some(ObjectContract::ByteBuffer) => Ok(()),
        _ => Err(UnsupportedReason::InvalidStack),
    }
}

pub(super) fn expect_scalar(
    receiver: ScalarKind,
    expected: ScalarKind,
) -> Result<(), UnsupportedReason> {
    if receiver == expected {
        Ok(())
    } else {
        Err(UnsupportedReason::InvalidStack)
    }
}

pub(super) fn class_test_target(
    context: &SegmentAnalysisContext<'_>,
    ty: u32,
) -> Result<u32, UnsupportedReason> {
    let class = match context.module.types.get(ty as usize) {
        Some(BcType::Class(class) | BcType::Inst(class, _)) => *class,
        _ => return Err(UnsupportedReason::InvalidStack),
    };
    relocate_class(class, context.class_relocation)
}

pub(super) fn option_test_target(
    module: &Module,
    ty: u32,
) -> Result<Option<OptionTarget>, UnsupportedReason> {
    let class = match module.types.get(ty as usize) {
        Some(BcType::Class(class) | BcType::Inst(class, _)) => *class,
        _ => return Err(UnsupportedReason::InvalidStack),
    };
    let core = lm_bytecode::corepin::declared_layout(module);
    Ok(if core.option == Some(class) {
        Some(OptionTarget::Family)
    } else if core.option_some == Some(class) {
        Some(OptionTarget::Some)
    } else if core.option_none == Some(class) {
        Some(OptionTarget::None)
    } else {
        None
    })
}

pub(super) fn value_contract(
    context: &SegmentAnalysisContext<'_>,
    ty: u32,
) -> Result<ValueContract, UnsupportedReason> {
    let kind = scalar_kind_in(context.module, context.verified_types, ty)?;
    let core = lm_bytecode::corepin::declared_layout(context.module);
    let object = match context.verified_types.get(ty as usize) {
        Some(BcType::Str) => Some(ObjectContract::Str),
        Some(BcType::Class(class) | BcType::Inst(class, _))
            if [core.text, core.substring].contains(&Some(*class)) =>
        {
            Some(ObjectContract::Text)
        }
        Some(BcType::Class(class) | BcType::Inst(class, _))
            if core.string_builder == Some(*class) =>
        {
            Some(ObjectContract::StringBuilder)
        }
        Some(BcType::Class(class) | BcType::Inst(class, _)) if core.byte_buffer == Some(*class) => {
            Some(ObjectContract::ByteBuffer)
        }
        Some(BcType::Class(class) | BcType::Inst(class, _))
            if matches!(kind, ScalarKind::Object(_)) =>
        {
            Some(ObjectContract::Instance(relocate_class(
                *class,
                context.class_relocation,
            )?))
        }
        Some(BcType::List(_)) => Some(ObjectContract::List),
        Some(BcType::Map(_, _)) => Some(ObjectContract::Map),
        Some(BcType::Tuple(_)) => Some(ObjectContract::Tuple),
        Some(BcType::Fn(_, _, _, _)) => Some(ObjectContract::Closure),
        Some(BcType::Bytes) => Some(ObjectContract::Bytes),
        Some(BcType::Digest) => Some(ObjectContract::Digest),
        _ => None,
    };
    Ok(ValueContract { kind, object })
}

pub(super) fn relocate_class(
    class: u32,
    relocation: Option<&[u32]>,
) -> Result<u32, UnsupportedReason> {
    match relocation {
        Some(classes) => classes
            .get(class as usize)
            .copied()
            .ok_or(UnsupportedReason::MissingSource),
        None => Ok(class),
    }
}
