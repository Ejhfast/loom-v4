//! Interned types for the week-1 language slice.
//!
//! The store interns primitive and function types. Type identity is a
//! dense `TypeId`, so equality checks compare one integer.

use std::collections::HashMap;
use std::fmt;

/// A dense identifier for one interned type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

/// The unit type `()`.
pub const UNIT: TypeId = TypeId(0);
/// The `Bool` type.
pub const BOOL: TypeId = TypeId(1);
/// The `Int` type.
pub const INT: TypeId = TypeId(2);
/// The `String` type.
pub const STRING: TypeId = TypeId(3);
/// The bottom type for expressions that cannot complete normally.
pub const NEVER: TypeId = TypeId(4);

/// The structure of one type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int,
    String,
    Never,
    /// A function type with parameter types and one result type.
    Fn(Vec<TypeId>, TypeId),
}

/// An interning store for types.
pub struct TypeStore {
    types: Vec<Type>,
    index: HashMap<Type, TypeId>,
}

impl Default for TypeStore {
    fn default() -> TypeStore {
        TypeStore::new()
    }
}

impl TypeStore {
    pub fn new() -> TypeStore {
        let mut store = TypeStore {
            types: Vec::new(),
            index: HashMap::new(),
        };
        // Keep this order aligned with the public constants.
        store.intern(Type::Unit);
        store.intern(Type::Bool);
        store.intern(Type::Int);
        store.intern(Type::String);
        store.intern(Type::Never);
        store
    }

    /// Intern a type and return its stable identifier.
    pub fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(id) = self.index.get(&ty) {
            return *id;
        }
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty.clone());
        self.index.insert(ty, id);
        id
    }

    /// Intern a function type.
    pub fn intern_fn(&mut self, params: Vec<TypeId>, ret: TypeId) -> TypeId {
        self.intern(Type::Fn(params, ret))
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    /// Look up a primitive type by its source name.
    pub fn by_name(&self, name: &str) -> Option<TypeId> {
        match name {
            "Bool" => Some(BOOL),
            "Int" => Some(INT),
            "String" => Some(STRING),
            _ => None,
        }
    }

    /// Return true when a value of type `found` is valid where the
    /// checker expects type `expected`. `Never` is valid everywhere.
    pub fn compatible(&self, expected: TypeId, found: TypeId) -> bool {
        found == expected || found == NEVER
    }

    /// Render one type name for diagnostics.
    pub fn display(&self, id: TypeId) -> String {
        match self.get(id) {
            Type::Unit => "()".to_string(),
            Type::Bool => "Bool".to_string(),
            Type::Int => "Int".to_string(),
            Type::String => "String".to_string(),
            Type::Never => "Never".to_string(),
            Type::Fn(params, ret) => {
                let mut out = String::from("(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&self.display(*p));
                }
                out.push_str(") -> ");
                out.push_str(&self.display(*ret));
                out
            }
        }
    }
}

impl fmt::Debug for TypeStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeStore")
            .field("count", &self.types.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_have_stable_ids() {
        let store = TypeStore::new();
        assert_eq!(*store.get(UNIT), Type::Unit);
        assert_eq!(*store.get(BOOL), Type::Bool);
        assert_eq!(*store.get(INT), Type::Int);
        assert_eq!(*store.get(STRING), Type::String);
        assert_eq!(*store.get(NEVER), Type::Never);
    }

    #[test]
    fn interning_is_idempotent() {
        let mut store = TypeStore::new();
        let a = store.intern_fn(vec![INT, BOOL], STRING);
        let b = store.intern_fn(vec![INT, BOOL], STRING);
        let c = store.intern_fn(vec![INT], STRING);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn display_is_readable() {
        let mut store = TypeStore::new();
        let f = store.intern_fn(vec![INT, STRING], BOOL);
        assert_eq!(store.display(f), "(Int, String) -> Bool");
        assert_eq!(store.display(UNIT), "()");
    }

    #[test]
    fn never_is_compatible_everywhere() {
        let store = TypeStore::new();
        assert!(store.compatible(INT, NEVER));
        assert!(store.compatible(INT, INT));
        assert!(!store.compatible(INT, BOOL));
    }
}
