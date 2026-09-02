//! Map, equality, freeze, and digest runtime paths.

use super::*;

impl MachineRuntime<'_> {
    pub(super) fn runtime_map_intern_text_range(
        &mut self,
        request: MapInternTextRangeRequest<'_>,
    ) -> HeapOperationResult {
        let map = object_reference(request.map);
        let source = object_reference(request.source);
        if !matches!(
            self.machine.vm.heap.try_get(map),
            Some(crate::Object::Map { .. })
        ) {
            return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(map) {
            return HeapOperationResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let start = match usize::try_from(request.start) {
            Ok(start) => start,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let length = match usize::try_from(request.length) {
            Ok(length) => length,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let end = match start.checked_add(length) {
            Some(end) => end,
            None => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => match bytes.slice(start, end) {
                Some(bytes) => bytes,
                None => {
                    return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
                }
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(text) = bytes.utf8_view() else {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        };
        let query = crate::machine::BorrowedStringKey::Text(&text);
        match self.machine.map_lookup_borrowed_string(map, query) {
            Ok(Some((_, Value::Obj(reference)))) => return Self::heap_object_value(reference),
            Ok(Some(_)) => {
                return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch);
            }
            Ok(None) => {}
            Err(fault) => return HeapOperationResult::Fault(fault),
        }

        let object = match query.owned_object(self.machine) {
            Ok(Some(object)) => object,
            Ok(None) => {
                return HeapOperationResult::Fault(crate::FaultCode::MalformedState);
            }
            Err(crate::FaultCode::HeapLimit) => return HeapOperationResult::HeapLimit,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        let semantic_hash = match query.semantic_hash(self.machine) {
            Ok(hash) => hash,
            Err(fault) => return HeapOperationResult::Fault(fault),
        };
        let Some(growth) = 40usize.checked_add(self.machine.vm.heap.allocation_cost(&object))
        else {
            return HeapOperationResult::HeapLimit;
        };
        let heap_request = HeapOperationRequest {
            first: request.map,
            second: request.source,
            third: 0,
            roots: request.roots,
            allow_collection: request.allow_collection,
        };
        if let Err(result) = self.reserve_heap_growth(growth, &heap_request) {
            return result;
        }
        match self.machine.vm.heap.get_mut(map) {
            crate::Object::Map { entries, index } => {
                if entries.try_reserve(1).is_err() {
                    return HeapOperationResult::HeapLimit;
                }
                if let Err(fault) = index.epoch.bump() {
                    return HeapOperationResult::Fault(fault);
                }
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        }
        let reference = self.machine.vm.heap.alloc(object);
        self.allocations = self.allocations.saturating_add(1);
        let key = Value::Obj(reference);
        match self.machine.vm.heap.get_mut(map) {
            crate::Object::Map { entries, index } => {
                let position = entries.len() as u32;
                entries.push(MapEntry {
                    key,
                    value: key,
                    semantic_hash,
                });
                index.push_live(Machine::map_index_hash(semantic_hash), position);
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        }
        self.machine.vm.heap.recharge_local(map);
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: (reference.slot & lm_heap::JIT_PAGE_MASK == 0)
                .then(|| self.machine.vm.heap.jit_view()),
        }
    }

    pub(super) fn insert_map_entry(&mut self, request: MapInsertRequest<'_>) -> RuntimeUnitResult {
        let MapInsertRequest {
            reference,
            key,
            value,
            semantic_hash,
            entry_count,
            roots,
            allow_collection,
            key_storage,
        } = request;
        let can_grow = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, index }) if entries.len() == entry_count => {
                index.epoch.ensure_bumpable()
            }
            Some(crate::Object::Map { .. }) => return RuntimeUnitResult::Interpreter,
            _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(fault) = can_grow {
            return RuntimeUnitResult::Fault(fault);
        }
        let owned_key = match key_storage {
            MapInsertKeyStorage::BorrowedString => {
                match crate::machine::BorrowedStringKey::Value(key).owned_object(self.machine) {
                    Ok(key) => key,
                    Err(fault) => return RuntimeUnitResult::Fault(fault),
                }
            }
            MapInsertKeyStorage::Declared => None,
        };
        let key_cost = owned_key
            .as_ref()
            .map(|object| self.machine.vm.heap.allocation_cost(object))
            .unwrap_or(0);
        let Some(growth) = 40usize.checked_add(key_cost) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::HeapLimit);
        };
        if self.machine.vm.heap.collection_due(growth) && !allow_collection {
            return RuntimeUnitResult::Interpreter;
        }
        let roots = match decode_root_objects(roots) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => {
                return RuntimeUnitResult::Fault(crate::FaultCode::HeapLimit);
            }
            Err(CaptureDecodeFailure::Invalid) => return RuntimeUnitResult::Interpreter,
        };
        if let Err(fault) =
            self.machine
                .reserve_native(growth, self.base_local, self.base_operand, &roots)
        {
            return RuntimeUnitResult::Fault(fault);
        }
        let key = owned_key
            .map(|object| Value::Obj(self.machine.vm.heap.alloc(object)))
            .unwrap_or(key);
        match self.machine.vm.heap.get_mut(reference) {
            crate::Object::Map { entries, index } if entries.len() == entry_count => {
                if entries.try_reserve(1).is_err() {
                    return RuntimeUnitResult::Fault(crate::FaultCode::HeapLimit);
                }
                if let Err(fault) = index.epoch.bump() {
                    return RuntimeUnitResult::Fault(fault);
                }
                let position = entries.len() as u32;
                entries.push(MapEntry {
                    key,
                    value,
                    semantic_hash,
                });
                index.push_live(Machine::map_index_hash(semantic_hash), position);
            }
            crate::Object::Map { .. } => return RuntimeUnitResult::Interpreter,
            _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
        }
        self.machine.vm.heap.recharge_local(reference);
        RuntimeUnitResult::Done
    }

    pub(super) fn map_entry_value(
        &self,
        reference: u64,
        index: u64,
        load_value: bool,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let index = index as i64;
        let entry = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) if index >= 0 => entries
                .get(index as usize)
                .filter(|entry| entry.is_live())
                .copied(),
            Some(crate::Object::Map { .. }) => None,
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(entry) = entry else {
            return RuntimeValueResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        runtime_value(if load_value { entry.value } else { entry.key })
    }

    pub(super) fn map_probe_entry_value(
        &self,
        reference: u64,
        token: u64,
        load_value: bool,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let entry = match self.machine.map_token_entry(reference, token as i64) {
            Ok(Some(entry)) => entry,
            Ok(None) => return RuntimeValueResult::Fault(crate::FaultCode::MissingKey),
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        let pair = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => entries.get(entry).copied(),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(pair) = pair else {
            return RuntimeValueResult::Fault(crate::FaultCode::MalformedState);
        };
        runtime_value(if load_value { pair.value } else { pair.key })
    }

    pub(super) fn runtime_map_has(
        &mut self,
        reference: u64,
        key_bits: u64,
        key_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        let Some(key) = tagged_value(key_tag, key_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        match self.machine.map_lookup(reference, key) {
            Ok(found) => RuntimeValueResult::Value {
                bits: u64::from(found.is_some()),
                tag: ValueTag::Bool as u64,
            },
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_map_at(
        &mut self,
        reference: u64,
        key_bits: u64,
        key_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        let Some(key) = tagged_value(key_tag, key_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let position = match self.machine.map_lookup(reference, key) {
            Ok(Some(position)) => position,
            Ok(None) => return RuntimeValueResult::Fault(crate::FaultCode::MissingKey),
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        let value = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => {
                let Some(entry) = entries.get(position) else {
                    return RuntimeValueResult::Fault(crate::FaultCode::MalformedState);
                };
                entry.value
            }
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_value(value)
    }

    pub(super) fn runtime_map_get(
        &mut self,
        reference: u64,
        key_bits: u64,
        key_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        let Some(key) = tagged_value(key_tag, key_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let position = match self.machine.map_lookup(reference, key) {
            Ok(Some(position)) => position,
            Ok(None) => return RuntimeValueResult::Missing,
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        let value = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => {
                let Some(entry) = entries.get(position).filter(|entry| entry.is_live()) else {
                    return RuntimeValueResult::Fault(crate::FaultCode::MalformedState);
                };
                entry.value
            }
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_value(value)
    }

    pub(super) fn runtime_map_next_index(
        &mut self,
        reference: u64,
        cursor: u64,
        expected: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let cursor = cursor as i64;
        let expected = expected as i64;
        let crate::Object::Map { entries, index } = self.machine.vm.heap.get(reference) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        if expected < 0 || index.epoch.0 != expected as u32 {
            return RuntimeValueResult::Fault(crate::FaultCode::CollectionModified);
        }
        let Ok(cursor) = usize::try_from(cursor) else {
            return RuntimeValueResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let next = entries
            .get(cursor..)
            .and_then(|tail| tail.iter().position(MapEntry::is_live))
            .map(|offset| cursor + offset)
            .map_or(-1, |position| position as i64);
        runtime_int(next)
    }

    pub(super) fn runtime_map_key_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult {
        self.map_entry_value(reference, index, false)
    }

    pub(super) fn runtime_map_value_at(
        &mut self,
        reference: u64,
        index: u64,
    ) -> RuntimeValueResult {
        self.map_entry_value(reference, index, true)
    }

    pub(super) fn runtime_map_remove(
        &mut self,
        reference: u64,
        key_bits: u64,
        key_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeValueResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(key) = tagged_value(key_tag, key_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let position = match self.machine.map_lookup(reference, key) {
            Ok(Some(position)) => position,
            Ok(None) => return RuntimeValueResult::Missing,
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        match self.machine.remove_map_entry(reference, position) {
            Ok(value) => runtime_value(value),
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_map_clear(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeValueResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let changed = match self.machine.vm.heap.get_mut(reference) {
            crate::Object::Map { entries, index } if index.live_len() > 0 => {
                if let Err(fault) = index.epoch.bump() {
                    return RuntimeValueResult::Fault(fault);
                }
                entries.clear();
                index.reset();
                true
            }
            crate::Object::Map { .. } => false,
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if changed {
            self.machine.vm.heap.recharge_local(reference);
        }
        runtime_value(Value::Unit)
    }

    pub(super) fn runtime_map_probe(
        &mut self,
        reference: u64,
        semantic: u64,
        prior: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if let Err(fault) = self.machine.ensure_map_index(reference) {
            return RuntimeValueResult::Fault(fault);
        }
        let semantic = semantic as i64;
        let prior = prior as i64;
        let (epoch, mut prior_slot) = if prior == 0 {
            let epoch = match self.machine.vm.heap.get_mut(reference) {
                crate::Object::Map { index, .. } => index.epoch.observe(),
                _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
            };
            (epoch, None)
        } else {
            let (epoch, slot) = match map_probe_parts(prior) {
                Ok(parts) => parts,
                Err(fault) => return RuntimeValueResult::Fault(fault),
            };
            let current = match self.machine.vm.heap.get(reference) {
                crate::Object::Map { index, .. } => index.epoch.0,
                _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
            };
            if current != epoch {
                return RuntimeValueResult::Fault(crate::FaultCode::CollectionModified);
            }
            if slot.is_none() {
                return runtime_int(prior);
            }
            (epoch, slot)
        };
        let hash = Machine::map_index_hash(semantic);
        let slot = loop {
            let found = match self.machine.vm.heap.get(reference) {
                crate::Object::Map { index, .. } => index.probe(hash, prior_slot),
                _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
            };
            let Some((slot, entry)) = found else {
                break None;
            };
            let live = match self.machine.vm.heap.get(reference) {
                crate::Object::Map { entries, .. } => {
                    entries.get(entry as usize).is_some_and(MapEntry::is_live)
                }
                _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
            };
            if live {
                break Some(slot);
            }
            prior_slot = Some(slot);
        };
        match map_probe_token(epoch, slot) {
            Ok(token) => runtime_int(token),
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_map_probe_key(
        &mut self,
        reference: u64,
        token: u64,
    ) -> RuntimeValueResult {
        self.map_probe_entry_value(reference, token, false)
    }

    pub(super) fn runtime_map_probe_value(
        &mut self,
        reference: u64,
        token: u64,
    ) -> RuntimeValueResult {
        self.map_probe_entry_value(reference, token, true)
    }

    pub(super) fn runtime_map_probe_set_value(
        &mut self,
        reference: u64,
        token: u64,
        value_bits: u64,
        value_tag: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeValueResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(value) = tagged_value(value_tag, value_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let entry = match self.machine.map_token_entry(reference, token as i64) {
            Ok(Some(entry)) => entry,
            Ok(None) => return RuntimeValueResult::Fault(crate::FaultCode::MalformedState),
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        let replaced = match self.machine.vm.heap.get_mut(reference) {
            crate::Object::Map { entries, .. } => entries.get_mut(entry).map(|entry| {
                entry.value = value;
            }),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if replaced.is_none() {
            return RuntimeValueResult::Fault(crate::FaultCode::MalformedState);
        }
        runtime_value(Value::Unit)
    }

    pub(super) fn runtime_map_probe_remove(
        &mut self,
        reference: u64,
        token: u64,
    ) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeValueResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let entry = match self.machine.map_token_entry(reference, token as i64) {
            Ok(Some(entry)) => entry,
            Ok(None) => return RuntimeValueResult::Fault(crate::FaultCode::MalformedState),
            Err(fault) => return RuntimeValueResult::Fault(fault),
        };
        match self.machine.remove_map_entry(reference, entry) {
            Ok(value) => runtime_value(value),
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_map_insert_hashed(
        &mut self,
        request: MapInsertHashedRequest<'_>,
    ) -> RuntimeUnitResult {
        let reference = object_reference(request.reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeUnitResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(key) = tagged_value(request.key_tag, request.key_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Some(value) = tagged_value(request.value_tag, request.value_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        match self.machine.map_token_entry(reference, request.token) {
            Ok(None) => {}
            Ok(Some(_)) => return RuntimeUnitResult::Fault(crate::FaultCode::MalformedState),
            Err(fault) => return RuntimeUnitResult::Fault(fault),
        }
        if let Value::Obj(key_reference) = key {
            if let Err(fault) = lm_graph::verify_frozen(
                &mut self.machine.vm.heap,
                key_reference,
                &self.machine.config.graph,
            ) {
                return RuntimeUnitResult::Fault(match fault {
                    crate::FaultCode::UnsendableValue => crate::FaultCode::MutableMapKey,
                    other => other,
                });
            }
        }
        let entry_count = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => entries.len(),
            _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.insert_map_entry(MapInsertRequest {
            reference,
            key,
            value,
            semantic_hash: request.semantic_hash,
            entry_count,
            roots: request.roots,
            allow_collection: request.allow_collection,
            key_storage: MapInsertKeyStorage::Declared,
        })
    }

    pub(super) fn runtime_map_put_probe(
        &mut self,
        reference: u64,
        key_bits: u64,
        key_tag: u64,
    ) -> MapPutProbeResult {
        let reference = object_reference(reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return MapPutProbeResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return MapPutProbeResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(key) = tagged_value(key_tag, key_bits) else {
            return MapPutProbeResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let position = match self.machine.map_lookup(reference, key) {
            Ok(position) => position,
            Err(fault) => return MapPutProbeResult::Fault(fault),
        };
        if let Some(position) = position {
            let (previous, entry_count) = match self.machine.vm.heap.try_get(reference) {
                Some(crate::Object::Map { entries, .. }) => {
                    let Some(entry) = entries.get(position) else {
                        return MapPutProbeResult::Fault(crate::FaultCode::MalformedState);
                    };
                    (entry.value, entries.len())
                }
                _ => return MapPutProbeResult::Fault(crate::FaultCode::TypeMismatch),
            };
            let Some(bits) = value_bits(previous) else {
                return MapPutProbeResult::Fault(crate::FaultCode::TypeMismatch);
            };
            let Ok(position) = u32::try_from(position) else {
                return MapPutProbeResult::Fault(crate::FaultCode::MalformedState);
            };
            let Ok(entry_count) = u32::try_from(entry_count) else {
                return MapPutProbeResult::Fault(crate::FaultCode::MalformedState);
            };
            return MapPutProbeResult::Existing {
                position,
                entry_count,
                bits,
                tag: previous.tag() as u64,
            };
        }

        let semantic = match self.machine.key_semantic_hash(key) {
            Ok(semantic) => semantic,
            Err(fault) => return MapPutProbeResult::Fault(fault),
        };
        let entry_count = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => match u32::try_from(entries.len()) {
                Ok(count) => count,
                Err(_) => return MapPutProbeResult::Fault(crate::FaultCode::MalformedState),
            },
            _ => return MapPutProbeResult::Fault(crate::FaultCode::TypeMismatch),
        };
        MapPutProbeResult::Vacant {
            semantic_hash: semantic,
            entry_count,
        }
    }

    pub(super) fn runtime_map_put_discard(
        &mut self,
        request: MapPutDiscardRequest<'_>,
    ) -> RuntimeUnitResult {
        let reference = object_reference(request.reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeUnitResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(key) = tagged_value(request.key_tag, request.key_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Some(value) = tagged_value(request.value_tag, request.value_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let position = match self.machine.map_lookup(reference, key) {
            Ok(position) => position,
            Err(fault) => return RuntimeUnitResult::Fault(fault),
        };
        if let Some(position) = position {
            let replaced = match self.machine.vm.heap.get_mut(reference) {
                crate::Object::Map { entries, .. } => entries.get_mut(position).map(|entry| {
                    entry.value = value;
                }),
                _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
            };
            return if replaced.is_some() {
                RuntimeUnitResult::Done
            } else {
                RuntimeUnitResult::Fault(crate::FaultCode::MalformedState)
            };
        }
        let semantic_hash = match self.machine.key_semantic_hash(key) {
            Ok(hash) => hash,
            Err(fault) => return RuntimeUnitResult::Fault(fault),
        };
        let entry_count = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Map { entries, .. }) => entries.len(),
            _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.insert_map_entry(MapInsertRequest {
            reference,
            key,
            value,
            semantic_hash,
            entry_count,
            roots: request.roots,
            allow_collection: request.allow_collection,
            key_storage: if request.borrowed_string_key {
                MapInsertKeyStorage::BorrowedString
            } else {
                MapInsertKeyStorage::Declared
            },
        })
    }

    pub(super) fn runtime_map_put_commit(
        &mut self,
        request: MapPutCommitRequest<'_>,
    ) -> RuntimeUnitResult {
        let reference = object_reference(request.reference);
        if !matches!(
            self.machine.vm.heap.try_get(reference),
            Some(crate::Object::Map { .. })
        ) {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        }
        if self.machine.vm.heap.is_frozen(reference) {
            return RuntimeUnitResult::Fault(crate::FaultCode::FrozenWrite);
        }
        let Some(key) = tagged_value(request.key_tag, request.key_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Some(value) = tagged_value(request.value_tag, request.value_bits) else {
            return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Ok(entry_count) = usize::try_from(request.entry_count) else {
            return RuntimeUnitResult::Interpreter;
        };

        if !request.vacant {
            let Ok(expected) = usize::try_from(request.token) else {
                return RuntimeUnitResult::Interpreter;
            };
            let replaced = match self.machine.vm.heap.get_mut(reference) {
                crate::Object::Map { entries, .. } if entries.len() == entry_count => {
                    entries.get_mut(expected).and_then(|entry| {
                        entry
                            .is_live()
                            .then(|| std::mem::replace(&mut entry.value, value))
                    })
                }
                crate::Object::Map { .. } => return RuntimeUnitResult::Interpreter,
                _ => return RuntimeUnitResult::Fault(crate::FaultCode::TypeMismatch),
            };
            return if replaced.is_some() {
                RuntimeUnitResult::Done
            } else {
                RuntimeUnitResult::Fault(crate::FaultCode::MalformedState)
            };
        }

        self.insert_map_entry(MapInsertRequest {
            reference,
            key,
            value,
            semantic_hash: request.token as i64,
            entry_count,
            roots: request.roots,
            allow_collection: request.allow_collection,
            key_storage: if request.borrowed_string_key {
                MapInsertKeyStorage::BorrowedString
            } else {
                MapInsertKeyStorage::Declared
            },
        })
    }

    pub(super) fn runtime_reserve_map(
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
        let Some(crate::Object::Map { entries, index }) = self.machine.vm.heap.try_get(reference)
        else {
            return CollectionReserveResult::Interpreter;
        };
        if self.machine.vm.heap.is_frozen(reference) {
            return CollectionReserveResult::Interpreter;
        }
        if additional <= entries.capacity().saturating_sub(entries.len()) {
            return CollectionReserveResult::Done {
                heap: self.machine.vm.heap.jit_view(),
            };
        }
        if index.epoch.ensure_bumpable().is_err() {
            return CollectionReserveResult::Interpreter;
        }
        let Some(growth) = additional.checked_mul(40) else {
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
        let crate::Object::Map { entries, index } = self.machine.vm.heap.get_mut(reference) else {
            return CollectionReserveResult::Interpreter;
        };
        let before = entries.capacity();
        if entries.try_reserve(additional).is_err() {
            return CollectionReserveResult::HeapLimit;
        }
        if entries.capacity() != before && index.epoch.bump().is_err() {
            return CollectionReserveResult::Interpreter;
        }
        CollectionReserveResult::Done {
            heap: self.machine.vm.heap.jit_view(),
        }
    }

    pub(super) fn runtime_values_equal(
        &mut self,
        left_bits: u64,
        left_tag: u64,
        right_bits: u64,
        right_tag: u64,
    ) -> RuntimeValueResult {
        let Some(left) = tagged_value(left_tag, left_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        let Some(right) = tagged_value(right_tag, right_bits) else {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        };
        match self.machine.values_equal(self.module, left, right) {
            Ok(equal) => RuntimeValueResult::Value {
                bits: u64::from(equal),
                tag: ValueTag::Bool as u64,
            },
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_compare_text(&mut self, left: u64, right: u64) -> RuntimeValueResult {
        let left = object_reference(left);
        let right = object_reference(right);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (
                Some(crate::Object::Str(left) | crate::Object::Substring(left)),
                Some(crate::Object::Str(right) | crate::Object::Substring(right)),
            ) => (left, right),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_ordering(left.as_str().cmp(right.as_str()))
    }

    pub(super) fn runtime_compare_bytes(&mut self, left: u64, right: u64) -> RuntimeValueResult {
        let left = object_reference(left);
        let right = object_reference(right);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (Some(crate::Object::Bytes(left)), Some(crate::Object::Bytes(right))) => (left, right),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_ordering(left.as_slice().cmp(right.as_slice()))
    }

    pub(super) fn runtime_hash_text(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let hash = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => text.semantic_hash(),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_int(hash as i64)
    }

    pub(super) fn runtime_hash_bytes(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let hash = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Bytes(bytes)) => bytes.semantic_hash(),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_int(hash as i64)
    }

    pub(super) fn runtime_freeze_graph(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        if self.machine.vm.heap.try_get(reference).is_none() {
            return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
        }
        match lm_graph::freeze(
            &mut self.machine.vm.heap,
            reference,
            &self.machine.config.graph,
        ) {
            Ok(()) => runtime_value(Value::Obj(reference)),
            Err(fault) => RuntimeValueResult::Fault(fault),
        }
    }

    pub(super) fn runtime_digest_value(&mut self, request: DigestRequest<'_>) -> AllocationResult {
        let reference = object_reference(request.reference);
        if self.machine.vm.heap.try_get(reference).is_none() {
            return AllocationResult::Interpreter;
        }
        let bytes = crate::world::digest_typed_value(
            self.module,
            self.envs,
            &mut self.machine.vm.heap,
            reference,
            request.ty,
            TypeEnvId(request.environment),
            &self.machine.config.graph,
        );
        let Ok(bytes) = bytes else {
            return AllocationResult::Interpreter;
        };
        match self.allocate_object(
            crate::Object::NativeDigest(bytes),
            request.roots,
            request.allow_collection,
        ) {
            value @ AllocationResult::Value { .. } => value,
            AllocationResult::CollectionRequired
            | AllocationResult::HeapLimit
            | AllocationResult::Interpreter => AllocationResult::Interpreter,
        }
    }
}
