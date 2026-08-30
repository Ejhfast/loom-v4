//! Typed native runtime slow paths.

use crate::machine::Machine;
use crate::NamespaceRuntime;
use lm_jit::{
    AllocationResult, ListGrowthRequest, ListGrowthResult, ListReserveRequest, ListReserveResult,
    NativeRuntime, NativeTypeEnvironmentCache, ScalarKind, LOCAL_INITIALIZED,
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
        ScalarKind::Operation => Some(ValueTag::Op),
    };
    if expected.is_some_and(|tag| value.tag() != tag) {
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

fn value_bits(value: Value) -> Option<u64> {
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

fn tagged_value(tag: u64, bits: u64) -> Option<Value> {
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
    pub(super) module: &'a NamespaceRuntime,
    pub(super) base_local: usize,
    pub(super) base_operand: usize,
    pub(super) allocations: u64,
}

impl Drop for MachineRuntime<'_> {
    fn drop(&mut self) {
        self.machine.native_type_environments = std::mem::take(&mut self.type_environments);
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
        if root_bits.len() != root_tags.len() || root_bits.len() != root_states.len() {
            return AllocationResult::Interpreter;
        }
        let Some(class_entry) = self.module.classes.get(class as usize) else {
            return AllocationResult::Interpreter;
        };
        let object = crate::Object::Instance {
            class,
            fields: vec![Value::Uninit; class_entry.fields.len()].into(),
            env: lm_value::Witness(lm_value::TypeEnvId(environment)),
        };
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
                let heap = Some(self.machine.vm.heap.jit_view());
                AllocationResult::Value {
                    bits: object_bits(reference),
                    heap,
                }
            }
            Ok(_) => AllocationResult::Interpreter,
            Err(crate::FaultCode::HeapLimit) => AllocationResult::HeapLimit,
            Err(_) => AllocationResult::Interpreter,
        }
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
