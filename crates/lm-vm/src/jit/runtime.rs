//! Typed native runtime slow paths.

use crate::machine::Machine;
use crate::NamespaceRuntime;
use lm_heap::{MapEntry, MapIndex, StructuralEpoch};
use lm_jit::{
    AllocationResult, CallbackAllocationRequest, CallbackAllocationResult,
    ClosureAllocationRequest, ListGrowthRequest, ListGrowthResult, ListReserveRequest,
    ListReserveResult, MapPutCommitRequest, MapPutDiscardRequest, MapPutProbeResult,
    NativeResolvedCallCache, NativeRuntime, NativeTypeEnvironmentCache, RuntimeUnitResult,
    RuntimeValueResult, ScalarKind, ValueArrayAllocationRequest, LOCAL_INITIALIZED,
};
use lm_value::{canonical_float_bits, CallbackRef, ObjRef, Value, ValueTag};

pub(super) fn scalar_parts(kind: ScalarKind, value: Value) -> Option<(u64, u64)> {
    let expected = match kind {
        ScalarKind::Unit => Some(ValueTag::Unit),
        ScalarKind::Bool => Some(ValueTag::Bool),
        ScalarKind::Int => Some(ValueTag::Int),
        ScalarKind::Float => Some(ValueTag::Float),
        ScalarKind::Char => Some(ValueTag::Char),
        ScalarKind::Object(_) => Some(ValueTag::Obj),
        ScalarKind::Tagged(_) => None,
        ScalarKind::Callback(_) => None,
        ScalarKind::Operation => Some(ValueTag::Op),
    };
    if expected.is_some_and(|tag| value.tag() != tag) {
        return None;
    }
    if matches!(kind, ScalarKind::Callback(_))
        && !matches!(value, Value::Obj(_) | Value::Callback(_))
    {
        return None;
    }
    let bits = value_bits(value)?;
    if matches!(kind, ScalarKind::Float) && canonical_float_bits(bits) != bits {
        return None;
    }
    Some((value.tag() as u64, bits))
}

pub(super) fn parts_value(kind: ScalarKind, tag: u64, bits: u64) -> Option<Value> {
    let value = tagged_value(tag, bits)?;
    scalar_parts(kind, value).map(|_| value)
}

pub(super) fn value_bits(value: Value) -> Option<u64> {
    Some(match value {
        Value::Unit => 0,
        Value::Bool(value) => u64::from(value),
        Value::Int(value) => value as u64,
        Value::Float(bits) => bits,
        Value::Char(value) => u64::from(u32::from(value)),
        Value::Obj(reference) => object_bits(reference),
        Value::Op(operation) => u64::from(operation),
        Value::Callback(reference) => reference_bits(reference),
        Value::EmptyCase { ty, arm } => u64::from(ty) | (u64::from(arm) << 32),
        Value::Uninit => return None,
    })
}

pub(super) fn tagged_value(tag: u64, bits: u64) -> Option<Value> {
    Some(match tag {
        tag if tag == ValueTag::Unit as u64 && bits == 0 => Value::Unit,
        tag if tag == ValueTag::Bool as u64 && bits <= 1 => Value::Bool(bits != 0),
        tag if tag == ValueTag::Int as u64 => Value::Int(bits as i64),
        tag if tag == ValueTag::Float as u64 => Value::Float(bits),
        tag if tag == ValueTag::Char as u64 && bits <= u64::from(u32::MAX) => {
            Value::Char(char::from_u32(bits as u32)?)
        }
        tag if tag == ValueTag::Obj as u64 => Value::Obj(object_reference(bits)),
        tag if tag == ValueTag::Op as u64 && bits <= u64::from(u32::MAX) => Value::Op(bits as u32),
        tag if tag == ValueTag::Callback as u64 => Value::Callback(callback_reference(bits)),
        tag if tag == ValueTag::EmptyCase as u64 => Value::EmptyCase {
            ty: bits as u32,
            arm: (bits >> 32) as u32,
        },
        _ => return None,
    })
}

fn object_bits(reference: ObjRef) -> u64 {
    u64::from(reference.slot) | (u64::from(reference.generation) << 32)
}

fn object_reference(bits: u64) -> ObjRef {
    ObjRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

fn reference_bits(reference: CallbackRef) -> u64 {
    u64::from(reference.slot) | (u64::from(reference.generation) << 32)
}

fn callback_reference(bits: u64) -> CallbackRef {
    CallbackRef {
        slot: bits as u32,
        generation: (bits >> 32) as u32,
    }
}

pub(super) struct MachineRuntime<'a> {
    pub(super) machine: &'a mut Machine,
    pub(super) type_environments: NativeTypeEnvironmentCache,
    pub(super) resolved_calls: NativeResolvedCallCache,
    pub(super) module: &'a NamespaceRuntime,
    pub(super) base_local: usize,
    pub(super) base_operand: usize,
    pub(super) allocations: u64,
}

impl Drop for MachineRuntime<'_> {
    fn drop(&mut self) {
        self.machine.native_type_environments = std::mem::take(&mut self.type_environments);
        self.machine.native_resolved_calls = std::mem::take(&mut self.resolved_calls);
    }
}

enum CaptureDecodeFailure {
    Limit,
    Invalid,
}

fn decode_captures(bits: &[u64], tags: &[u64]) -> Result<Vec<Value>, CaptureDecodeFailure> {
    if bits.len() != tags.len() {
        return Err(CaptureDecodeFailure::Invalid);
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(bits.len())
        .map_err(|_| CaptureDecodeFailure::Limit)?;
    for (&bits, &tag) in bits.iter().zip(tags) {
        values.push(tagged_value(tag, bits).ok_or(CaptureDecodeFailure::Invalid)?);
    }
    Ok(values)
}

fn runtime_value(value: Value) -> RuntimeValueResult {
    let Some(bits) = value_bits(value) else {
        return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch);
    };
    RuntimeValueResult::Value {
        bits,
        tag: value.tag() as u64,
    }
}

fn decode_root_objects(
    bits: &[u64],
    tags: &[u64],
    states: &[u8],
) -> Result<Vec<ObjRef>, CaptureDecodeFailure> {
    if bits.len() != tags.len() || bits.len() != states.len() {
        return Err(CaptureDecodeFailure::Invalid);
    }
    let mut roots = Vec::new();
    roots
        .try_reserve_exact(bits.len())
        .map_err(|_| CaptureDecodeFailure::Limit)?;
    roots.extend(
        bits.iter()
            .copied()
            .zip(tags.iter().copied())
            .zip(states.iter().copied())
            .filter(|((_, tag), state)| {
                *tag == ValueTag::Obj as u64 && state & LOCAL_INITIALIZED != 0
            })
            .map(|((bits, _), _)| object_reference(bits)),
    );
    Ok(roots)
}

struct MapInsertRequest<'a> {
    reference: ObjRef,
    key: Value,
    value: Value,
    semantic_hash: i64,
    entry_count: usize,
    root_bits: &'a [u64],
    root_tags: &'a [u64],
    root_states: &'a [u8],
    allow_collection: bool,
}

impl MachineRuntime<'_> {
    fn allocate_object(
        &mut self,
        object: crate::Object,
        root_bits: &[u64],
        root_tags: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> AllocationResult {
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return AllocationResult::Interpreter;
        }
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            let heap = if reference.slot & lm_heap::JIT_PAGE_MASK == 0 {
                Some(self.machine.vm.heap.jit_view())
            } else {
                None
            };
            return AllocationResult::Value {
                bits: object_bits(reference),
                heap,
            };
        }
        if !allow_collection {
            return AllocationResult::Interpreter;
        }
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return AllocationResult::HeapLimit;
        }
        roots.extend(
            root_bits
                .iter()
                .copied()
                .zip(root_tags.iter().copied())
                .zip(root_states.iter().copied())
                .filter(|((_, tag), state)| {
                    *tag == ValueTag::Obj as u64 && state & LOCAL_INITIALIZED != 0
                })
                .map(|((bits, _), _)| object_reference(bits)),
        );
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                AllocationResult::Value {
                    bits: object_bits(reference),
                    heap: Some(self.machine.vm.heap.jit_view()),
                }
            }
            Ok(_) => AllocationResult::Interpreter,
            Err(crate::FaultCode::HeapLimit) => AllocationResult::HeapLimit,
            Err(_) => AllocationResult::Interpreter,
        }
    }

    fn insert_map_entry(&mut self, request: MapInsertRequest<'_>) -> RuntimeUnitResult {
        let MapInsertRequest {
            reference,
            key,
            value,
            semantic_hash,
            entry_count,
            root_bits,
            root_tags,
            root_states,
            allow_collection,
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
        if self.machine.vm.heap.collection_due(40) && !allow_collection {
            return RuntimeUnitResult::Interpreter;
        }
        let roots = match decode_root_objects(root_bits, root_tags, root_states) {
            Ok(roots) => roots,
            Err(CaptureDecodeFailure::Limit) => {
                return RuntimeUnitResult::Fault(crate::FaultCode::HeapLimit);
            }
            Err(CaptureDecodeFailure::Invalid) => return RuntimeUnitResult::Interpreter,
        };
        if let Err(fault) =
            self.machine
                .reserve_native(40, self.base_local, self.base_operand, &roots)
        {
            return RuntimeUnitResult::Fault(fault);
        }
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
}

impl NativeRuntime for MachineRuntime<'_> {
    fn allocate_instance(
        &mut self,
        class: u32,
        environment: u32,
        root_bits: &[u64],
        root_tags: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> AllocationResult {
        let Some(class_entry) = self.module.classes.get(class as usize) else {
            return AllocationResult::Interpreter;
        };
        let object = crate::Object::Instance {
            class,
            fields: vec![Value::Uninit; class_entry.fields.len()].into(),
            env: lm_value::Witness(lm_value::TypeEnvId(environment)),
        };
        self.allocate_object(object, root_bits, root_tags, root_states, allow_collection)
    }

    fn allocate_closure(&mut self, request: ClosureAllocationRequest<'_>) -> AllocationResult {
        let captures = match decode_captures(request.capture_bits, request.capture_tags) {
            Ok(captures) => captures,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        let object = crate::Object::Closure {
            func: request.function,
            captures: captures.into(),
            env: lm_value::Witness(lm_value::TypeEnvId(request.environment)),
        };
        self.allocate_object(
            object,
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        )
    }

    fn allocate_callback(
        &mut self,
        request: CallbackAllocationRequest<'_>,
    ) -> CallbackAllocationResult {
        let captures = match decode_captures(request.capture_bits, request.capture_tags) {
            Ok(captures) => captures,
            Err(CaptureDecodeFailure::Limit) => return CallbackAllocationResult::StackLimit,
            Err(CaptureDecodeFailure::Invalid) => {
                return CallbackAllocationResult::Interpreter;
            }
        };
        match self.machine.alloc_callback_native(
            request.function,
            captures,
            lm_value::TypeEnvId(request.environment),
            request.owner_depth,
        ) {
            Ok(Value::Callback(reference)) => CallbackAllocationResult::Value {
                bits: reference_bits(reference),
            },
            Ok(_) => CallbackAllocationResult::Interpreter,
            Err(crate::FaultCode::StackLimit) => CallbackAllocationResult::StackLimit,
            Err(_) => CallbackAllocationResult::Interpreter,
        }
    }

    fn allocate_tuple(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        let items = match decode_captures(request.item_bits, request.item_tags) {
            Ok(items) => items,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        self.allocate_object(
            crate::Object::Tuple {
                items: items.into(),
            },
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        )
    }

    fn allocate_list(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        let items = match decode_captures(request.item_bits, request.item_tags) {
            Ok(items) => items,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        self.allocate_object(
            crate::Object::List {
                items: items.into(),
                epoch: StructuralEpoch::default(),
            },
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        )
    }

    fn allocate_map(&mut self, request: ValueArrayAllocationRequest<'_>) -> AllocationResult {
        let flat = match decode_captures(request.item_bits, request.item_tags) {
            Ok(flat) => flat,
            Err(CaptureDecodeFailure::Limit) => return AllocationResult::HeapLimit,
            Err(CaptureDecodeFailure::Invalid) => return AllocationResult::Interpreter,
        };
        if flat.len() % 2 != 0 {
            return AllocationResult::Interpreter;
        }
        let mut entries: Vec<MapEntry> = Vec::new();
        let mut index = MapIndex::default();
        if entries.try_reserve_exact(flat.len() / 2).is_err() {
            return AllocationResult::HeapLimit;
        }
        for pair in flat.chunks_exact(2) {
            let key = pair[0];
            let value = pair[1];
            let semantic = match self.machine.key_semantic_hash(key) {
                Ok(semantic) => semantic,
                Err(_) => return AllocationResult::Interpreter,
            };
            let hash = Machine::map_index_hash(semantic);
            let hit = index
                .candidates(hash)
                .find(|position| self.machine.key_eq(entries[*position as usize].key, key));
            match hit {
                Some(position) => entries[position as usize].value = value,
                None => {
                    index.push_live(hash, entries.len() as u32);
                    entries.push(MapEntry {
                        key,
                        value,
                        semantic_hash: semantic,
                    });
                }
            }
        }
        self.allocate_object(
            crate::Object::Map { entries, index },
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        )
    }

    fn map_has(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
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

    fn map_at(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
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

    fn map_put_probe(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> MapPutProbeResult {
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

    fn map_put_discard(&mut self, request: MapPutDiscardRequest<'_>) -> RuntimeUnitResult {
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
            root_bits: request.root_bits,
            root_tags: request.root_tags,
            root_states: request.root_states,
            allow_collection: request.allow_collection,
        })
    }

    fn map_put_commit(&mut self, request: MapPutCommitRequest<'_>) -> RuntimeUnitResult {
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
            root_bits: request.root_bits,
            root_tags: request.root_tags,
            root_states: request.root_states,
            allow_collection: request.allow_collection,
        })
    }

    fn grow_list(&mut self, request: ListGrowthRequest<'_>) -> ListGrowthResult {
        let ListGrowthRequest {
            reference,
            value_bits,
            value_tag,
            root_bits,
            root_tags,
            root_states,
            allow_collection,
        } = request;
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return ListGrowthResult::Interpreter;
        }
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
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return ListGrowthResult::HeapLimit;
        }
        roots.extend(
            root_bits
                .iter()
                .copied()
                .zip(root_tags.iter().copied())
                .zip(root_states.iter().copied())
                .filter(|((_, tag), state)| {
                    *tag == ValueTag::Obj as u64 && state & LOCAL_INITIALIZED != 0
                })
                .map(|((bits, _), _)| object_reference(bits)),
        );
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

    fn reserve_list(&mut self, request: ListReserveRequest<'_>) -> ListReserveResult {
        let ListReserveRequest {
            reference,
            additional,
            root_bits,
            root_tags,
            root_states,
            allow_collection,
        } = request;
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return ListReserveResult::Interpreter;
        }
        let Ok(additional) = usize::try_from(additional) else {
            return ListReserveResult::Interpreter;
        };
        let reference = object_reference(reference);
        let Some(crate::Object::List { items, epoch }) = self.machine.vm.heap.try_get(reference)
        else {
            return ListReserveResult::Interpreter;
        };
        if self.machine.vm.heap.is_frozen(reference) {
            return ListReserveResult::Interpreter;
        }
        if additional <= items.capacity().saturating_sub(items.len()) {
            return ListReserveResult::Done {
                heap: self.machine.vm.heap.jit_view(),
            };
        }
        if epoch.ensure_bumpable().is_err() {
            return ListReserveResult::Interpreter;
        }
        let Some(growth) = additional.checked_mul(std::mem::size_of::<Value>()) else {
            return ListReserveResult::HeapLimit;
        };
        if self.machine.vm.heap.collection_due(growth) && !allow_collection {
            return ListReserveResult::Interpreter;
        }
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return ListReserveResult::HeapLimit;
        }
        roots.extend(
            root_bits
                .iter()
                .copied()
                .zip(root_tags.iter().copied())
                .zip(root_states.iter().copied())
                .filter(|((_, tag), state)| {
                    *tag == ValueTag::Obj as u64 && state & LOCAL_INITIALIZED != 0
                })
                .map(|((bits, _), _)| object_reference(bits)),
        );
        if let Err(fault) =
            self.machine
                .reserve_native(growth, self.base_local, self.base_operand, &roots)
        {
            return if fault == crate::FaultCode::HeapLimit {
                ListReserveResult::HeapLimit
            } else {
                ListReserveResult::Interpreter
            };
        }
        let crate::Object::List { items, epoch } = self.machine.vm.heap.get_mut(reference) else {
            return ListReserveResult::Interpreter;
        };
        let before = items.capacity();
        if items.try_reserve(additional).is_err() {
            return ListReserveResult::HeapLimit;
        }
        if items.capacity() != before && epoch.bump().is_err() {
            return ListReserveResult::Interpreter;
        }
        ListReserveResult::Done {
            heap: self.machine.vm.heap.jit_view(),
        }
    }
}
