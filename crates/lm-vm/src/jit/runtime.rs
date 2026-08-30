//! Temporary native runtime slow paths.

use crate::machine::Machine;
use crate::NamespaceRuntime;
use lm_jit::{Runtime, RuntimeResult, ScalarKind, ValueRepr, LOCAL_INITIALIZED};
use lm_value::{canonical_float_bits, ObjRef, Value};

pub(super) fn scalar_bits(kind: ScalarKind, value: Value) -> Option<u64> {
    match (kind, value) {
        (ScalarKind::Unit, Value::Unit) => Some(0),
        (ScalarKind::Bool, Value::Bool(value)) => Some(u64::from(value)),
        (ScalarKind::Int, Value::Int(value)) => Some(value as u64),
        (ScalarKind::Float, Value::Float(bits)) if canonical_float_bits(bits) == bits => Some(bits),
        (ScalarKind::Object(_), Value::Obj(reference)) => Some(object_bits(reference)),
        (ScalarKind::Operation, Value::Op(operation)) => Some(u64::from(operation)),
        _ => None,
    }
}

pub(super) fn bits_value(kind: ScalarKind, bits: u64) -> Value {
    match kind {
        ScalarKind::Unit => Value::Unit,
        ScalarKind::Bool => Value::Bool(bits != 0),
        ScalarKind::Int => Value::Int(bits as i64),
        ScalarKind::Float => Value::Float(canonical_float_bits(bits)),
        ScalarKind::Object(_) => Value::Obj(object_reference(bits)),
        ScalarKind::Operation => Value::Op(bits as u32),
    }
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

pub(super) struct MachineRuntime<'a> {
    pub(super) machine: &'a mut Machine,
    pub(super) module: &'a NamespaceRuntime,
    pub(super) base_local: usize,
    pub(super) base_operand: usize,
    pub(super) heap_reads: u64,
    pub(super) allocations: u64,
}

impl Runtime for MachineRuntime<'_> {
    fn load_field(&mut self, reference: ObjRef, field: u32, expected: ValueRepr) -> RuntimeResult {
        self.heap_reads = self.heap_reads.saturating_add(1);
        let Some(crate::Object::Instance { fields, .. }) = self.machine.vm.heap.try_get(reference)
        else {
            return RuntimeResult::TypeMismatch;
        };
        let Some(value) = fields.get(field as usize).copied() else {
            return RuntimeResult::TypeMismatch;
        };
        if value == Value::Uninit {
            return RuntimeResult::UninitializedField;
        }
        match representation_bits(expected, value) {
            Some(bits) => RuntimeResult::Value(bits),
            None => RuntimeResult::Interpreter,
        }
    }

    fn allocate_instance(
        &mut self,
        class: u32,
        root_bits: &[u64],
        root_states: &[u8],
        allow_collection: bool,
    ) -> RuntimeResult {
        if root_bits.len() != root_states.len() {
            return RuntimeResult::Interpreter;
        }
        let Some(class_entry) = self.module.classes.get(class as usize) else {
            return RuntimeResult::Interpreter;
        };
        let object = crate::Object::Instance {
            class,
            fields: vec![Value::Uninit; class_entry.fields.len()],
            env: lm_value::Witness::EMPTY,
        };
        let cost = self.machine.vm.heap.allocation_cost(&object);
        if !self.machine.vm.heap.collection_due(cost) {
            let reference = self.machine.vm.heap.alloc(object);
            self.allocations = self.allocations.saturating_add(1);
            return RuntimeResult::Value(object_bits(reference));
        }
        if !allow_collection {
            return RuntimeResult::Interpreter;
        }
        let mut roots = Vec::new();
        if roots.try_reserve_exact(root_bits.len()).is_err() {
            return RuntimeResult::HeapLimit;
        }
        roots.extend(
            root_bits
                .iter()
                .copied()
                .zip(root_states.iter().copied())
                .filter(|(_, state)| state & LOCAL_INITIALIZED != 0)
                .map(|(bits, _)| object_reference(bits)),
        );
        match self
            .machine
            .alloc_native(object, self.base_local, self.base_operand, &roots)
        {
            Ok(Value::Obj(reference)) => {
                self.allocations = self.allocations.saturating_add(1);
                RuntimeResult::Value(object_bits(reference))
            }
            Ok(_) => RuntimeResult::Interpreter,
            Err(crate::FaultCode::HeapLimit) => RuntimeResult::HeapLimit,
            Err(_) => RuntimeResult::Interpreter,
        }
    }
}

fn representation_bits(expected: ValueRepr, value: Value) -> Option<u64> {
    match (expected, value) {
        (ValueRepr::Unit, Value::Unit) => Some(0),
        (ValueRepr::Bool, Value::Bool(value)) => Some(u64::from(value)),
        (ValueRepr::Int, Value::Int(value)) => Some(value as u64),
        (ValueRepr::Float, Value::Float(bits)) if canonical_float_bits(bits) == bits => Some(bits),
        (ValueRepr::Object, Value::Obj(reference)) => Some(object_bits(reference)),
        (ValueRepr::Operation, Value::Op(operation)) => Some(u64::from(operation)),
        _ => None,
    }
}
