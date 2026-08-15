//! Interned types for the week-2 language slice.
//!
//! The store interns primitive, class, collection, and function types.
//! Type identity is a dense `TypeId`, so equality checks compare one
//! integer. The store also records the class table (name and parent),
//! so subtype queries and joins need no other context.

use std::collections::HashMap;
use std::fmt;

/// A dense identifier for one interned type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TypeId(pub u32);

/// A dense identifier for one declared class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClassId(pub u32);

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
/// The `StringBuilder` native type.
pub const STRING_BUILDER: TypeId = TypeId(5);
/// The `ByteBuffer` native type.
pub const BYTE_BUFFER: TypeId = TypeId(6);

/// The structure of one type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Unit,
    Bool,
    Int,
    String,
    Never,
    StringBuilder,
    ByteBuffer,
    /// A class instance type.
    Class(ClassId),
    /// A list type with one invariant element type.
    List(TypeId),
    /// A map type with invariant key and value types.
    Map(TypeId, TypeId),
    /// A function type with parameter types and one result type.
    Fn(Vec<TypeId>, TypeId),
}

/// The registered name and parent of one class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassMeta {
    pub name: String,
    pub parent: Option<ClassId>,
}

/// An interning store for types plus the class table.
pub struct TypeStore {
    types: Vec<Type>,
    index: HashMap<Type, TypeId>,
    classes: Vec<ClassMeta>,
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
            classes: Vec::new(),
        };
        // Keep this order aligned with the public constants.
        store.intern(Type::Unit);
        store.intern(Type::Bool);
        store.intern(Type::Int);
        store.intern(Type::String);
        store.intern(Type::Never);
        store.intern(Type::StringBuilder);
        store.intern(Type::ByteBuffer);
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

    /// Register a class. The parent can be set later, before any
    /// subtype query that involves the class.
    pub fn register_class(&mut self, name: impl Into<String>) -> ClassId {
        let id = ClassId(self.classes.len() as u32);
        self.classes.push(ClassMeta {
            name: name.into(),
            parent: None,
        });
        id
    }

    pub fn set_class_parent(&mut self, class: ClassId, parent: ClassId) {
        self.classes[class.0 as usize].parent = Some(parent);
    }

    pub fn class_meta(&self, class: ClassId) -> &ClassMeta {
        &self.classes[class.0 as usize]
    }

    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// Look up a primitive or native type by its source name.
    pub fn by_name(&self, name: &str) -> Option<TypeId> {
        match name {
            "Bool" => Some(BOOL),
            "Int" => Some(INT),
            "String" => Some(STRING),
            "StringBuilder" => Some(STRING_BUILDER),
            "ByteBuffer" => Some(BYTE_BUFFER),
            _ => None,
        }
    }

    /// Return true when class `child` equals `ancestor` or inherits it.
    pub fn class_extends(&self, child: ClassId, ancestor: ClassId) -> bool {
        let mut cur = Some(child);
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            cur = self.classes[c.0 as usize].parent;
        }
        false
    }

    /// Return true when a value of type `found` is valid where the
    /// checker expects type `expected`. `Never` is valid everywhere.
    /// A subclass instance is valid at a superclass type. Generic
    /// applications are invariant.
    pub fn compatible(&self, expected: TypeId, found: TypeId) -> bool {
        if found == expected || found == NEVER {
            return true;
        }
        match (self.get(found), self.get(expected)) {
            (Type::Class(a), Type::Class(b)) => self.class_extends(*a, *b),
            _ => false,
        }
    }

    /// Join two branch types. Classes join at their nearest common
    /// ancestor. `None` marks unrelated types.
    pub fn join(&self, a: TypeId, b: TypeId) -> Option<TypeId> {
        if self.compatible(b, a) {
            return Some(b);
        }
        if self.compatible(a, b) {
            return Some(a);
        }
        if let (Type::Class(ca), Type::Class(cb)) = (self.get(a), self.get(b)) {
            let (ca, cb) = (*ca, *cb);
            let mut anc = Some(ca);
            while let Some(c) = anc {
                if self.class_extends(cb, c) {
                    return self.index.get(&Type::Class(c)).copied();
                }
                anc = self.classes[c.0 as usize].parent;
            }
        }
        None
    }

    /// Return true when values of the type live in the heap.
    pub fn is_heap(&self, id: TypeId) -> bool {
        matches!(
            self.get(id),
            Type::String
                | Type::StringBuilder
                | Type::ByteBuffer
                | Type::Class(_)
                | Type::List(_)
                | Type::Map(_, _)
                | Type::Fn(_, _)
        )
    }

    /// Render one type name for diagnostics.
    pub fn display(&self, id: TypeId) -> String {
        match self.get(id) {
            Type::Unit => "()".to_string(),
            Type::Bool => "Bool".to_string(),
            Type::Int => "Int".to_string(),
            Type::String => "String".to_string(),
            Type::Never => "Never".to_string(),
            Type::StringBuilder => "StringBuilder".to_string(),
            Type::ByteBuffer => "ByteBuffer".to_string(),
            Type::Class(c) => self.classes[c.0 as usize].name.clone(),
            Type::List(e) => format!("[{}]", self.display(*e)),
            Type::Map(k, v) => format!("{{{}: {}}}", self.display(*k), self.display(*v)),
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
            .field("classes", &self.classes.len())
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
        assert_eq!(*store.get(STRING_BUILDER), Type::StringBuilder);
        assert_eq!(*store.get(BYTE_BUFFER), Type::ByteBuffer);
    }

    #[test]
    fn interning_is_idempotent() {
        let mut store = TypeStore::new();
        let a = store.intern_fn(vec![INT, BOOL], STRING);
        let b = store.intern_fn(vec![INT, BOOL], STRING);
        let c = store.intern_fn(vec![INT], STRING);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let l1 = store.intern(Type::List(INT));
        let l2 = store.intern(Type::List(INT));
        assert_eq!(l1, l2);
    }

    #[test]
    fn display_is_readable() {
        let mut store = TypeStore::new();
        let f = store.intern_fn(vec![INT, STRING], BOOL);
        assert_eq!(store.display(f), "(Int, String) -> Bool");
        assert_eq!(store.display(UNIT), "()");
        let l = store.intern(Type::List(INT));
        assert_eq!(store.display(l), "[Int]");
        let m = store.intern(Type::Map(STRING, INT));
        assert_eq!(store.display(m), "{String: Int}");
    }

    #[test]
    fn never_is_compatible_everywhere() {
        let store = TypeStore::new();
        assert!(store.compatible(INT, NEVER));
        assert!(store.compatible(INT, INT));
        assert!(!store.compatible(INT, BOOL));
    }

    #[test]
    fn subclass_is_compatible_with_ancestor() {
        let mut store = TypeStore::new();
        let animal = store.register_class("Animal");
        let dog = store.register_class("Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        assert!(store.compatible(t_animal, t_dog));
        assert!(!store.compatible(t_dog, t_animal));
    }

    #[test]
    fn generic_applications_are_invariant() {
        let mut store = TypeStore::new();
        let animal = store.register_class("Animal");
        let dog = store.register_class("Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        let l_animal = store.intern(Type::List(t_animal));
        let l_dog = store.intern(Type::List(t_dog));
        assert!(!store.compatible(l_animal, l_dog));
        assert!(!store.compatible(l_dog, l_animal));
    }

    #[test]
    fn join_finds_the_common_ancestor() {
        let mut store = TypeStore::new();
        let animal = store.register_class("Animal");
        let dog = store.register_class("Dog");
        let cat = store.register_class("Cat");
        store.set_class_parent(dog, animal);
        store.set_class_parent(cat, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        let t_cat = store.intern(Type::Class(cat));
        assert_eq!(store.join(t_dog, t_cat), Some(t_animal));
        assert_eq!(store.join(t_dog, t_animal), Some(t_animal));
        assert_eq!(store.join(t_dog, t_dog), Some(t_dog));
        assert_eq!(store.join(INT, BOOL), None);
        assert_eq!(store.join(INT, NEVER), Some(INT));
    }
}
