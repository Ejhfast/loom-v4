//! The runtime value representation.
//!
//! `Value` is a 16-byte copyable tagged union. Heap data lives in the
//! VM object table and values hold only a generation-checked reference.

/// A generation-checked reference to one object-table slot.
///
/// The `slot` names an entry in the per-VM object table. The
/// `generation` must match the entry generation. A mismatch marks a
/// stale reference to a collected slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjRef {
    pub slot: u32,
    pub generation: u32,
}

/// One runtime value. `Int` keeps its full 64-bit width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    /// A reference to a heap object: string, instance, list, map,
    /// closure, builder, or native handle.
    Obj(ObjRef),
    /// A first-class operation value: the dense manifest slot of one
    /// exact operation. The identity-indexed type lives in the static
    /// type system, not in the value.
    Op(u32),
    /// The marker for an object field without a first assignment.
    /// No instruction can produce or store this value.
    Uninit,
}

impl Value {
    /// Return the object reference when the value holds one.
    pub fn as_obj(self) -> Option<ObjRef> {
        match self {
            Value::Obj(r) => Some(r),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_16_bytes() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
    }

    #[test]
    fn value_is_copy() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Value>();
    }

    #[test]
    fn int_keeps_full_width() {
        let v = Value::Int(i64::MIN);
        assert_eq!(v, Value::Int(i64::MIN));
    }

    #[test]
    fn as_obj_extracts_references() {
        let r = ObjRef {
            slot: 3,
            generation: 7,
        };
        assert_eq!(Value::Obj(r).as_obj(), Some(r));
        assert_eq!(Value::Int(1).as_obj(), None);
    }
}
