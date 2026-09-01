//! List runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn runtime_list_contains(
        &mut self,
        reference: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let Some(value) = tagged_value(value_tag, value_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Some(crate::Object::List { items, .. }) = self.machine.vm.heap.try_get(reference)
        else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        for item in items.iter().copied() {
            match self.machine.values_equal(self.module, item, value) {
                Ok(true) => {
                    return RuntimeValueResult::Value {
                        bits: 1,
                        tag: ValueTag::Bool as u64,
                    };
                }
                Ok(false) => {}
                Err(fault) => return RuntimeValueResult::Fault(fault),
            }
        }
        RuntimeValueResult::Value {
            bits: 0,
            tag: ValueTag::Bool as u64,
        }
    }

    pub(super) fn runtime_grow_list(&mut self, request: ListGrowthRequest<'_>) -> ListGrowthResult {
        let ListGrowthRequest {
            reference,
            value_bits,
            value_tag,
            roots,
            allow_collection,
        } = request;
        let reference = object_reference(reference);
        let Some(value) = tagged_value(value_tag, value_bits) else {
            return ListGrowthResult::Interpreter;
        };
        let Some(crate::Object::List { epoch, .. }) = self.machine.vm.heap.try_get(reference)
        else {
            return ListGrowthResult::Interpreter;
        };
        if self.machine.vm.heap.is_frozen(reference) || epoch.ensure_bumpable().is_err() {
            return ListGrowthResult::Interpreter;
        }
        if self.machine.vm.heap.collection_due(16) && !allow_collection {
            return ListGrowthResult::Interpreter;
        }
        let roots = match decode_root_objects(roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return ListGrowthResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return ListGrowthResult::Interpreter,
        };
        if let Err(fault) =
            self.machine
                .reserve_native(16, self.base_local, self.base_operand, &roots)
        {
            return if fault == crate::FaultCode::HeapLimit {
                ListGrowthResult::HeapLimit
            } else {
                ListGrowthResult::Interpreter
            };
        }
        let crate::Object::List { items, epoch } = self.machine.vm.heap.get_mut(reference) else {
            return ListGrowthResult::Interpreter;
        };
        if items.try_reserve(1).is_err() {
            return ListGrowthResult::HeapLimit;
        }
        if epoch.bump().is_err() {
            return ListGrowthResult::Interpreter;
        }
        items.push(value);
        self.machine.vm.heap.recharge_local(reference);
        ListGrowthResult::Done {
            heap: self.machine.vm.heap.jit_view(),
        }
    }

    pub(super) fn runtime_insert_list(
        &mut self,
        request: ListInsertRequest<'_>,
    ) -> ListGrowthResult {
        let ListInsertRequest {
            reference,
            index,
            value_bits,
            value_tag,
            roots,
            allow_collection,
        } = request;
        let Ok(index) = usize::try_from(index) else {
            return ListGrowthResult::Interpreter;
        };
        let reference = object_reference(reference);
        let Some(value) = tagged_value(value_tag, value_bits) else {
            return ListGrowthResult::Interpreter;
        };
        let Some(crate::Object::List { items, epoch }) = self.machine.vm.heap.try_get(reference)
        else {
            return ListGrowthResult::Interpreter;
        };
        if self.machine.vm.heap.is_frozen(reference)
            || index > items.len()
            || epoch.ensure_bumpable().is_err()
        {
            return ListGrowthResult::Interpreter;
        }
        if self.machine.vm.heap.collection_due(16) && !allow_collection {
            return ListGrowthResult::Interpreter;
        }
        let roots = match decode_root_objects(roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return ListGrowthResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return ListGrowthResult::Interpreter,
        };
        if let Err(fault) =
            self.machine
                .reserve_native(16, self.base_local, self.base_operand, &roots)
        {
            return if fault == crate::FaultCode::HeapLimit {
                ListGrowthResult::HeapLimit
            } else {
                ListGrowthResult::Interpreter
            };
        }
        let crate::Object::List { items, epoch } = self.machine.vm.heap.get_mut(reference) else {
            return ListGrowthResult::Interpreter;
        };
        if index > items.len() {
            return ListGrowthResult::Interpreter;
        }
        if items.try_reserve(1).is_err() {
            return ListGrowthResult::HeapLimit;
        }
        if epoch.bump().is_err() {
            return ListGrowthResult::Interpreter;
        }
        items.insert(index, value);
        self.machine.vm.heap.recharge_local(reference);
        ListGrowthResult::Done {
            heap: self.machine.vm.heap.jit_view(),
        }
    }

    pub(super) fn runtime_reserve_list(
        &mut self,
        request: CollectionReserveRequest<'_>,
    ) -> CollectionReserveResult {
        let CollectionReserveRequest {
            reference,
            additional,
            roots,
            allow_collection,
        } = request;
        let Ok(additional) = usize::try_from(additional) else {
            return CollectionReserveResult::Interpreter;
        };
        let reference = object_reference(reference);
        let Some(crate::Object::List { items, epoch }) = self.machine.vm.heap.try_get(reference)
        else {
            return CollectionReserveResult::Interpreter;
        };
        if self.machine.vm.heap.is_frozen(reference) {
            return CollectionReserveResult::Interpreter;
        }
        if additional <= items.capacity().saturating_sub(items.len()) {
            return CollectionReserveResult::Done {
                heap: self.machine.vm.heap.jit_view(),
            };
        }
        if epoch.ensure_bumpable().is_err() {
            return CollectionReserveResult::Interpreter;
        }
        let Some(growth) = additional.checked_mul(std::mem::size_of::<Value>()) else {
            return CollectionReserveResult::HeapLimit;
        };
        if self.machine.vm.heap.collection_due(growth) && !allow_collection {
            return CollectionReserveResult::Interpreter;
        }
        let roots = match decode_root_objects(roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => return CollectionReserveResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return CollectionReserveResult::Interpreter,
        };
        if let Err(fault) =
            self.machine
                .reserve_native(growth, self.base_local, self.base_operand, &roots)
        {
            return if fault == crate::FaultCode::HeapLimit {
                CollectionReserveResult::HeapLimit
            } else {
                CollectionReserveResult::Interpreter
            };
        }
        let crate::Object::List { items, epoch } = self.machine.vm.heap.get_mut(reference) else {
            return CollectionReserveResult::Interpreter;
        };
        let before = items.capacity();
        if items.try_reserve(additional).is_err() {
            return CollectionReserveResult::HeapLimit;
        }
        if items.capacity() != before && epoch.bump().is_err() {
            return CollectionReserveResult::Interpreter;
        }
        CollectionReserveResult::Done {
            heap: self.machine.vm.heap.jit_view(),
        }
    }
}
