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
#[repr(C)]
pub struct ObjRef {
    pub slot: u32,
    pub generation: u32,
}

/// A generation-checked reference to one machine callback slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct CallbackRef {
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
#[repr(transparent)]
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
#[repr(transparent)]
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

/// The canonical quiet NaN encoding.
pub const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

/// Normalize every NaN encoding to one canonical value.
pub fn canonical_float_bits(bits: u64) -> u64 {
    let value = f64::from_bits(bits);
    if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        bits
    }
}

/// Stable tags for the two-word runtime value ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum ValueTag {
    Unit = 0,
    Bool = 1,
    Int = 2,
    Float = 3,
    Char = 4,
    Obj = 5,
    Op = 6,
    Callback = 7,
    EmptyCase = 8,
    Uninit = 9,
}

/// Byte offset of the value tag.
pub const VALUE_TAG_OFFSET: usize = 0;
/// Byte offset of every value payload.
pub const VALUE_PAYLOAD_OFFSET: usize = 8;
/// Stable size of one runtime value.
pub const VALUE_SIZE: usize = 16;
/// Stable alignment of one runtime value.
pub const VALUE_ALIGN: usize = 8;

/// One runtime value. `Int` and `Float` keep their full 64-bit width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, u64)]
pub enum Value {
    Unit = 0,
    Bool(bool) = 1,
    Int(i64) = 2,
    /// One canonical IEEE 754 binary64 bit pattern.
    Float(u64) = 3,
    /// One Unicode scalar value.
    Char(char) = 4,
    /// A reference to a heap object: string, instance, list, map,
    /// closure, builder, or native handle.
    Obj(ObjRef) = 5,
    /// A first-class operation value: the dense manifest slot of one
    /// exact operation. The identity-indexed type lives in the static
    /// type system, not in the value.
    Op(u32) = 6,
    /// A reference to one machine-local nonescaping callback.
    Callback(CallbackRef) = 7,
    /// One nullary arm of a native one-payload enum.
    ///
    /// `ty` names the closed family type. `arm` names its source arm.
    /// Pinned `Option` uses this value for `None`.
    EmptyCase {
        ty: u32,
        arm: u32,
    } = 8,
    /// The marker for an object field without a first assignment.
    /// No instruction can produce or store this value.
    Uninit = 9,
}

const _: () = assert!(std::mem::size_of::<Value>() == VALUE_SIZE);
const _: () = assert!(std::mem::align_of::<Value>() == VALUE_ALIGN);
const _: () = assert!(std::mem::size_of::<ValueTag>() == 8);
const _: () = assert!(std::mem::offset_of!(ObjRef, slot) == 0);
const _: () = assert!(std::mem::offset_of!(ObjRef, generation) == 4);

impl Value {
    /// Return the stable runtime ABI tag.
    pub fn tag(self) -> ValueTag {
        match self {
            Value::Unit => ValueTag::Unit,
            Value::Bool(_) => ValueTag::Bool,
            Value::Int(_) => ValueTag::Int,
            Value::Float(_) => ValueTag::Float,
            Value::Char(_) => ValueTag::Char,
            Value::Obj(_) => ValueTag::Obj,
            Value::Op(_) => ValueTag::Op,
            Value::Callback(_) => ValueTag::Callback,
            Value::EmptyCase { .. } => ValueTag::EmptyCase,
            Value::Uninit => ValueTag::Uninit,
        }
    }

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
        assert_eq!(std::mem::size_of::<Value>(), VALUE_SIZE);
        assert_eq!(std::mem::align_of::<Value>(), VALUE_ALIGN);
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
    fn float_keeps_bits_and_normalizes_nan() {
        assert_eq!(Value::Float((-0.0f64).to_bits()), Value::Float(1u64 << 63));
        assert_eq!(
            canonical_float_bits(0x7ff0_0000_0000_0001),
            CANONICAL_NAN_BITS
        );
    }

    #[test]
    fn char_keeps_one_unicode_scalar() {
        assert_eq!(Value::Char('猫'), Value::Char('猫'));
    }

    #[test]
    fn an_empty_case_keeps_the_value_size() {
        let value = Value::EmptyCase { ty: 7, arm: 1 };
        assert_eq!(value, Value::EmptyCase { ty: 7, arm: 1 });
        assert_eq!(std::mem::size_of::<Value>(), 16);
    }

    #[test]
    fn every_value_uses_its_stable_tag_word() {
        let values = [
            Value::Unit,
            Value::Bool(true),
            Value::Int(-7),
            Value::Float(1.5f64.to_bits()),
            Value::Char('x'),
            Value::Obj(ObjRef {
                slot: 3,
                generation: 4,
            }),
            Value::Op(5),
            Value::Callback(CallbackRef {
                slot: 6,
                generation: 7,
            }),
            Value::EmptyCase { ty: 8, arm: 9 },
            Value::Uninit,
        ];
        for value in values {
            let address = std::ptr::from_ref(&value).cast::<u8>();
            // SAFETY: The stable tag occupies the first aligned word.
            let tag = unsafe { address.add(VALUE_TAG_OFFSET).cast::<u64>().read() };
            assert_eq!(tag, value.tag() as u64);
        }
    }

    #[test]
    fn every_value_uses_its_stable_payload_offset() {
        #[repr(C)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct Pair {
            first: u32,
            second: u32,
        }

        fn payload<T: Copy>(value: &Value) -> T {
            let address = std::ptr::from_ref(value).cast::<u8>();
            // SAFETY: Each call uses the declared payload type of this value.
            unsafe { address.add(VALUE_PAYLOAD_OFFSET).cast::<T>().read() }
        }

        assert!(payload::<bool>(&Value::Bool(true)));
        assert_eq!(payload::<i64>(&Value::Int(-7)), -7);
        assert_eq!(payload::<u64>(&Value::Float(11)), 11);
        assert_eq!(payload::<char>(&Value::Char('猫')), '猫');
        assert_eq!(
            payload::<ObjRef>(&Value::Obj(ObjRef {
                slot: 3,
                generation: 4,
            })),
            ObjRef {
                slot: 3,
                generation: 4,
            }
        );
        assert_eq!(payload::<u32>(&Value::Op(5)), 5);
        assert_eq!(
            payload::<CallbackRef>(&Value::Callback(CallbackRef {
                slot: 6,
                generation: 7,
            })),
            CallbackRef {
                slot: 6,
                generation: 7,
            }
        );
        assert_eq!(
            payload::<Pair>(&Value::EmptyCase { ty: 8, arm: 9 }),
            Pair {
                first: 8,
                second: 9,
            }
        );
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
