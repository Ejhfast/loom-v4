//! Typed native runtime slow paths.

use crate::machine::{
    float_text, integer_text_len, map_probe_parts, map_probe_token, parse_float_text, Machine,
};
use crate::NamespaceRuntime;
use lm_heap::{
    MapEntry, MapIndex, NativeByteBuffer, NativeStringBuilder, SharedBytes, SharedText,
    StructuralEpoch,
};
use lm_jit::{
    AllocationResult, CallbackAllocationRequest, CallbackAllocationResult,
    ClosureAllocationRequest, CollectionReserveRequest, CollectionReserveResult, DigestRequest,
    HeapOperationRequest, HeapOperationResult, ListGrowthRequest, ListGrowthResult,
    ListInsertRequest, MapInsertHashedRequest, MapPutCommitRequest, MapPutDiscardRequest,
    MapPutProbeResult, NativeResolvedCallCache, NativeRuntime, NativeTypeEnvironmentCache,
    RuntimeUnitResult, RuntimeValueResult, ScalarKind, ValueArrayAllocationRequest,
    LOCAL_INITIALIZED,
};
use lm_value::{canonical_float_bits, CallbackRef, ObjRef, TypeEnvId, Value, ValueTag};
use std::fmt::Write;

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
    pub(super) envs: &'a mut lm_bytecode::closed::TypeEnvs,
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

    fn allocate_heap_object(
        &mut self,
        object: crate::Object,
        request: &HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        match self.allocate_object(
            object,
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        ) {
            AllocationResult::Value { bits, heap } => HeapOperationResult::Value { bits, heap },
            AllocationResult::HeapLimit => HeapOperationResult::HeapLimit,
            AllocationResult::Interpreter => HeapOperationResult::Interpreter,
        }
    }

    fn allocate_heap_reference(
        &mut self,
        object: crate::Object,
        request: &HeapOperationRequest<'_>,
        extra_roots: &[ObjRef],
    ) -> Result<ObjRef, HeapOperationResult> {
        if request.root_bits.len() != request.root_tags.len()
            || request.root_bits.len() != request.root_states.len()
        {
            return Err(HeapOperationResult::Interpreter);
        }
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            return Ok(reference);
        }
        if !request.allow_collection {
            return Err(HeapOperationResult::Interpreter);
        }
        let mut roots =
            match decode_root_objects(request.root_bits, request.root_tags, request.root_states) {
                Ok(roots) => roots,
                Err(CaptureDecodeFailure::Limit) => return Err(HeapOperationResult::HeapLimit),
                Err(CaptureDecodeFailure::Invalid) => {
                    return Err(HeapOperationResult::Interpreter);
                }
            };
        if roots.try_reserve_exact(extra_roots.len()).is_err() {
            return Err(HeapOperationResult::HeapLimit);
        }
        roots.extend_from_slice(extra_roots);
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                Ok(reference)
            }
            Ok(_) => Err(HeapOperationResult::Interpreter),
            Err(crate::FaultCode::HeapLimit) => Err(HeapOperationResult::HeapLimit),
            Err(fault) => Err(HeapOperationResult::Fault(fault)),
        }
    }

    fn reserve_heap_growth(
        &mut self,
        growth: usize,
        request: &HeapOperationRequest<'_>,
    ) -> Result<(), HeapOperationResult> {
        if growth == 0 {
            return Ok(());
        }
        if self.machine.vm.heap.collection_due(growth) && !request.allow_collection {
            return Err(HeapOperationResult::Interpreter);
        }
        let roots =
            match decode_root_objects(request.root_bits, request.root_tags, request.root_states) {
                Ok(roots) => roots,
                Err(CaptureDecodeFailure::Limit) => return Err(HeapOperationResult::HeapLimit),
                Err(CaptureDecodeFailure::Invalid) => {
                    return Err(HeapOperationResult::Interpreter);
                }
            };
        match self
            .machine
            .reserve_native(growth, self.base_local, self.base_operand, &roots)
        {
            Ok(()) => Ok(()),
            Err(crate::FaultCode::HeapLimit) => Err(HeapOperationResult::HeapLimit),
            Err(fault) => Err(HeapOperationResult::Fault(fault)),
        }
    }

    fn heap_object_value(reference: ObjRef) -> HeapOperationResult {
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: None,
        }
    }

    fn string_builder_growth(
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

    fn byte_buffer_growth(
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

    fn append_builder_text(
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

    fn bytes_binary(
        &mut self,
        request: HeapOperationRequest<'_>,
        operation: fn(u8, u8) -> u8,
    ) -> HeapOperationResult {
        let left = object_reference(request.first);
        let right = object_reference(request.second);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (Some(crate::Object::Bytes(left)), Some(crate::Object::Bytes(right))) => {
                (left.clone(), right.clone())
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if left.len() != right.len() {
            return HeapOperationResult::Fault(crate::FaultCode::LengthMismatch);
        }
        if let Err(result) = self.reserve_heap_growth(left.len(), &request) {
            return result;
        }
        let mut output = Vec::new();
        if output.try_reserve_exact(left.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(
            left.as_slice()
                .iter()
                .copied()
                .zip(right.as_slice().iter().copied())
                .map(|(left, right)| operation(left, right)),
        );
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
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

    fn map_entry_value(&self, reference: u64, index: u64, load_value: bool) -> RuntimeValueResult {
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

    fn map_probe_entry_value(
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

    fn list_contains(
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

    fn map_get(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
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

    fn map_next_index(&mut self, reference: u64, cursor: u64, expected: u64) -> RuntimeValueResult {
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

    fn map_key_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult {
        self.map_entry_value(reference, index, false)
    }

    fn map_value_at(&mut self, reference: u64, index: u64) -> RuntimeValueResult {
        self.map_entry_value(reference, index, true)
    }

    fn map_remove(&mut self, reference: u64, key_bits: u64, key_tag: u64) -> RuntimeValueResult {
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

    fn map_clear(&mut self, reference: u64) -> RuntimeValueResult {
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

    fn map_probe(&mut self, reference: u64, semantic: u64, prior: u64) -> RuntimeValueResult {
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

    fn map_probe_key(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
        self.map_probe_entry_value(reference, token, false)
    }

    fn map_probe_value(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
        self.map_probe_entry_value(reference, token, true)
    }

    fn map_probe_set_value(
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

    fn map_probe_remove(&mut self, reference: u64, token: u64) -> RuntimeValueResult {
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

    fn map_insert_hashed(&mut self, request: MapInsertHashedRequest<'_>) -> RuntimeUnitResult {
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
            root_bits: request.root_bits,
            root_tags: request.root_tags,
            root_states: request.root_states,
            allow_collection: request.allow_collection,
        })
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

    fn insert_list(&mut self, request: ListInsertRequest<'_>) -> ListGrowthResult {
        let ListInsertRequest {
            reference,
            index,
            value_bits,
            value_tag,
            root_bits,
            root_tags,
            root_states,
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
        let roots = match decode_root_objects(root_bits, root_tags, root_states) {
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

    fn reserve_list(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        let CollectionReserveRequest {
            reference,
            additional,
            root_bits,
            root_tags,
            root_states,
            allow_collection,
        } = request;
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return CollectionReserveResult::Interpreter;
        }
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
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return CollectionReserveResult::HeapLimit;
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

    fn reserve_map(&mut self, request: CollectionReserveRequest<'_>) -> CollectionReserveResult {
        let CollectionReserveRequest {
            reference,
            additional,
            root_bits,
            root_tags,
            root_states,
            allow_collection,
        } = request;
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return CollectionReserveResult::Interpreter;
        }
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
        let roots = match decode_root_objects(root_bits, root_tags, root_states) {
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

    fn values_equal(
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

    fn compare_text(&mut self, left: u64, right: u64) -> RuntimeValueResult {
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

    fn compare_bytes(&mut self, left: u64, right: u64) -> RuntimeValueResult {
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

    fn hash_text(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let hash = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => text.semantic_hash(),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_int(hash as i64)
    }

    fn hash_bytes(&mut self, reference: u64) -> RuntimeValueResult {
        let reference = object_reference(reference);
        let hash = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Bytes(bytes)) => bytes.semantic_hash(),
            _ => return RuntimeValueResult::Fault(crate::FaultCode::TypeMismatch),
        };
        runtime_int(hash as i64)
    }

    fn freeze_graph(&mut self, reference: u64) -> RuntimeValueResult {
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

    fn digest_value(&mut self, request: DigestRequest<'_>) -> AllocationResult {
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
            request.root_bits,
            request.root_tags,
            request.root_states,
            request.allow_collection,
        ) {
            value @ AllocationResult::Value { .. } => value,
            AllocationResult::HeapLimit | AllocationResult::Interpreter => {
                AllocationResult::Interpreter
            }
        }
    }

    fn string_builder_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.allocate_heap_object(
            crate::Object::StrBuilder(NativeStringBuilder::new()),
            &request,
        )
    }

    fn string_builder_append_text(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let builder = object_reference(request.first);
        let source = object_reference(request.second);
        let text_len = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => text.len(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
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

    fn string_builder_append_int(
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

    fn string_builder_append_bool(
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

    fn string_builder_append_char(
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

    fn string_builder_append_float(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let text = float_text(f64::from_bits(request.second));
        self.append_builder_text(request, &text)
    }

    fn string_builder_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn string_builder_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        if !request.allow_collection {
            return HeapOperationResult::Interpreter;
        }
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

    fn byte_buffer_new(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.allocate_heap_object(crate::Object::ByteBuf(NativeByteBuffer::new()), &request)
    }

    fn byte_buffer_append(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn byte_buffer_build(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn byte_buffer_extend(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn byte_buffer_reserve(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn byte_buffer_finish(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        if !request.allow_collection {
            return HeapOperationResult::Interpreter;
        }
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

    fn bytes_from_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let source = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Str(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Bytes(text.bytes()), &request)
    }

    fn bytes_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let source = object_reference(request.first);
        let Ok(start) = usize::try_from(request.second as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let Ok(length) = usize::try_from(request.third as i64) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let Some(end) = start.checked_add(length) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => match bytes.slice(start, end) {
                Some(bytes) => bytes,
                None => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    fn bytes_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let left = object_reference(request.first);
        let right = object_reference(request.second);
        let (left, right) = match (
            self.machine.vm.heap.try_get(left),
            self.machine.vm.heap.try_get(right),
        ) {
            (Some(crate::Object::Bytes(left)), Some(crate::Object::Bytes(right))) => {
                (left.clone(), right.clone())
            }
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let Some(length) = left.len().checked_add(right.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let bytes = match left.try_concat(&right) {
            Ok(bytes) => bytes,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    fn bytes_compact(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let source = object_reference(request.first);
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => bytes.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(bytes.len(), &request) {
            return result;
        }
        let bytes = match bytes.try_compact() {
            Ok(bytes) => bytes,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Bytes(bytes), &request)
    }

    fn bytes_text_view(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let source = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => match bytes.utf8_view() {
                Some(text) => text,
                None => return HeapOperationResult::Fault(crate::FaultCode::BadCast),
            },
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        self.allocate_heap_object(crate::Object::Substring(text), &request)
    }

    fn bytes_bit_and(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left & right)
    }

    fn bytes_bit_or(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left | right)
    }

    fn bytes_bit_xor(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_binary(request, |left, right| left ^ right)
    }

    fn bytes_bit_not(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let source = object_reference(request.first);
        let bytes = match self.machine.vm.heap.try_get(source) {
            Some(crate::Object::Bytes(bytes)) => bytes.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if let Err(result) = self.reserve_heap_growth(bytes.len(), &request) {
            return result;
        }
        let mut output = Vec::new();
        if output.try_reserve_exact(bytes.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(bytes.as_slice().iter().map(|value| !value));
        self.allocate_heap_object(crate::Object::Bytes(SharedBytes::from(output)), &request)
    }

    fn text_concat(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_concat_operation(request)
    }

    fn text_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, prefix| text.starts_with(prefix))
    }

    fn text_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, suffix| text.ends_with(suffix))
    }

    fn text_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_predicate_operation(request, |text, needle| text.contains(needle))
    }

    fn text_find_scalar(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_find_operation(request, true)
    }

    fn text_find_byte(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_find_operation(request, false)
    }

    fn text_trim(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_trim_operation(request, true, true)
    }

    fn text_trim_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_trim_operation(request, true, false)
    }

    fn text_trim_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_trim_operation(request, false, true)
    }

    fn text_lower_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_ascii_operation(request, true)
    }

    fn text_upper_ascii(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_ascii_operation(request, false)
    }

    fn text_replace(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_replace_operation(request)
    }

    fn text_parse_int_status(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_parse_int_operation(request, true)
    }

    fn text_parse_int_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_parse_int_operation(request, false)
    }

    fn text_pad_start(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_pad_operation(request, true)
    }

    fn text_pad_end(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_pad_operation(request, false)
    }

    fn bytes_ends_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_predicate_operation(request, <[u8]>::ends_with)
    }

    fn bytes_contains(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_contains_operation(request)
    }

    fn text_split(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_split_operation(request, true)
    }

    fn text_lines(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_split_operation(request, false)
    }

    fn text_slice(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_slice_operation(request, true)
    }

    fn text_slice_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_slice_operation(request, false)
    }

    fn text_bytes(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_bytes_operation(request)
    }

    fn text_to_string(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_to_string_operation(request)
    }

    fn bytes_text(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_text_operation(request)
    }

    fn byte_buffer_find_from(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.byte_buffer_find_operation(request)
    }

    fn bytes_starts_with(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_predicate_operation(request, <[u8]>::starts_with)
    }

    fn bytes_find_index(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_find_operation(request)
    }

    fn bytes_hex(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_hex_operation(request)
    }

    fn bytes_is_utf8(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.bytes_is_utf8_operation(request)
    }

    fn text_parse_float_status(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        self.text_parse_float_operation(request, true)
    }

    fn text_parse_float_value(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.text_parse_float_operation(request, false)
    }

    fn float_fixed(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        self.float_fixed_operation(request)
    }
}

impl MachineRuntime<'_> {
    fn text_pair(
        &self,
        first: u64,
        second: u64,
    ) -> Result<(SharedText, SharedText), HeapOperationResult> {
        let first = object_reference(first);
        let second = object_reference(second);
        match (
            self.machine.vm.heap.try_get(first),
            self.machine.vm.heap.try_get(second),
        ) {
            (
                Some(crate::Object::Str(first) | crate::Object::Substring(first)),
                Some(crate::Object::Str(second) | crate::Object::Substring(second)),
            ) => Ok((first.clone(), second.clone())),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn bytes_pair(
        &self,
        first: u64,
        second: u64,
    ) -> Result<(SharedBytes, SharedBytes), HeapOperationResult> {
        let first = object_reference(first);
        let second = object_reference(second);
        match (
            self.machine.vm.heap.try_get(first),
            self.machine.vm.heap.try_get(second),
        ) {
            (Some(crate::Object::Bytes(first)), Some(crate::Object::Bytes(second))) => {
                Ok((first.clone(), second.clone()))
            }
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn text_value(&self, reference: u64) -> Result<SharedText, HeapOperationResult> {
        let reference = object_reference(reference);
        match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => Ok(text.clone()),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn bytes_value(&self, reference: u64) -> Result<SharedBytes, HeapOperationResult> {
        let reference = object_reference(reference);
        match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Bytes(bytes)) => Ok(bytes.clone()),
            _ => Err(HeapOperationResult::Fault(crate::FaultCode::TypeMismatch)),
        }
    }

    fn text_concat_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let (left, right) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let Some(length) = left.len().checked_add(right.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let text = match left.try_concat(&right) {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    fn text_predicate_operation(
        &self,
        request: HeapOperationRequest<'_>,
        predicate: fn(&str, &str) -> bool,
    ) -> HeapOperationResult {
        let (text, argument) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        heap_bool(predicate(text.as_str(), argument.as_str()))
    }

    fn text_find_operation(
        &self,
        request: HeapOperationRequest<'_>,
        scalar: bool,
    ) -> HeapOperationResult {
        let (text, needle) = match self.text_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let found = if scalar {
            text.find_scalar(&needle)
        } else {
            text.find_byte(&needle)
        };
        let value = match found {
            Some(index) => match i64::try_from(index) {
                Ok(index) => index,
                Err(_) => {
                    return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
                }
            },
            None => -1,
        };
        heap_int(value)
    }

    fn text_trim_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        trim_start: bool,
        trim_end: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let source = text.as_str();
        let start = if trim_start {
            source.len() - source.trim_start().len()
        } else {
            0
        };
        let end = if trim_end {
            source.trim_end().len()
        } else {
            source.len()
        }
        .max(start);
        let Some(slice) = text.slice(start, end) else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        self.allocate_heap_object(crate::Object::Substring(slice), &request)
    }

    fn text_ascii_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        lower: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let length = text.len();
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        output.extend(text.as_str().chars().map(|value| {
            if lower {
                value.to_ascii_lowercase()
            } else {
                value.to_ascii_uppercase()
            }
        }));
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    fn text_replace_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let needle = match self.text_value(request.second) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let replacement = match self.text_value(request.third) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let source = text.as_str();
        let needle_text = needle.as_str();
        let replacement_text = replacement.as_str();
        let matches = source.match_indices(needle_text).count();
        let Some(removed) = matches.checked_mul(needle_text.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        let Some(added) = matches.checked_mul(replacement_text.len()) else {
            return HeapOperationResult::HeapLimit;
        };
        let Some(length) = source
            .len()
            .checked_sub(removed)
            .and_then(|kept| kept.checked_add(added))
        else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        let mut cursor = 0;
        for (at, matched) in source.match_indices(needle_text) {
            output.push_str(&source[cursor..at]);
            output.push_str(replacement_text);
            cursor = at + matched.len();
        }
        output.push_str(&source[cursor..]);
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    fn text_parse_int_operation(
        &self,
        request: HeapOperationRequest<'_>,
        status: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let radix = u32::try_from(request.second as i64)
            .ok()
            .filter(|radix| (2..=36).contains(radix));
        let Some(radix) = radix else {
            return heap_int(if status { 3 } else { 0 });
        };
        let parsed = i64::from_str_radix(text.as_str(), radix);
        let answer = match (status, parsed) {
            (true, Ok(_)) => 0,
            (true, Err(error)) => match error.kind() {
                std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => 2,
                _ => 1,
            },
            (false, Ok(value)) => value,
            (false, Err(_)) => 0,
        };
        heap_int(answer)
    }

    fn text_pad_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        before: bool,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(text) | crate::Object::Substring(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        let scalar_length = match i64::try_from(text.char_count()) {
            Ok(length) => length,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow),
        };
        let padding = (request.second as i64).saturating_sub(scalar_length);
        if padding <= 0
            && matches!(
                self.machine.vm.heap.try_get(reference),
                Some(crate::Object::Str(_))
            )
        {
            return heap_bits(request.first);
        }
        let padding = match usize::try_from(padding.max(0)) {
            Ok(padding) => padding,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let Some(length) = text.len().checked_add(padding) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        if before {
            output.extend(std::iter::repeat_n(' ', padding));
        }
        output.push_str(text.as_str());
        if !before {
            output.extend(std::iter::repeat_n(' ', padding));
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    fn bytes_predicate_operation(
        &self,
        request: HeapOperationRequest<'_>,
        predicate: fn(&[u8], &[u8]) -> bool,
    ) -> HeapOperationResult {
        let (bytes, argument) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        heap_bool(predicate(bytes.as_slice(), argument.as_slice()))
    }

    fn bytes_contains_operation(&self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let (bytes, needle) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let needle = needle.as_slice();
        heap_bool(
            needle.is_empty()
                || bytes
                    .as_slice()
                    .windows(needle.len())
                    .any(|window| window == needle),
        )
    }

    fn text_split_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        split: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let separator = if split {
            match self.text_value(request.second) {
                Ok(separator) => Some(separator),
                Err(result) => return result,
            }
        } else {
            None
        };
        let visible = text.as_str();
        let mut ranges = Vec::new();
        match separator.as_ref() {
            Some(separator) if separator.as_str().is_empty() => {
                let Some(count) = text.char_count().checked_add(2) else {
                    return HeapOperationResult::HeapLimit;
                };
                if ranges.try_reserve_exact(count).is_err() {
                    return HeapOperationResult::HeapLimit;
                }
                ranges.push((0, 0));
                let mut start = 0;
                for (at, scalar) in visible.char_indices() {
                    ranges.push((at, at + scalar.len_utf8()));
                    start = at + scalar.len_utf8();
                }
                ranges.push((start, visible.len()));
            }
            Some(separator) => {
                let needle = separator.as_str();
                let Some(count) = visible.match_indices(needle).count().checked_add(1) else {
                    return HeapOperationResult::HeapLimit;
                };
                if ranges.try_reserve_exact(count).is_err() {
                    return HeapOperationResult::HeapLimit;
                }
                let mut start = 0;
                while let Some(at) = visible[start..].find(needle) {
                    let at = start + at;
                    ranges.push((start, at));
                    start = at + needle.len();
                }
                ranges.push((start, visible.len()));
            }
            None => {
                let count = visible
                    .as_bytes()
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
                    .saturating_add(1);
                if ranges.try_reserve_exact(count).is_err() {
                    return HeapOperationResult::HeapLimit;
                }
                let mut start = 0;
                while start < visible.len() {
                    let end = visible[start..]
                        .find('\n')
                        .map(|at| start + at)
                        .unwrap_or(visible.len());
                    let stop = if visible[start..end].ends_with('\r') {
                        end - 1
                    } else {
                        end
                    };
                    ranges.push((start, stop));
                    start = end + 1;
                }
            }
        }
        let Some(cost) = ranges.len().checked_mul(2 * lm_heap::MIN_OBJECT_COST) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(cost, &request) {
            return result;
        }
        let mut items = Vec::new();
        if items.try_reserve_exact(ranges.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        let mut extra_roots = Vec::new();
        if extra_roots.try_reserve_exact(ranges.len()).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        for (start, end) in ranges {
            let Some(piece) = text.slice(start, end) else {
                return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
            };
            let reference = match self.allocate_heap_reference(
                crate::Object::Substring(piece),
                &request,
                &extra_roots,
            ) {
                Ok(reference) => reference,
                Err(result) => return result,
            };
            extra_roots.push(reference);
            items.push(Value::Obj(reference));
        }
        let reference = match self.allocate_heap_reference(
            crate::Object::List {
                items: items.into(),
                epoch: StructuralEpoch::default(),
            },
            &request,
            &extra_roots,
        ) {
            Ok(reference) => reference,
            Err(result) => return result,
        };
        HeapOperationResult::Value {
            bits: object_bits(reference),
            heap: Some(self.machine.vm.heap.jit_view()),
        }
    }

    fn text_slice_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
        scalar: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let start = match usize::try_from(request.second as i64) {
            Ok(start) => start,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let length = match usize::try_from(request.third as i64) {
            Ok(length) => length,
            Err(_) => return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds),
        };
        let slice = if scalar {
            text.scalar_slice(start, length)
        } else {
            start
                .checked_add(length)
                .and_then(|end| text.slice(start, end))
        };
        let Some(slice) = slice else {
            return HeapOperationResult::Fault(crate::FaultCode::IndexOutOfBounds);
        };
        self.allocate_heap_object(crate::Object::Substring(slice), &request)
    }

    fn text_bytes_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        self.allocate_heap_object(crate::Object::Bytes(text.bytes()), &request)
    }

    fn text_to_string_operation(
        &mut self,
        request: HeapOperationRequest<'_>,
    ) -> HeapOperationResult {
        let reference = object_reference(request.first);
        let text = match self.machine.vm.heap.try_get(reference) {
            Some(crate::Object::Str(_)) => return heap_bits(request.first),
            Some(crate::Object::Substring(text)) => text.clone(),
            _ => return HeapOperationResult::Fault(crate::FaultCode::TypeMismatch),
        };
        if !text.has_bounded_retention() {
            if let Err(result) = self.reserve_heap_growth(text.len(), &request) {
                return result;
            }
        }
        let text = match text.try_bounded() {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    fn bytes_text_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let Some(text) = bytes.utf8_view() else {
            return HeapOperationResult::Fault(crate::FaultCode::BadCast);
        };
        if !text.has_bounded_retention() {
            if let Err(result) = self.reserve_heap_growth(text.len(), &request) {
                return result;
            }
        }
        let text = match text.try_bounded() {
            Ok(text) => text,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(text), &request)
    }

    fn byte_buffer_find_operation(&self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
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

    fn bytes_find_operation(&self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let (bytes, needle) = match self.bytes_pair(request.first, request.second) {
            Ok(pair) => pair,
            Err(result) => return result,
        };
        let needle = needle.as_slice();
        let found = if needle.is_empty() {
            Some(0)
        } else {
            bytes
                .as_slice()
                .windows(needle.len())
                .position(|window| window == needle)
        };
        let value = match found {
            Some(index) => match i64::try_from(index) {
                Ok(index) => index,
                Err(_) => {
                    return HeapOperationResult::Fault(crate::FaultCode::IntegerOverflow);
                }
            },
            None => -1,
        };
        heap_int(value)
    }

    fn bytes_hex_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        let Some(length) = bytes.len().checked_mul(2) else {
            return HeapOperationResult::HeapLimit;
        };
        if let Err(result) = self.reserve_heap_growth(length, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(length).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in bytes.as_slice() {
            output.push(char::from(HEX[(byte >> 4) as usize]));
            output.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }

    fn bytes_is_utf8_operation(&self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let bytes = match self.bytes_value(request.first) {
            Ok(bytes) => bytes,
            Err(result) => return result,
        };
        heap_bool(bytes.is_utf8())
    }

    fn text_parse_float_operation(
        &self,
        request: HeapOperationRequest<'_>,
        status: bool,
    ) -> HeapOperationResult {
        let text = match self.text_value(request.first) {
            Ok(text) => text,
            Err(result) => return result,
        };
        let parsed = parse_float_text(text.as_str());
        if status {
            heap_int(parsed.err().unwrap_or(0))
        } else {
            heap_bits(canonical_float_bits(parsed.unwrap_or(0.0).to_bits()))
        }
    }

    fn float_fixed_operation(&mut self, request: HeapOperationRequest<'_>) -> HeapOperationResult {
        let digits = request.second as i64;
        if digits < 0 {
            return HeapOperationResult::Fault(crate::FaultCode::InvalidPrecision);
        }
        let digits = match usize::try_from(digits) {
            Ok(digits) => digits,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        let value = f64::from_bits(request.first);
        let capacity = if value.is_finite() {
            match digits.checked_add(312) {
                Some(capacity) => capacity,
                None => return HeapOperationResult::HeapLimit,
            }
        } else {
            4
        };
        if let Err(result) = self.reserve_heap_growth(capacity, &request) {
            return result;
        }
        let mut output = String::new();
        if output.try_reserve_exact(capacity).is_err() {
            return HeapOperationResult::HeapLimit;
        }
        if write!(&mut output, "{value:.digits$}").is_err() {
            return HeapOperationResult::HeapLimit;
        }
        let output = match SharedText::try_from_string(output) {
            Ok(output) => output,
            Err(_) => return HeapOperationResult::HeapLimit,
        };
        self.allocate_heap_object(crate::Object::Str(output), &request)
    }
}

fn heap_bits(bits: u64) -> HeapOperationResult {
    HeapOperationResult::Value { bits, heap: None }
}

fn heap_bool(value: bool) -> HeapOperationResult {
    heap_bits(u64::from(value))
}

fn heap_int(value: i64) -> HeapOperationResult {
    heap_bits(value as u64)
}

fn runtime_int(value: i64) -> RuntimeValueResult {
    RuntimeValueResult::Value {
        bits: value as u64,
        tag: ValueTag::Int as u64,
    }
}

fn runtime_ordering(ordering: std::cmp::Ordering) -> RuntimeValueResult {
    runtime_int(match ordering {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}
