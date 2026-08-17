//! Interned types for the week-3 language slice.
//!
//! The store interns primitive, class, generic-application, tuple,
//! collection, and function types. Type identity is a dense `TypeId`,
//! so equality checks compare one integer. The store also records the
//! class table (name, parent, generic arity, kind), a precomputed
//! nominal ancestor table, and interned effect-row names. Subtype
//! queries are memoized structural recursion with no backtracking.

use std::cell::RefCell;
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
/// The frozen machine `Fault` value type.
pub const FAULT: TypeId = TypeId(7);
/// The opaque pending-request token type.
pub const REQUEST: TypeId = TypeId(8);
/// The holder-local policy-table handle type.
pub const POLICY_TABLE: TypeId = TypeId(9);
/// The unloaded virtual machine handle type.
pub const EMPTY_VM: TypeId = TypeId(10);
/// The frozen canonical graph digest type.
pub const DIGEST: TypeId = TypeId(11);
/// One verified snapshot image whose result type is not yet checked.
pub const SNAPSHOT_IMAGE: TypeId = TypeId(12);
/// Immutable binary data.
pub const BYTES: TypeId = TypeId(13);
/// A typed file resource designator.
pub const FILE_HANDLE: TypeId = TypeId(14);
/// A holder-local resource-management designator.
pub const RESOURCE_HANDLE: TypeId = TypeId(15);

/// One element of an effect row.
///
/// `Op` names an exact operation or a group by an interned row-name
/// index. `Var` names one effect parameter of the enclosing callable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RowElem {
    Op(u32),
    Var(u32),
}

/// A canonical effect row: sorted, without duplicates.
pub type Row = Vec<RowElem>;

/// The declaration kind of one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// An ordinary class.
    Normal,
    /// The abstract closed parent of one enum family.
    EnumParent,
    /// One final case class of an enum family.
    EnumCase,
}

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
    /// Immutable binary data.
    Bytes,
    /// The frozen canonical graph digest of one value.
    Digest,
    /// An instance type of a class without generic parameters.
    Class(ClassId),
    /// An instance type of a generic class applied to arguments.
    /// Applications are invariant.
    Inst(ClassId, Vec<TypeId>),
    /// A list type with one invariant element type.
    List(TypeId),
    /// A map type with invariant key and value types.
    Map(TypeId, TypeId),
    /// A fixed-arity structural tuple with covariant elements.
    Tuple(Vec<TypeId>),
    /// A function type with parameter types, parameter `mut` markers,
    /// a result, and a row. The marker vector length equals the
    /// parameter vector length.
    Fn(Vec<TypeId>, Vec<bool>, TypeId, Row),
    /// One type parameter of the enclosing generic definition.
    Var(u32),
    /// The frozen machine `Fault` value type.
    Fault,
    /// The opaque pending-request token type.
    Request,
    /// The holder-local policy-table handle type.
    PolicyTable,
    /// The unloaded virtual machine handle type.
    EmptyVm,
    /// A loaded virtual machine typed by its terminal result.
    Vm(TypeId),
    /// A typed pending-call token: the argument view type and the
    /// reply type.
    PendingCall(TypeId, TypeId),
    /// A proc handle: the mailbox message type and the terminal
    /// result type of the proc.
    Handle(TypeId, TypeId),
    /// One verified snapshot image with no checked result type. A
    /// receiverless self snapshot and the loader both produce it,
    /// because neither can name the enclosing machine result type
    /// (specification 17.1).
    SnapshotImage,
    /// One snapshot of a machine world, typed by the terminal result
    /// type of its root machine.
    Snapshot(TypeId),
    /// A file resource designator.
    FileHandle,
    /// A holder-local resource-management designator.
    ResourceHandle,
    /// An identity-indexed first-class operation value: the manifest
    /// operation slot and the callable function type.
    Op(u32, TypeId),
}

/// The registered metadata of one class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassMeta {
    pub name: String,
    pub parent: Option<ClassId>,
    /// The type arguments of a generic parent, for example the `Work`
    /// of `class Worker < Proc[Work]`. Empty for a plain parent.
    ///
    /// Only a class without type parameters may declare a parent, so
    /// these arguments never hold a type variable. Every ancestor walk
    /// therefore reads them without substitution.
    pub parent_args: Vec<TypeId>,
    /// The number of generic type parameters.
    pub type_params: u32,
    pub kind: ClassKind,
}

/// An interning store for types plus the class table.
pub struct TypeStore {
    types: Vec<Type>,
    index: HashMap<Type, TypeId>,
    classes: Vec<ClassMeta>,
    row_names: Vec<String>,
    row_name_index: HashMap<String, u32>,
    /// Memoized subtype answers keyed by `(expected, found)`.
    subtype_cache: RefCell<HashMap<(TypeId, TypeId), bool>>,
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
            row_names: Vec::new(),
            row_name_index: HashMap::new(),
            subtype_cache: RefCell::new(HashMap::new()),
        };
        // Keep this order aligned with the public constants.
        store.intern(Type::Unit);
        store.intern(Type::Bool);
        store.intern(Type::Int);
        store.intern(Type::String);
        store.intern(Type::Never);
        store.intern(Type::StringBuilder);
        store.intern(Type::ByteBuffer);
        store.intern(Type::Fault);
        store.intern(Type::Request);
        store.intern(Type::PolicyTable);
        store.intern(Type::EmptyVm);
        store.intern(Type::Digest);
        store.intern(Type::SnapshotImage);
        store.intern(Type::Bytes);
        store.intern(Type::FileHandle);
        store.intern(Type::ResourceHandle);
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

    /// Intern a function type. `muts` marks the `mut` parameters and
    /// must have one entry per parameter.
    pub fn intern_fn(
        &mut self,
        params: Vec<TypeId>,
        muts: Vec<bool>,
        ret: TypeId,
        row: Row,
    ) -> TypeId {
        debug_assert_eq!(params.len(), muts.len());
        self.intern(Type::Fn(params, muts, ret, row))
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    /// Look up an interned type without interning a new one.
    pub fn find(&self, ty: &Type) -> Option<TypeId> {
        self.index.get(ty).copied()
    }

    /// Intern an effect row name, for example `Io.Print` or `Io`.
    pub fn intern_row_name(&mut self, name: &str) -> u32 {
        if let Some(idx) = self.row_name_index.get(name) {
            return *idx;
        }
        let idx = self.row_names.len() as u32;
        self.row_names.push(name.to_string());
        self.row_name_index.insert(name.to_string(), idx);
        idx
    }

    pub fn row_name(&self, idx: u32) -> &str {
        &self.row_names[idx as usize]
    }

    /// Sort a row into canonical order and remove duplicates.
    /// Operations sort by name text; variables sort by index.
    pub fn canonical_row(&self, mut row: Row) -> Row {
        row.sort_by_key(|elem| self.row_elem_key(*elem));
        row.dedup();
        row
    }

    fn row_elem_key(&self, elem: RowElem) -> (u8, String, u32) {
        match elem {
            RowElem::Op(n) => (0, self.row_names[n as usize].clone(), 0),
            RowElem::Var(v) => (1, String::new(), v),
        }
    }

    /// Return true when every element of `sub` is included in `sup`.
    /// An exact operation `G.Op` is included when `sup` holds it or
    /// holds its group `G`. A variable is included only when `sup`
    /// holds the same variable.
    pub fn row_included(&self, sub: &[RowElem], sup: &[RowElem]) -> bool {
        sub.iter().all(|elem| self.row_elem_included(*elem, sup))
    }

    fn row_elem_included(&self, elem: RowElem, sup: &[RowElem]) -> bool {
        match elem {
            RowElem::Var(v) => sup.contains(&RowElem::Var(v)),
            RowElem::Op(n) => {
                let name = &self.row_names[n as usize];
                sup.iter().any(|s| match s {
                    RowElem::Op(m) => {
                        let sup_name = &self.row_names[*m as usize];
                        sup_name == name
                            || name
                                .split_once('.')
                                .map(|(group, _)| group == sup_name)
                                .unwrap_or(false)
                    }
                    RowElem::Var(_) => false,
                })
            }
        }
    }

    /// Register a class. The parent can be set later, before any
    /// subtype query that involves the class.
    pub fn register_class(
        &mut self,
        name: impl Into<String>,
        type_params: u32,
        kind: ClassKind,
    ) -> ClassId {
        let id = ClassId(self.classes.len() as u32);
        self.classes.push(ClassMeta {
            name: name.into(),
            parent: None,
            parent_args: Vec::new(),
            type_params,
            kind,
        });
        id
    }

    pub fn set_class_parent(&mut self, class: ClassId, parent: ClassId) {
        self.set_class_parent_args(class, parent, Vec::new());
    }

    /// Record the parent of one class together with the type arguments
    /// of a generic parent.
    pub fn set_class_parent_args(&mut self, class: ClassId, parent: ClassId, args: Vec<TypeId>) {
        let meta = &mut self.classes[class.0 as usize];
        meta.parent = Some(parent);
        meta.parent_args = args;
        self.subtype_cache.borrow_mut().clear();
    }

    /// The type arguments of `ancestor` seen from an instance of
    /// `child` applied to `args`. `None` when `child` does not inherit
    /// `ancestor`.
    ///
    /// Three parent shapes exist. An enum case shares the arity of its
    /// family and passes its arguments through. A declared generic
    /// parent records closed arguments, because a generic class never
    /// declares a parent. Every other parent has no arguments. The
    /// walk therefore never substitutes.
    pub fn ancestor_args(
        &self,
        child: ClassId,
        args: &[TypeId],
        ancestor: ClassId,
    ) -> Option<Vec<TypeId>> {
        let mut cur = child;
        let mut cur_args = args.to_vec();
        loop {
            if cur == ancestor {
                return Some(cur_args);
            }
            let meta = &self.classes[cur.0 as usize];
            let parent = meta.parent?;
            if !meta.parent_args.is_empty() {
                cur_args = meta.parent_args.clone();
            } else if self.classes[parent.0 as usize].type_params == 0 {
                cur_args = Vec::new();
            }
            cur = parent;
        }
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
            "Never" => Some(NEVER),
            "Int" => Some(INT),
            "String" => Some(STRING),
            "StringBuilder" => Some(STRING_BUILDER),
            "ByteBuffer" => Some(BYTE_BUFFER),
            "Fault" => Some(FAULT),
            "Request" => Some(REQUEST),
            "PolicyTable" => Some(POLICY_TABLE),
            "EmptyVm" => Some(EMPTY_VM),
            "Digest" => Some(DIGEST),
            "SnapshotImage" => Some(SNAPSHOT_IMAGE),
            "Bytes" => Some(BYTES),
            "FileHandle" => Some(FILE_HANDLE),
            "ResourceHandle" => Some(RESOURCE_HANDLE),
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
    /// applications are invariant. Tuples are covariant element-wise.
    /// Functions are contravariant in parameters, covariant in the
    /// result, and covariant in the row by inclusion.
    pub fn compatible(&self, expected: TypeId, found: TypeId) -> bool {
        if found == expected || found == NEVER {
            return true;
        }
        let key = (expected, found);
        if let Some(hit) = self.subtype_cache.borrow().get(&key) {
            return *hit;
        }
        let answer = match (self.get(found), self.get(expected)) {
            (Type::Class(a), Type::Class(b)) => self.class_extends(*a, *b),
            // A class may inherit a generic parent, so a plain class
            // instance can satisfy an application type.
            (Type::Class(a), Type::Inst(b, ys)) => {
                self.ancestor_args(*a, &[], *b).as_ref() == Some(ys)
            }
            (Type::Inst(a, xs), Type::Class(b)) => {
                self.ancestor_args(*a, xs, *b) == Some(Vec::new())
            }
            (Type::Inst(a, xs), Type::Inst(b, ys)) => {
                self.ancestor_args(*a, xs, *b).as_ref() == Some(ys)
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(x, y)| self.compatible(*y, *x))
            }
            (Type::Fn(fp, fm, fr, frow), Type::Fn(ep, em, er, erow)) => {
                // A function that needs a `mut` argument is not valid
                // where the expected type promises a read-only call.
                fp.len() == ep.len()
                    && fp
                        .iter()
                        .zip(ep.iter())
                        .all(|(f, e)| self.compatible(*f, *e))
                    && fm.iter().zip(em.iter()).all(|(f, e)| !*f || *e)
                    && self.compatible(*er, *fr)
                    && self.row_included(frow, erow)
            }
            _ => false,
        };
        self.subtype_cache.borrow_mut().insert(key, answer);
        answer
    }

    /// Join two branch types. Classes and generic applications join at
    /// their nearest common ancestor. Tuples join element-wise.
    /// `None` marks unrelated types.
    pub fn join(&mut self, a: TypeId, b: TypeId) -> Option<TypeId> {
        if self.compatible(b, a) {
            return Some(b);
        }
        if self.compatible(a, b) {
            return Some(a);
        }
        match (self.get(a).clone(), self.get(b).clone()) {
            (Type::Class(ca), Type::Class(cb)) => {
                let common = self.common_ancestor(ca, cb)?;
                // The common ancestor may be a generic class, because
                // a class may inherit an instantiated generic parent.
                // Both branches must reach it with equal arguments.
                let left = self.ancestor_args(ca, &[], common)?;
                let right = self.ancestor_args(cb, &[], common)?;
                if left != right {
                    return None;
                }
                if left.is_empty() {
                    Some(self.intern(Type::Class(common)))
                } else {
                    Some(self.intern(Type::Inst(common, left)))
                }
            }
            (Type::Inst(ca, xs), Type::Inst(cb, ys)) => {
                if xs != ys {
                    return None;
                }
                let common = self.common_ancestor(ca, cb)?;
                Some(self.intern(Type::Inst(common, xs)))
            }
            (Type::Tuple(xs), Type::Tuple(ys)) => {
                if xs.len() != ys.len() {
                    return None;
                }
                let mut elems = Vec::with_capacity(xs.len());
                for (x, y) in xs.iter().zip(ys.iter()) {
                    elems.push(self.join(*x, *y)?);
                }
                Some(self.intern(Type::Tuple(elems)))
            }
            _ => None,
        }
    }

    fn common_ancestor(&self, a: ClassId, b: ClassId) -> Option<ClassId> {
        let mut anc = Some(a);
        while let Some(c) = anc {
            if self.class_extends(b, c) {
                return Some(c);
            }
            anc = self.classes[c.0 as usize].parent;
        }
        None
    }

    /// Return true when values of the type live in the heap. An `Op`
    /// value is an immediate and does not live in the heap.
    pub fn is_heap(&self, id: TypeId) -> bool {
        matches!(
            self.get(id),
            Type::String
                | Type::StringBuilder
                | Type::ByteBuffer
                | Type::Bytes
                | Type::Digest
                | Type::Class(_)
                | Type::Inst(_, _)
                | Type::List(_)
                | Type::Map(_, _)
                | Type::Tuple(_)
                | Type::Fn(_, _, _, _)
                | Type::Fault
                | Type::Request
                | Type::PolicyTable
                | Type::EmptyVm
                | Type::Vm(_)
                | Type::PendingCall(_, _)
                | Type::Handle(_, _)
                | Type::SnapshotImage
                | Type::Snapshot(_)
                | Type::FileHandle
                | Type::ResourceHandle
        )
    }

    /// True when the type names a holder-local native class.
    ///
    /// A holder-local native value stays in the heap that holds it
    /// (specification 16.4), so it never crosses a machine boundary.
    /// This is the leaf test. A caller that owes a whole message type
    /// walks the composite parts and the declared fields itself.
    ///
    /// `Handle` is not holder local: a handle crosses as a typed
    /// designator (18.5).
    pub fn is_holder_local_native(&self, id: TypeId) -> bool {
        matches!(
            self.get(id),
            Type::StringBuilder
                | Type::ByteBuffer
                | Type::Request
                | Type::PolicyTable
                | Type::EmptyVm
                | Type::Vm(_)
                | Type::PendingCall(_, _)
                | Type::ResourceHandle
        )
    }

    /// Substitute type variables and effect variables in `ty`.
    /// `targs[i]` replaces `Var(i)`. `rowargs[i]` replaces the row
    /// variable `i`. Missing entries keep the variable.
    pub fn substitute(&mut self, ty: TypeId, targs: &[TypeId], rowargs: &[Row]) -> TypeId {
        if targs.is_empty() && rowargs.is_empty() {
            return ty;
        }
        match self.get(ty).clone() {
            Type::Var(i) => targs.get(i as usize).copied().unwrap_or(ty),
            Type::Inst(c, args) => {
                let args: Vec<TypeId> = args
                    .iter()
                    .map(|a| self.substitute(*a, targs, rowargs))
                    .collect();
                self.intern(Type::Inst(c, args))
            }
            Type::List(e) => {
                let e = self.substitute(e, targs, rowargs);
                self.intern(Type::List(e))
            }
            Type::Map(k, v) => {
                let k = self.substitute(k, targs, rowargs);
                let v = self.substitute(v, targs, rowargs);
                self.intern(Type::Map(k, v))
            }
            Type::Tuple(elems) => {
                let elems: Vec<TypeId> = elems
                    .iter()
                    .map(|e| self.substitute(*e, targs, rowargs))
                    .collect();
                self.intern(Type::Tuple(elems))
            }
            Type::Fn(params, muts, ret, row) => {
                let params: Vec<TypeId> = params
                    .iter()
                    .map(|p| self.substitute(*p, targs, rowargs))
                    .collect();
                let ret = self.substitute(ret, targs, rowargs);
                let row = self.substitute_row(&row, rowargs);
                self.intern(Type::Fn(params, muts, ret, row))
            }
            Type::Vm(t) => {
                let t = self.substitute(t, targs, rowargs);
                self.intern(Type::Vm(t))
            }
            Type::Snapshot(t) => {
                let t = self.substitute(t, targs, rowargs);
                self.intern(Type::Snapshot(t))
            }
            Type::PendingCall(a, r) => {
                let a = self.substitute(a, targs, rowargs);
                let r = self.substitute(r, targs, rowargs);
                self.intern(Type::PendingCall(a, r))
            }
            Type::Handle(m, r) => {
                let m = self.substitute(m, targs, rowargs);
                let r = self.substitute(r, targs, rowargs);
                self.intern(Type::Handle(m, r))
            }
            _ => ty,
        }
    }

    /// Substitute effect variables in one row and re-canonicalize.
    pub fn substitute_row(&self, row: &[RowElem], rowargs: &[Row]) -> Row {
        let mut out = Vec::new();
        for elem in row {
            match elem {
                RowElem::Var(i) => match rowargs.get(*i as usize) {
                    Some(replacement) => out.extend_from_slice(replacement),
                    None => out.push(*elem),
                },
                RowElem::Op(_) => out.push(*elem),
            }
        }
        self.canonical_row(out)
    }

    /// Return true when the type contains a type variable.
    pub fn contains_var(&self, ty: TypeId) -> bool {
        match self.get(ty) {
            Type::Var(_) => true,
            Type::Inst(_, args) => args.iter().any(|a| self.contains_var(*a)),
            Type::List(e) => self.contains_var(*e),
            Type::Map(k, v) => self.contains_var(*k) || self.contains_var(*v),
            Type::Tuple(elems) => elems.iter().any(|e| self.contains_var(*e)),
            Type::Fn(params, _, ret, _) => {
                params.iter().any(|p| self.contains_var(*p)) || self.contains_var(*ret)
            }
            Type::Vm(t) | Type::Snapshot(t) => self.contains_var(*t),
            Type::PendingCall(a, r) => self.contains_var(*a) || self.contains_var(*r),
            Type::Handle(m, r) => self.contains_var(*m) || self.contains_var(*r),
            _ => false,
        }
    }

    /// Return true when the type contains an effect variable inside a
    /// function-type row.
    pub fn contains_effect_var(&self, ty: TypeId) -> bool {
        match self.get(ty) {
            Type::Inst(_, args) => args.iter().any(|a| self.contains_effect_var(*a)),
            Type::List(e) => self.contains_effect_var(*e),
            Type::Map(k, v) => self.contains_effect_var(*k) || self.contains_effect_var(*v),
            Type::Tuple(elems) => elems.iter().any(|e| self.contains_effect_var(*e)),
            Type::Fn(params, _, ret, row) => {
                row.iter().any(|e| matches!(e, RowElem::Var(_)))
                    || params.iter().any(|p| self.contains_effect_var(*p))
                    || self.contains_effect_var(*ret)
            }
            Type::Vm(t) | Type::Snapshot(t) => self.contains_effect_var(*t),
            Type::PendingCall(a, r) => self.contains_effect_var(*a) || self.contains_effect_var(*r),
            Type::Handle(m, r) => self.contains_effect_var(*m) || self.contains_effect_var(*r),
            _ => false,
        }
    }

    /// Render one row for diagnostics.
    pub fn display_row(&self, row: &[RowElem]) -> String {
        let parts: Vec<String> = row
            .iter()
            .map(|e| match e {
                RowElem::Op(n) => self.row_names[*n as usize].clone(),
                RowElem::Var(v) => format!("e{v}"),
            })
            .collect();
        parts.join(", ")
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
            Type::Bytes => "Bytes".to_string(),
            Type::Digest => "Digest".to_string(),
            Type::Class(c) => self.classes[c.0 as usize].name.clone(),
            Type::Inst(c, args) => {
                let parts: Vec<String> = args.iter().map(|a| self.display(*a)).collect();
                format!("{}[{}]", self.classes[c.0 as usize].name, parts.join(", "))
            }
            Type::List(e) => format!("[{}]", self.display(*e)),
            Type::Map(k, v) => format!("{{{}: {}}}", self.display(*k), self.display(*v)),
            Type::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|e| self.display(*e)).collect();
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            Type::Fn(params, muts, ret, row) => {
                let mut out = String::from("(");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if muts.get(i).copied().unwrap_or(false) {
                        out.push_str("mut ");
                    }
                    out.push_str(&self.display(*p));
                }
                out.push_str(") -> ");
                out.push_str(&self.display(*ret));
                if !row.is_empty() {
                    out.push_str(" with ");
                    out.push_str(&self.display_row(row));
                }
                out
            }
            Type::Var(i) => format!("${i}"),
            Type::Fault => "Fault".to_string(),
            Type::Request => "Request".to_string(),
            Type::PolicyTable => "PolicyTable".to_string(),
            Type::EmptyVm => "EmptyVm".to_string(),
            Type::Vm(t) => format!("Vm[{}]", self.display(*t)),
            Type::SnapshotImage => "SnapshotImage".to_string(),
            Type::Snapshot(t) => format!("Snapshot[{}]", self.display(*t)),
            Type::FileHandle => "FileHandle".to_string(),
            Type::ResourceHandle => "ResourceHandle".to_string(),
            Type::PendingCall(a, r) => {
                format!("PendingCall[{}, {}]", self.display(*a), self.display(*r))
            }
            Type::Handle(m, r) => {
                format!("Handle[{}, {}]", self.display(*m), self.display(*r))
            }
            Type::Op(op, f) => {
                format!("Op[{}, {}]", lm_abi::op_name(*op), self.display(*f))
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

    fn class(store: &mut TypeStore, name: &str) -> ClassId {
        store.register_class(name, 0, ClassKind::Normal)
    }

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
        let a = store.intern_fn(vec![INT, BOOL], vec![false; 2], STRING, vec![]);
        let b = store.intern_fn(vec![INT, BOOL], vec![false; 2], STRING, vec![]);
        let c = store.intern_fn(vec![INT], vec![false], STRING, vec![]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let l1 = store.intern(Type::List(INT));
        let l2 = store.intern(Type::List(INT));
        assert_eq!(l1, l2);
        let t1 = store.intern(Type::Tuple(vec![INT, BOOL]));
        let t2 = store.intern(Type::Tuple(vec![INT, BOOL]));
        assert_eq!(t1, t2);
    }

    #[test]
    fn display_is_readable() {
        let mut store = TypeStore::new();
        let f = store.intern_fn(vec![INT, STRING], vec![false; 2], BOOL, vec![]);
        assert_eq!(store.display(f), "(Int, String) -> Bool");
        assert_eq!(store.display(UNIT), "()");
        let l = store.intern(Type::List(INT));
        assert_eq!(store.display(l), "[Int]");
        let m = store.intern(Type::Map(STRING, INT));
        assert_eq!(store.display(m), "{String: Int}");
        let t = store.intern(Type::Tuple(vec![INT, STRING]));
        assert_eq!(store.display(t), "(Int, String)");
        let one = store.intern(Type::Tuple(vec![INT]));
        assert_eq!(store.display(one), "(Int,)");
        let io = store.intern_row_name("Io.Print");
        let f2 = store.intern_fn(vec![], vec![], UNIT, vec![RowElem::Op(io)]);
        assert_eq!(store.display(f2), "() -> () with Io.Print");
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
        let animal = class(&mut store, "Animal");
        let dog = class(&mut store, "Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        assert!(store.compatible(t_animal, t_dog));
        assert!(!store.compatible(t_dog, t_animal));
    }

    #[test]
    fn generic_applications_are_invariant() {
        let mut store = TypeStore::new();
        let animal = class(&mut store, "Animal");
        let dog = class(&mut store, "Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        let l_animal = store.intern(Type::List(t_animal));
        let l_dog = store.intern(Type::List(t_dog));
        assert!(!store.compatible(l_animal, l_dog));
        assert!(!store.compatible(l_dog, l_animal));
        // User generic applications are also invariant.
        let boxc = store.register_class("Box", 1, ClassKind::Normal);
        let b_animal = store.intern(Type::Inst(boxc, vec![t_animal]));
        let b_dog = store.intern(Type::Inst(boxc, vec![t_dog]));
        assert!(!store.compatible(b_animal, b_dog));
        assert!(!store.compatible(b_dog, b_animal));
    }

    #[test]
    fn tuples_are_covariant() {
        let mut store = TypeStore::new();
        let animal = class(&mut store, "Animal");
        let dog = class(&mut store, "Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        let tup_animal = store.intern(Type::Tuple(vec![t_animal, INT]));
        let tup_dog = store.intern(Type::Tuple(vec![t_dog, INT]));
        assert!(store.compatible(tup_animal, tup_dog));
        assert!(!store.compatible(tup_dog, tup_animal));
    }

    #[test]
    fn functions_are_contravariant_in_parameters() {
        let mut store = TypeStore::new();
        let animal = class(&mut store, "Animal");
        let dog = class(&mut store, "Dog");
        store.set_class_parent(dog, animal);
        let t_animal = store.intern(Type::Class(animal));
        let t_dog = store.intern(Type::Class(dog));
        let takes_animal = store.intern_fn(vec![t_animal], vec![false], INT, vec![]);
        let takes_dog = store.intern_fn(vec![t_dog], vec![false], INT, vec![]);
        // A function of Animal serves where a function of Dog is expected.
        assert!(store.compatible(takes_dog, takes_animal));
        assert!(!store.compatible(takes_animal, takes_dog));
        // Results are covariant.
        let ret_animal = store.intern_fn(vec![], vec![], t_animal, vec![]);
        let ret_dog = store.intern_fn(vec![], vec![], t_dog, vec![]);
        assert!(store.compatible(ret_animal, ret_dog));
        assert!(!store.compatible(ret_dog, ret_animal));
    }

    #[test]
    fn function_rows_are_covariant_by_inclusion() {
        let mut store = TypeStore::new();
        let io_print = store.intern_row_name("Io.Print");
        let io = store.intern_row_name("Io");
        let pure_fn = store.intern_fn(vec![], vec![], UNIT, vec![]);
        let print_fn = store.intern_fn(vec![], vec![], UNIT, vec![RowElem::Op(io_print)]);
        let io_fn = store.intern_fn(vec![], vec![], UNIT, vec![RowElem::Op(io)]);
        assert!(store.compatible(print_fn, pure_fn));
        assert!(store.compatible(io_fn, print_fn));
        assert!(!store.compatible(pure_fn, print_fn));
        assert!(!store.compatible(print_fn, io_fn));
    }

    #[test]
    fn row_inclusion_uses_groups() {
        let mut store = TypeStore::new();
        let io_print = store.intern_row_name("Io.Print");
        let io = store.intern_row_name("Io");
        let fs = store.intern_row_name("Fs");
        assert!(store.row_included(&[], &[RowElem::Op(io_print)]));
        assert!(store.row_included(&[RowElem::Op(io_print)], &[RowElem::Op(io)]));
        assert!(!store.row_included(&[RowElem::Op(io)], &[RowElem::Op(io_print)]));
        assert!(!store.row_included(&[RowElem::Op(io_print)], &[RowElem::Op(fs)]));
        assert!(store.row_included(&[RowElem::Var(0)], &[RowElem::Var(0)]));
        assert!(!store.row_included(&[RowElem::Var(0)], &[RowElem::Op(io)]));
    }

    #[test]
    fn join_finds_the_common_ancestor() {
        let mut store = TypeStore::new();
        let animal = class(&mut store, "Animal");
        let dog = class(&mut store, "Dog");
        let cat = class(&mut store, "Cat");
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

    #[test]
    fn join_covers_generic_applications_and_tuples() {
        let mut store = TypeStore::new();
        let parent = store.register_class("Option", 1, ClassKind::EnumParent);
        let some = store.register_class("Option.Some", 1, ClassKind::EnumCase);
        let none = store.register_class("Option.None", 1, ClassKind::EnumCase);
        store.set_class_parent(some, parent);
        store.set_class_parent(none, parent);
        let some_int = store.intern(Type::Inst(some, vec![INT]));
        let none_int = store.intern(Type::Inst(none, vec![INT]));
        let opt_int = store.intern(Type::Inst(parent, vec![INT]));
        assert_eq!(store.join(some_int, none_int), Some(opt_int));
        let none_str = store.intern(Type::Inst(none, vec![STRING]));
        assert_eq!(store.join(some_int, none_str), None);
        let ta = store.intern(Type::Tuple(vec![some_int]));
        let tb = store.intern(Type::Tuple(vec![none_int]));
        let tj = store.intern(Type::Tuple(vec![opt_int]));
        assert_eq!(store.join(ta, tb), Some(tj));
    }

    #[test]
    fn substitution_replaces_variables() {
        let mut store = TypeStore::new();
        let v0 = store.intern(Type::Var(0));
        let list_v0 = store.intern(Type::List(v0));
        let list_int = store.intern(Type::List(INT));
        assert_eq!(store.substitute(list_v0, &[INT], &[]), list_int);
        let f = store.intern_fn(vec![v0], vec![false], v0, vec![RowElem::Var(0)]);
        let io = store.intern_row_name("Io");
        let got = store.substitute(f, &[STRING], &[vec![RowElem::Op(io)]]);
        let want = store.intern_fn(vec![STRING], vec![false], STRING, vec![RowElem::Op(io)]);
        assert_eq!(got, want);
    }

    #[test]
    fn canonical_row_sorts_and_dedups() {
        let mut store = TypeStore::new();
        let io = store.intern_row_name("Io");
        let fs = store.intern_row_name("Fs");
        let row = store.canonical_row(vec![
            RowElem::Var(1),
            RowElem::Op(io),
            RowElem::Op(fs),
            RowElem::Op(io),
            RowElem::Var(0),
        ]);
        assert_eq!(
            row,
            vec![
                RowElem::Op(fs),
                RowElem::Op(io),
                RowElem::Var(0),
                RowElem::Var(1)
            ]
        );
    }
}
