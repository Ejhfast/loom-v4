//! String and byte builder runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn string_builder_growth(
        &mut self,
        reference: ObjRef,
        additional: usize,
        request: &HeapOperationRequest<'_>,
    ) -> Result<usize, HeapOperationResult> {
        if self.machine.vm.heap.is_frozen(reference) {
            return Err(HeapOperationResult::Fault(crate::FaultCode::FrozenWrite));
        }
        let growth = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::StrBuilder(builder)) => builder.reserve_growth(additional),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
        .ok_or(HeapOperationResult::Fault(crate::FaultCode::InvalidVmState))?;
        self.reserve_heap_growth(growth, request)?;
        Ok(growth)
    }

    pub(super) fn byte_buffer_growth(
        &mut self,
        reference: ObjRef,
        additional: usize,
        request: &HeapOperationRequest<'_>,
    ) -> Result<usize, HeapOperationResult> {
        if self.machine.vm.heap.is_frozen(reference) {
            return Err(HeapOperationResult::Fault(crate::FaultCode::FrozenWrite));
        }
        let growth = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::ByteBuf(buffer)) => buffer.reserve_growth(additional),
            _ => return Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
        .ok_or(HeapOperationResult::Fault(crate::FaultCode::InvalidVmState))?;
        self.reserve_heap_growth(growth, request)?;
        Ok(growth)
    }

    pub(super) fn append_builder_text(
        &mut self,
        request: HeapOperationRequest<'_>,
        text: &str,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        let growth = match self.string_builder_growth(builder, text.len(), &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        let appended = match self.machine.vm.heap.get_mut(builder) {
            crate::Object::StrBuilder(target) => {
                if growth != 0 {
                    match target.try_reserve(text.len()) {
                        Ok(true) => {}
                        Ok(false) => {
                            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                        }
                        Err(_) => return HeapOperationResult::HeapLimit,
                    }
                }
                target.append_str(text)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(builder);
        }
        Self::heap_object_value(builder)
    }

    pub(super) fn byte_buffer_find_operation(
        &self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        let needle = match self.bytes_value(request.second) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let bytes = match self.machine.vm.heap.try_get(buffer) {
            Some(crate::Object::ByteBuf(bytes)) if bytes.buffer().is_some() => bytes,
            Some(crate::Object::ByteBuf(_)) => {
                return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let found = usize::try_from(request.third as i64)
            .ok()
            .and_then(|start| bytes.find_from(&needle, start))
            .and_then(|index| i64::try_from(index).ok())
            .unwrap_or(-1);
        heap_int(found)
    }

    pub(super) fn runtime_string_builder_new(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.allocate_heap_object(
            crate::Object::StrBuilder(NativeStringBuilder::new()),
            &request,
        )
    }

    pub(super) fn runtime_string_builder_append_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        let source = object_reference(request.second);
        let text_len = match self.machine.vm.heap.text(source) {
            Some(text) => text.len(),
            None => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let growth = match self.string_builder_growth(builder, text_len, &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        if growth != 0 {
            match self.machine.vm.heap.get_mut(builder) {
                crate::Object::StrBuilder(target) => match target.try_reserve(text_len) {
                    Ok(true) => {}
                    Ok(false) => {
                        return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                    }
                    Err(_) => return HeapOperationResult::HeapLimit,
                },
                _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
            }
        }
        let appended = self.machine.vm.heap.append_string(builder, source);
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(builder);
        }
        Self::heap_object_value(builder)
    }

    pub(super) fn runtime_string_builder_append_int(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        let value = request.second as i64;
        let length = integer_text_len(value);
        let growth = match self.string_builder_growth(builder, length, &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        let appended = match self.machine.vm.heap.get_mut(builder) {
            crate::Object::StrBuilder(target) => {
                if growth != 0 {
                    match target.try_reserve(length) {
                        Ok(true) => {}
                        Ok(false) => {
                            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                        }
                        Err(_) => return HeapOperationResult::HeapLimit,
                    }
                }
                target.append_int(value)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(builder);
        }
        Self::heap_object_value(builder)
    }

    pub(super) fn runtime_string_builder_append_bool(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let text = match request.second {
            0 => "false",
            1 => "true",
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.append_builder_text(request, text)
    }

    pub(super) fn runtime_string_builder_append_char(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let Some(value) = u32::try_from(request.second).ok().and_then(char::from_u32) else {
            return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let builder = object_reference(request.first);
        let length = value.len_utf8();
        let growth = match self.string_builder_growth(builder, length, &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        let appended = match self.machine.vm.heap.get_mut(builder) {
            crate::Object::StrBuilder(target) => {
                if growth != 0 {
                    match target.try_reserve(length) {
                        Ok(true) => {}
                        Ok(false) => {
                            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                        }
                        Err(_) => return HeapOperationResult::HeapLimit,
                    }
                }
                target.push(value)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(builder);
        }
        Self::heap_object_value(builder)
    }

    pub(super) fn runtime_string_builder_append_float(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let text = float_text(f64::from_bits(request.second));
        self.append_builder_text(request, &text)
    }

    pub(super) fn runtime_string_builder_build(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        let (length, scalar_count, ascii) = match self.machine.vm.heap.try_get(builder) {
            Some(crate::Object::StrBuilder(builder)) => {
                let Some(length) = builder.byte_len() else {
                    return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                };
                let Some(scalar_count) = builder.scalar_len() else {
                    return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                };
                let Some(ascii) = builder.is_ascii() else {
                    return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                };
                (length, scalar_count, ascii)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let text = match self.machine.vm.heap.try_get(builder) {
            Some(crate::Object::StrBuilder(builder)) => {
                let Some(source) = builder.buffer() else {
                    return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                };
                match SharedText::try_from_str_parts(source, scalar_count, ascii) {
                    Ok(text) => text,
                    Err(_) => return HeapOperationResult::HeapLimit,
                }
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    pub(super) fn runtime_string_builder_finish(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        if self.machine.vm.heap.is_frozen(builder) {
            return HeapOperationResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let parts = match self.machine.vm.heap.get_mut(builder) {
            crate::Object::StrBuilder(builder) => builder.finish(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some((text, scalar_count, ascii)) = parts else {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        };
        self.machine.vm.heap.recharge_local(builder);
        let text = match SharedText::try_from_string_parts(text, scalar_count, ascii) {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        match self.allocate_heap_object(crate::Object::Str(text), &request) {
            HeapOperationResult::Interpreter => HeapOperationResult::HeapLimit,
            result => result,
        }
    }

    pub(super) fn runtime_byte_buffer_new(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.allocate_heap_object(crate::Object::ByteBuf(NativeByteBuffer::new()), &request)
    }

    pub(super) fn runtime_byte_buffer_append(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        let Ok(byte) = u8::try_from(request.second as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
        };
        let growth = match self.byte_buffer_growth(buffer, 1, &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        let appended = match self.machine.vm.heap.get_mut(buffer) {
            crate::Object::ByteBuf(target) => {
                if growth != 0 {
                    match target.try_reserve(1) {
                        Ok(true) => {}
                        Ok(false) => {
                            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                        }
                        Err(_) => return HeapOperationResult::HeapLimit,
                    }
                }
                target.push(byte)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(buffer);
        }
        Self::heap_object_value(buffer)
    }

    pub(super) fn runtime_byte_buffer_build(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        let length = match self.machine.vm.heap.try_get(buffer) {
            Some(crate::Object::ByteBuf(buffer)) => match buffer.len() {
                Some(length) => length,
                None => return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState),
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let bytes = match self.machine.vm.heap.try_get(buffer) {
            Some(crate::Object::ByteBuf(buffer)) => {
                let Some(source) = buffer.buffer() else {
                    return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                };
                match SharedBytes::try_from_slice(source) {
                    Ok(bytes) => bytes,
                    Err(_) => return HeapOperationResult::HeapLimit,
                }
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    pub(super) fn runtime_byte_buffer_extend(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        let source = object_reference(request.second);
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => bytes.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let growth = match self.byte_buffer_growth(buffer, bytes.len(), &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        let appended = match self.machine.vm.heap.get_mut(buffer) {
            crate::Object::ByteBuf(target) => {
                if growth != 0 {
                    match target.try_reserve(bytes.len()) {
                        Ok(true) => {}
                        Ok(false) => {
                            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                        }
                        Err(_) => return HeapOperationResult::HeapLimit,
                    }
                }
                target.extend(&bytes)
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !appended {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        }
        if growth != 0 {
            self.machine.vm.heap.recharge_local(buffer);
        }
        Self::heap_object_value(buffer)
    }

    pub(super) fn runtime_byte_buffer_reserve(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        let Ok(additional) = usize::try_from(request.second as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
        };
        let growth = match self.byte_buffer_growth(buffer, additional, &request) {
            Ok(growth) => growth,
            Err(result) => return result,
        };
        if growth != 0 {
            match self.machine.vm.heap.get_mut(buffer) {
                crate::Object::ByteBuf(target) => match target.try_reserve(additional) {
                    Ok(true) => {}
                    Ok(false) => {
                        return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
                    }
                    Err(_) => return HeapOperationResult::HeapLimit,
                },
                _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
            }
            self.machine.vm.heap.recharge_local(buffer);
        }
        Self::heap_object_value(buffer)
    }

    pub(super) fn runtime_byte_buffer_finish(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let buffer = object_reference(request.first);
        if self.machine.vm.heap.is_frozen(buffer) {
            return HeapOperationResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let bytes = match self.machine.vm.heap.get_mut(buffer) {
            crate::Object::ByteBuf(buffer) => buffer.finish(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(bytes) = bytes else {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidVmState);
        };
        self.machine.vm.heap.recharge_local(buffer);
        match self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(bytes)), &request) {
            HeapOperationResult::Interpreter => HeapOperationResult::HeapLimit,
            result => result,
        }
    }

    pub(super) fn runtime_byte_buffer_find_from(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.byte_buffer_find_operation(request)
    }
}
