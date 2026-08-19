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

/// One type environment of one world.
///
/// The verifier proves a generic body once, with the type variables of
/// that body opaque. One activation of the body needs the type
/// arguments its call site applied. A frame, a closure, an instance,
/// and a machine each store one of these indices, and the world holds
/// the table the index names.
///
/// Index zero names the empty environment. A monomorphic state stores
/// zero, allocates nothing, and performs no type work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeEnvId(pub u32);

impl TypeEnvId {
    /// The empty environment. It holds no type argument and no effect
    /// argument.
    pub const EMPTY: TypeEnvId = TypeEnvId(0);

    /// True when this index names the empty environment.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl Default for TypeEnvId {
    fn default() -> TypeEnvId {
        TypeEnvId::EMPTY
    }
}

/// The type environment one heap object was created under.
///
/// A witness is provenance. It never enters a guest digest, semantic
/// equality, or the semantic identity of a value, so two values with
/// equal structure stay equal when their witnesses differ. The
/// equality below states that rule for every holder of a witness.
#[derive(Debug, Clone, Copy, Default, Eq)]
pub struct Witness(pub TypeEnvId);

impl Witness {
    /// The witness of a state that needs no type argument.
    pub const EMPTY: Witness = Witness(TypeEnvId::EMPTY);

    /// The environment this witness names.
    pub fn env(self) -> TypeEnvId {
        self.0
    }
}

impl PartialEq for Witness {
    /// A witness is provenance, so it never decides value equality.
    fn eq(&self, _: &Witness) -> bool {
        true
    }
}

/// One runtime value. `Int` keeps its full 64-bit width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    /// One Unicode scalar value.
    Char(char),
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
    fn char_keeps_one_unicode_scalar() {
        assert_eq!(Value::Char('猫'), Value::Char('猫'));
    }

    #[test]
    fn the_empty_environment_is_index_zero() {
        assert_eq!(TypeEnvId::default(), TypeEnvId::EMPTY);
        assert!(TypeEnvId::EMPTY.is_empty());
        assert!(!TypeEnvId(1).is_empty());
    }

    /// A witness is provenance, so it never decides equality.
    #[test]
    fn two_witnesses_are_always_equal() {
        assert_eq!(Witness(TypeEnvId(1)), Witness(TypeEnvId(2)));
        assert_eq!(Witness::EMPTY.env(), TypeEnvId::EMPTY);
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
