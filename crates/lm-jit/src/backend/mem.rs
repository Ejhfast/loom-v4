//! Raw activation and memory access emission.

use super::*;

pub(super) fn store_i32_constant(
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
pub(super) enum RawActivationField {
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
            RawActivationField::Scalars => std_mem::offset_of!(RawNativeActivation, scalars),
            RawActivationField::Tags => std_mem::offset_of!(RawNativeActivation, tags),
            RawActivationField::States => std_mem::offset_of!(RawNativeActivation, states),
            RawActivationField::ScalarLen => std_mem::offset_of!(RawNativeActivation, scalar_len),
            RawActivationField::ScalarCapacity => {
                std_mem::offset_of!(RawNativeActivation, scalar_capacity)
            }
            RawActivationField::Frames => std_mem::offset_of!(RawNativeActivation, frames),
            RawActivationField::FrameLen => std_mem::offset_of!(RawNativeActivation, frame_len),
            RawActivationField::FrameCapacity => {
                std_mem::offset_of!(RawNativeActivation, frame_capacity)
            }
            RawActivationField::ChangedFrom => {
                std_mem::offset_of!(RawNativeActivation, changed_from)
            }
            RawActivationField::MaxStackValues => {
                std_mem::offset_of!(RawNativeActivation, max_stack_values)
            }
            RawActivationField::BaseFrames => {
                std_mem::offset_of!(RawNativeActivation, base_frames)
            }
            RawActivationField::MaxFrames => std_mem::offset_of!(RawNativeActivation, max_frames),
            RawActivationField::RootCapacity => {
                std_mem::offset_of!(RawNativeActivation, root_capacity)
            }
            RawActivationField::LiteralValues => {
                std_mem::offset_of!(RawNativeActivation, literal_values)
            }
            RawActivationField::LiteralCount => {
                std_mem::offset_of!(RawNativeActivation, literal_count)
            }
            RawActivationField::PollRequested => {
                std_mem::offset_of!(RawNativeActivation, poll_requested)
            }
            RawActivationField::HardFuel => std_mem::offset_of!(RawNativeActivation, hard_fuel),
            RawActivationField::PollDeadline => {
                std_mem::offset_of!(RawNativeActivation, poll_deadline)
            }
            RawActivationField::PollInterval => {
                std_mem::offset_of!(RawNativeActivation, poll_interval)
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

pub(super) fn load_activation_u32(
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

pub(super) fn load_activation_u64(
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

pub(super) fn load_activation_pointer(
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

pub(super) fn store_activation_u32(
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

pub(super) fn store_activation_u64(
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

pub(super) fn load_cell_u32(
    builder: &mut FunctionBuilder<'_>,
    cell: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value(builder, types::I32, cell, offset)
}

pub(super) fn store_i32_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

pub(super) fn store_i8_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

pub(super) fn store_native_value(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

pub(super) fn load_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(builder, ty, pointer, offset, MemFlags::new())
}

pub(super) fn load_heap_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(builder, ty, pointer, offset, heap_mem_flags())
}

pub(super) fn load_immutable_heap_value(
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

pub(super) fn load_vmctx_value(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
) -> Result<ir::Value, CompileError> {
    load_value_with_flags(builder, ty, pointer, offset, vmctx_mem_flags())
}

pub(super) fn load_value_with_flags(
    builder: &mut FunctionBuilder<'_>,
    ty: ir::Type,
    pointer: ir::Value,
    offset: usize,
    flags: MemFlags,
) -> Result<ir::Value, CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    Ok(builder.ins().load(ty, flags, pointer, offset))
}

pub(super) fn store_i32_value_with_flags(
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

pub(super) const fn vmctx_mem_flags() -> MemFlags {
    MemFlags::trusted().with_alias_region(Some(AliasRegion::Vmctx))
}

pub(super) const fn immutable_vmctx_mem_flags() -> MemFlags {
    vmctx_mem_flags().with_readonly().with_can_move()
}

pub(super) const fn heap_mem_flags() -> MemFlags {
    MemFlags::trusted().with_alias_region(Some(AliasRegion::Heap))
}

pub(super) const fn table_mem_flags() -> MemFlags {
    MemFlags::trusted()
        .with_readonly()
        .with_alias_region(Some(AliasRegion::Table))
}

pub(super) fn store_i64(
    builder: &mut FunctionBuilder<'_>,
    pointer: ir::Value,
    offset: usize,
    value: ir::Value,
) -> Result<(), CompileError> {
    let offset = i32::try_from(offset).map_err(|_| CompileError::Backend)?;
    builder.ins().store(MemFlags::new(), value, pointer, offset);
    Ok(())
}

pub(super) fn store_heap_value(
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
