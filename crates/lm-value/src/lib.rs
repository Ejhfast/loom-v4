//! The runtime value representation.
//!
//! `Value` is a 16-byte copyable tagged union. Strings live in the VM
//! heap and values hold only a stable string reference.

/// A reference to one immutable string in the VM heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrRef(pub u32);

/// A reference to one code slot (a loaded function).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodeSlot(pub u32);

/// One runtime value. `Int` keeps its full 64-bit width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Unit,
    Bool(bool),
    Int(i64),
    Str(StrRef),
    Code(CodeSlot),
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
}
