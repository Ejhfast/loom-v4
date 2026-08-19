//! Independent bytecode verifier.
//!
//! The verifier receives a decoded module and rejects it unless every
//! table and every function is well formed. It validates the type,
//! selector, type-application, and class tables first. It then
//! reconstructs the operand-stack types and the local-slot types at
//! each block entry with a worklist, and it checks jumps, calls,
//! generic substitution, claimed effect rows, field access, closure
//! creation, tuples, casts, and collection operations. A generic
//! function body is verified once with its type variables opaque;
//! call sites substitute the callee signature through the type
//! application. The verifier shares no code with the source checker.

use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{BcClassKind, BcRow, BcType, Func, Instr, Module};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

/// The largest operand-stack depth the verifier accepts for one function.
const MAX_STATIC_STACK: usize = 4096;

/// The largest portable tuple arity.
const MAX_TUPLE_ARITY: usize = 16;

/// The largest local slot count of one function. The bound rejects a
/// forged `local_count` before any allocation is sized from it.
const MAX_LOCAL_SLOTS: u32 = 65_536;

/// The deepest a type may nest.
///
/// A type child names an earlier table entry, so a table of N entries
/// can nest N deep. Every walk over a type costs at least its depth. A
/// crafted artifact must not make that work unbounded.
///
/// The bound makes a deep type unrepresentable. It also keeps a
/// recursive walk safe, so a later walk needs no iterative form to stay
/// inside the Rust stack.
///
/// Real code nests far below this limit. `lm-bytecode` bounds an
/// interface type at 32 for the same reason.
const MAX_TYPE_DEPTH: u32 = 128;

/// The largest dataflow footprint of one function: block count times
/// local slots. The bound keeps hostile inputs from demanding an
/// unbounded state table.
const MAX_DATAFLOW_CELLS: u64 = 1 << 24;

/// Canonical type-table indices for the primitive types. Every module
/// must begin its type table with these entries in this order.
pub const TY_UNIT: u32 = 0;
pub const TY_BOOL: u32 = 1;
pub const TY_INT: u32 = 2;
pub const TY_STR: u32 = 3;

/// A verification failure. The message names the exact position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    pub func: u32,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "function {}: {}", self.func, self.message)
    }
}

fn err(func: u32, message: impl Into<String>) -> VerifyError {
    VerifyError {
        func,
        message: message.into(),
    }
}

/// One join step: a finished type, or the element lists of two tuples
/// whose elements the walk still joins.
enum Flat {
    Type(u32),
    Tuple(Vec<u32>, Vec<u32>),
}

/// The abstract state at one program point. Types are indices into
/// the extended type universe. `None` marks a local slot without a
/// known value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct State {
    locals: Vec<Option<u32>>,
    stack: Vec<u32>,
}

/// The extended type universe: the module table plus the types
/// created by substitution during verification.
struct Universe {
    types: Vec<BcType>,
    index: HashMap<BcType, u32>,
}

impl Universe {
    fn intern(&mut self, ty: BcType) -> u32 {
        if let Some(idx) = self.index.get(&ty) {
            return *idx;
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.index.insert(ty, idx);
        idx
    }
}

/// Shared lookup context for one module.
struct Ctx<'m> {
    module: &'m Module,
    /// Class index to the type index of its `Class` entry, when the
    /// module contains one.
    class_ty: Vec<Option<u32>>,
    uni: RefCell<Universe>,
    /// The resolved pinned core definitions of this module.
    core: CoreLayout,
}

impl<'m> Ctx<'m> {
    fn ty(&self, idx: u32) -> BcType {
        self.uni.borrow().types[idx as usize].clone()
    }

    fn intern(&self, ty: BcType) -> u32 {
        self.uni.borrow_mut().intern(ty)
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
    fn ancestor_args(&self, child: u32, args: &[u32], ancestor: u32) -> Option<Vec<u32>> {
        let mut cur = child;
        let mut cur_args = args.to_vec();
        loop {
            if cur == ancestor {
                return Some(cur_args);
            }
            let class = &self.module.classes[cur as usize];
            let parent = class.parent()?;
            if !class.parent_args.is_empty() {
                cur_args = class.parent_args.clone();
            } else if self.module.classes[parent as usize].type_params == 0 {
                cur_args = Vec::new();
            }
            cur = parent;
        }
    }

    /// The parent type arguments of one class, in the class's own type
    /// parameters. An enum case has the implicit identity arguments.
    fn declared_parent_args(&self, cidx: u32) -> Vec<u32> {
        let class = &self.module.classes[cidx as usize];
        if !class.parent_args.is_empty() {
            return class.parent_args.clone();
        }
        let Some(parent) = class.parent() else {
            return Vec::new();
        };
        let arity = self.module.classes[parent as usize].type_params;
        (0..arity).map(|i| self.intern(BcType::Var(i))).collect()
    }

    /// The class that declares one selector, walking the ancestor
    /// chain from `class`.
    fn method_owner(&self, mut class: u32, selector: u32) -> Option<u32> {
        loop {
            let entry = &self.module.classes[class as usize];
            if entry.methods.iter().any(|(sel, _)| *sel == selector) {
                return Some(class);
            }
            class = entry.parent()?;
        }
    }

    /// The sort key of one row element, for canonical order checks.
    fn row_key(&self, elem: &BcRow) -> (u8, String, u32) {
        match elem {
            BcRow::Op(idx) => (
                0,
                self.module
                    .strings
                    .get(*idx as usize)
                    .cloned()
                    .unwrap_or_default(),
                0,
            ),
            BcRow::Var(v) => (1, String::new(), *v),
        }
    }

    /// Return true when the row is sorted and has no duplicate.
    fn row_canonical(&self, row: &[BcRow]) -> bool {
        row.windows(2)
            .all(|w| self.row_key(&w[0]) < self.row_key(&w[1]))
    }

    /// Return true when every element of `sub` is included in `sup`.
    fn row_included(&self, sub: &[BcRow], sup: &[BcRow]) -> bool {
        sub.iter().all(|elem| match elem {
            BcRow::Var(v) => sup.contains(&BcRow::Var(*v)),
            BcRow::Op(n) => {
                let name = &self.module.strings[*n as usize];
                sup.iter().any(|s| match s {
                    BcRow::Op(m) => {
                        let sup_name = &self.module.strings[*m as usize];
                        lm_abi::row_name_included(name, sup_name)
                    }
                    BcRow::Var(_) => false,
                })
            }
        })
    }

    /// Substitute effect variables in a row and re-canonicalize.
    fn row_subst(&self, row: &[BcRow], rows: &[Vec<BcRow>]) -> Vec<BcRow> {
        let mut out: Vec<BcRow> = Vec::new();
        for elem in row {
            match elem {
                BcRow::Var(v) => match rows.get(*v as usize) {
                    Some(replacement) => out.extend_from_slice(replacement),
                    None => out.push(*elem),
                },
                BcRow::Op(_) => out.push(*elem),
            }
        }
        out.sort_by_key(|e| self.row_key(e));
        out.dedup();
        out
    }

    /// The child type indices of one type, in declaration order.
    ///
    /// Every child of a universe entry has a smaller index, because
    /// `intern` appends and a caller builds a node from the bottom up.
    /// Each walk below reads this one list, so the walks cannot drift.
    fn type_children(&self, ty: u32, out: &mut Vec<u32>) {
        match self.ty(ty) {
            BcType::Inst(_, args) | BcType::Tuple(args) => out.extend(args),
            BcType::List(e)
            | BcType::Vm(e)
            | BcType::Wait(e)
            | BcType::Snapshot(e)
            | BcType::Op(_, e) => out.push(e),
            BcType::Map(a, b) | BcType::PendingCall(a, b) | BcType::Handle(a, b) => {
                out.push(a);
                out.push(b);
            }
            BcType::Fn(params, _, ret, _) => {
                out.extend(params);
                out.push(ret);
            }
            _ => {}
        }
    }

    /// Substitute type variables and effect variables in one type.
    ///
    /// The walk is iterative. A crafted artifact can nest a type as
    /// deeply as its type table allows, so a walk on the Rust stack
    /// would abort the host.
    fn subst(&self, ty: u32, targs: &[u32], rows: &[Vec<BcRow>]) -> u32 {
        if targs.is_empty() && rows.is_empty() {
            return ty;
        }
        let mut done: HashMap<u32, u32> = HashMap::new();
        let mut children: Vec<u32> = Vec::new();
        // Each entry pairs one type with the flag that says whether
        // its children already sit on the stack.
        let mut stack: Vec<(u32, bool)> = vec![(ty, false)];
        while let Some((cur, expanded)) = stack.pop() {
            if done.contains_key(&cur) {
                continue;
            }
            if !expanded {
                stack.push((cur, true));
                children.clear();
                self.type_children(cur, &mut children);
                for child in &children {
                    stack.push((*child, false));
                }
                continue;
            }
            let child = |c: u32| done.get(&c).copied().unwrap_or(c);
            let built = match self.ty(cur) {
                BcType::Var(i) => targs.get(i as usize).copied().unwrap_or(cur),
                BcType::Inst(c, args) => {
                    self.intern(BcType::Inst(c, args.iter().map(|a| child(*a)).collect()))
                }
                BcType::List(e) => self.intern(BcType::List(child(e))),
                BcType::Map(k, v) => self.intern(BcType::Map(child(k), child(v))),
                BcType::Tuple(elems) => {
                    self.intern(BcType::Tuple(elems.iter().map(|e| child(*e)).collect()))
                }
                BcType::Fn(params, muts, ret, row) => self.intern(BcType::Fn(
                    params.iter().map(|p| child(*p)).collect(),
                    muts,
                    child(ret),
                    self.row_subst(&row, rows),
                )),
                BcType::Vm(t) => self.intern(BcType::Vm(child(t))),
                BcType::Wait(t) => self.intern(BcType::Wait(child(t))),
                BcType::Snapshot(t) => self.intern(BcType::Snapshot(child(t))),
                BcType::PendingCall(a, r) => self.intern(BcType::PendingCall(child(a), child(r))),
                BcType::Handle(m, r) => self.intern(BcType::Handle(child(m), child(r))),
                _ => cur,
            };
            done.insert(cur, built);
        }
        done.get(&ty).copied().unwrap_or(ty)
    }

    /// Return true when a value of type `found` is valid where the
    /// code expects type `expected`.
    ///
    /// The walk is iterative. A tuple type and a function type both
    /// carry element types, and a crafted artifact can nest either as
    /// deeply as its type table allows.
    ///
    /// The work list holds pairs that must all hold. A pair the rules
    /// refuse answers false at once.
    fn is_subtype(&self, found: u32, expected: u32) -> bool {
        let mut work: Vec<(u32, u32)> = vec![(found, expected)];
        let mut seen: HashSet<(u32, u32)> = HashSet::new();
        while let Some((f, e)) = work.pop() {
            if f == e || !seen.insert((f, e)) {
                continue;
            }
            let ok = if let (Some((a, xs)), Some((b, ys))) =
                (self.as_instance(f), self.as_instance(e))
            {
                self.ancestor_args(a, &xs, b).as_ref() == Some(&ys)
            } else {
                match (self.ty(f), self.ty(e)) {
                    // A plain class position names no argument, so the
                    // walk to the ancestor must also reach it with no
                    // argument. A class that inherits an instantiated
                    // generic parent therefore fits no plain position
                    // of that parent.
                    (BcType::Class(a), BcType::Class(b)) => {
                        self.ancestor_args(a, &[], b) == Some(Vec::new())
                    }
                    // A class may inherit an instantiated generic
                    // parent, so a plain class instance can satisfy an
                    // application type.
                    (BcType::Class(a), BcType::Inst(b, ys)) => {
                        self.ancestor_args(a, &[], b).as_ref() == Some(&ys)
                    }
                    (BcType::Inst(a, xs), BcType::Class(b)) => {
                        self.ancestor_args(a, &xs, b) == Some(Vec::new())
                    }
                    (BcType::Inst(a, xs), BcType::Inst(b, ys)) => {
                        self.ancestor_args(a, &xs, b).as_ref() == Some(&ys)
                    }
                    (BcType::Tuple(xs), BcType::Tuple(ys)) => {
                        if xs.len() != ys.len() {
                            return false;
                        }
                        work.extend(xs.iter().zip(ys.iter()).map(|(x, y)| (*x, *y)));
                        true
                    }
                    (BcType::Fn(fp, fm, fr, frow), BcType::Fn(ep, em, er, erow)) => {
                        // A function that needs a `mut` argument is
                        // not valid where the expected type promises a
                        // read-only call. A parameter compares in the
                        // other direction.
                        if fp.len() != ep.len()
                            || !fm.iter().zip(em.iter()).all(|(f, e)| !*f || *e)
                            || !self.row_included(&frow, &erow)
                        {
                            return false;
                        }
                        work.extend(fp.iter().zip(ep.iter()).map(|(f, e)| (*e, *f)));
                        work.push((fr, er));
                        true
                    }
                    _ => false,
                }
            };
            if !ok {
                return false;
            }
        }
        true
    }

    /// Test one key for a map query.
    fn accepts_map_query_key(&self, found: u32, expected: u32) -> bool {
        if self.is_subtype(found, expected) {
            return true;
        }
        let Some(text) = self.core.text else {
            return false;
        };
        let is_text = |ty| {
            self.as_instance(ty).is_some_and(|(class, args)| {
                self.ancestor_args(class, &args, text) == Some(Vec::new())
            })
        };
        is_text(found) && is_text(expected)
    }

    /// Join two types at a control-flow merge. Classes join at their
    /// nearest common ancestor. Unrelated types have no join.
    ///
    /// The walk is iterative. Only a tuple carries a nested join, so
    /// the stack holds the tuple positions the answer still needs.
    fn join(&self, a: u32, b: u32) -> Option<u32> {
        // A post-order walk over the pair DAG. The flag marks a pair
        // whose element pairs already have answers.
        let mut stack: Vec<(u32, u32, bool)> = vec![(a, b, false)];
        let mut done: HashMap<(u32, u32), Option<u32>> = HashMap::new();
        while let Some((x, y, expanded)) = stack.pop() {
            if done.contains_key(&(x, y)) {
                continue;
            }
            if !expanded {
                match self.join_flat(x, y)? {
                    Flat::Type(id) => {
                        done.insert((x, y), Some(id));
                    }
                    Flat::Tuple(xs, ys) => {
                        stack.push((x, y, true));
                        for (ex, ey) in xs.iter().zip(ys.iter()).rev() {
                            if !done.contains_key(&(*ex, *ey)) {
                                stack.push((*ex, *ey, false));
                            }
                        }
                    }
                }
                continue;
            }
            let (BcType::Tuple(xs), BcType::Tuple(ys)) = (self.ty(x), self.ty(y)) else {
                return None;
            };
            let mut elems = Vec::with_capacity(xs.len());
            for pair in xs.iter().copied().zip(ys.iter().copied()) {
                let Some(Some(joined)) = done.get(&pair) else {
                    done.insert((x, y), None);
                    elems.clear();
                    break;
                };
                elems.push(*joined);
            }
            if elems.len() != xs.len() {
                return None;
            }
            let joined = self.intern(BcType::Tuple(elems));
            done.insert((x, y), Some(joined));
        }
        done.remove(&(a, b)).flatten()
    }

    /// One join step that needs no nested answer.
    fn join_flat(&self, a: u32, b: u32) -> Option<Flat> {
        if let (BcType::Tuple(xs), BcType::Tuple(ys)) = (self.ty(a), self.ty(b)) {
            if xs.len() != ys.len() {
                return None;
            }
            return Some(Flat::Tuple(xs, ys));
        }
        if self.is_subtype(a, b) {
            return Some(Flat::Type(b));
        }
        if self.is_subtype(b, a) {
            return Some(Flat::Type(a));
        }
        let (ca, xs) = self.as_instance(a)?;
        let (cb, ys) = self.as_instance(b)?;
        let (common, args) = self.common_applied_ancestor(ca, &xs, cb, &ys)?;
        let joined = if self.module.classes[common as usize].type_params == 0 {
            if Some(common) == self.core.int {
                BcType::Int
            } else if Some(common) == self.core.boolean {
                BcType::Bool
            } else if Some(common) == self.core.string {
                BcType::Str
            } else if Some(common) == self.core.bytes {
                BcType::Bytes
            } else {
                BcType::Class(common)
            }
        } else {
            BcType::Inst(common, args)
        };
        Some(Flat::Type(self.intern(joined)))
    }

    /// Find the nearest common ancestor with one equal application.
    fn common_applied_ancestor(
        &self,
        a: u32,
        a_args: &[u32],
        b: u32,
        b_args: &[u32],
    ) -> Option<(u32, Vec<u32>)> {
        let mut ancestor = Some(a);
        while let Some(class) = ancestor {
            let left = self.ancestor_args(a, a_args, class)?;
            if let Some(right) = self.ancestor_args(b, b_args, class) {
                if left == right {
                    return Some((class, left));
                }
            }
            ancestor = self.module.classes[class as usize].parent();
        }
        None
    }

    /// Find the nearest common nominal ancestor.
    fn common_ancestor(&self, a: u32, b: u32) -> Option<u32> {
        let mut ancestor = Some(a);
        while let Some(class) = ancestor {
            if self.ancestor_args(b, &[], class).is_some() {
                return Some(class);
            }
            ancestor = self.module.classes[class as usize].parent();
        }
        None
    }

    /// Resolve a selector on a class, walking the ancestor chain.
    fn find_method(&self, mut class: u32, selector: u32) -> Option<u32> {
        loop {
            let c = &self.module.classes[class as usize];
            for (sel, func) in &c.methods {
                if *sel == selector {
                    return Some(*func);
                }
            }
            match c.parent() {
                Some(p) => class = p,
                None => return None,
            }
        }
    }

    /// Test whether one class is an enum parent or an enum case.
    fn is_enum_class(&self, class: u32) -> bool {
        self.module
            .classes
            .get(class as usize)
            .map(|c| {
                matches!(
                    c.kind,
                    lm_bytecode::BcClassKind::Abstract | lm_bytecode::BcClassKind::Case
                )
            })
            .unwrap_or(false)
    }

    /// The nominal class and arguments of one instance type.
    fn as_instance(&self, ty: u32) -> Option<(u32, Vec<u32>)> {
        match self.ty(ty) {
            BcType::Int => self.core.int.map(|class| (class, vec![])),
            BcType::Bool => self.core.boolean.map(|class| (class, vec![])),
            BcType::Str => self.core.string.map(|class| (class, vec![])),
            BcType::Bytes => self.core.bytes.map(|class| (class, vec![])),
            BcType::Class(c) => Some((c, vec![])),
            BcType::Inst(c, args) => Some((c, args)),
            _ => None,
        }
    }

    /// Return true when the type is a heap object type. An `Op` value
    /// is an immediate.
    fn is_heap(&self, idx: u32) -> bool {
        if let BcType::Class(class) = self.ty(idx) {
            if Some(class) == self.core.char_value {
                return false;
            }
        }
        matches!(
            self.ty(idx),
            BcType::Str
                | BcType::Class(_)
                | BcType::Inst(_, _)
                | BcType::List(_)
                | BcType::Map(_, _)
                | BcType::Tuple(_)
                | BcType::Fn(_, _, _, _)
                | BcType::Digest
                | BcType::Fault
                | BcType::Request
                | BcType::PolicyTable
                | BcType::EmptyVm
                | BcType::Vm(_)
                | BcType::Wait(_)
                | BcType::PendingCall(_, _)
                | BcType::Handle(_, _)
                | BcType::SnapshotImage
                | BcType::Snapshot(_)
                | BcType::Bytes
                | BcType::FileHandle
                | BcType::ResourceHandle
        )
    }

    /// Check that every type variable inside `ty` is below `limit`
    /// and every row variable is below `elimit`.
    ///
    /// The walk is iterative, so a deeply nested type table costs heap
    /// instead of Rust stack.
    fn vars_bounded(&self, ty: u32, limit: u32, elimit: u32) -> bool {
        let mut stack: Vec<u32> = vec![ty];
        let mut children: Vec<u32> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        while let Some(cur) = stack.pop() {
            if !seen.insert(cur) {
                continue;
            }
            match self.ty(cur) {
                BcType::Var(i) => {
                    if i >= limit {
                        return false;
                    }
                }
                BcType::Fn(_, _, _, row) => {
                    if !self.row_vars_bounded(&row, elimit) {
                        return false;
                    }
                }
                _ => {}
            }
            children.clear();
            self.type_children(cur, &mut children);
            stack.extend(children.iter().copied());
        }
        true
    }

    fn row_vars_bounded(&self, row: &[BcRow], elimit: u32) -> bool {
        row.iter().all(|e| match e {
            BcRow::Var(v) => *v < elimit,
            BcRow::Op(_) => true,
        })
    }

    /// True when a claimed row covers one exact operation name: the
    /// row holds the exact name or one containing effect set.
    fn row_has_name(&self, row: &[BcRow], name: &str) -> bool {
        row.iter().any(|elem| match elem {
            BcRow::Op(idx) => {
                let text = &self.module.strings[*idx as usize];
                lm_abi::row_name_included(name, text)
            }
            BcRow::Var(_) => false,
        })
    }

    /// Convert one manifest type into a universe type index. The core
    /// enums must be present when a signature names them.
    fn abi_ty(&self, t: lm_abi::AbiType) -> Result<u32, String> {
        match t {
            lm_abi::AbiType::Primitive(primitive) => match primitive {
                lm_abi::AbiPrimitive::Unit => Ok(TY_UNIT),
                lm_abi::AbiPrimitive::Bool => Ok(TY_BOOL),
                lm_abi::AbiPrimitive::Int => Ok(TY_INT),
                lm_abi::AbiPrimitive::String => Ok(TY_STR),
                lm_abi::AbiPrimitive::Bytes => Ok(self.intern(BcType::Bytes)),
                lm_abi::AbiPrimitive::SnapshotImage => Ok(self.intern(BcType::SnapshotImage)),
            },
            lm_abi::AbiType::Core(core) => {
                let (slot, name) = match core {
                    lm_abi::AbiCore::Text => (self.core.text, "Text"),
                    lm_abi::AbiCore::Substring => (self.core.substring, "Substring"),
                    lm_abi::AbiCore::Char => (self.core.char_value, "Char"),
                    lm_abi::AbiCore::StringBuilder => (self.core.string_builder, "StringBuilder"),
                    lm_abi::AbiCore::ByteBuffer => (self.core.byte_buffer, "ByteBuffer"),
                    lm_abi::AbiCore::OpenOptions => (self.core.open_options, "OpenOptions"),
                    lm_abi::AbiCore::SeekFrom => (self.core.seek_from, "SeekFrom"),
                    lm_abi::AbiCore::IoError => (self.core.io_error, "IoError"),
                    lm_abi::AbiCore::FsError => (self.core.fs_error, "FsError"),
                    lm_abi::AbiCore::SnapshotError => (self.core.snapshot_error, "SnapshotError"),
                    lm_abi::AbiCore::IpAddress => (self.core.ip_address, "IpAddress"),
                    lm_abi::AbiCore::SocketAddress => (self.core.socket_address, "SocketAddress"),
                    lm_abi::AbiCore::NetError => (self.core.net_error, "NetError"),
                    lm_abi::AbiCore::TcpRead => (self.core.tcp_read, "TcpRead"),
                    lm_abi::AbiCore::Shutdown => (self.core.shutdown, "Shutdown"),
                    lm_abi::AbiCore::TlsError => (self.core.tls_error, "TlsError"),
                };
                self.plain_inst(slot, name)
            }
            lm_abi::AbiType::Native(native) => match native {
                lm_abi::AbiNative::FileHandle => Ok(self.intern(BcType::FileHandle)),
                lm_abi::AbiNative::TcpResource => {
                    self.plain_inst(self.core.tcp_resource, "TcpResource")
                }
                lm_abi::AbiNative::TcpStream => self.plain_inst(self.core.tcp_stream, "TcpStream"),
                lm_abi::AbiNative::TcpListener => {
                    self.plain_inst(self.core.tcp_listener, "TcpListener")
                }
                lm_abi::AbiNative::TlsStream => self.plain_inst(self.core.tls_stream, "TlsStream"),
            },
            lm_abi::AbiType::List(element) => {
                let element = self.abi_ty(*element)?;
                Ok(self.intern(BcType::List(element)))
            }
            lm_abi::AbiType::Tuple(elements) => {
                let mut types = Vec::with_capacity(elements.len());
                for element in elements {
                    types.push(self.abi_ty(*element)?);
                }
                Ok(self.intern(BcType::Tuple(types)))
            }
            lm_abi::AbiType::Apply(constructor, arguments) => {
                if arguments.len() != constructor.arity() {
                    return Err(format!(
                        "the ABI type {} has the wrong generic arity",
                        t.text()
                    ));
                }
                let class = match constructor {
                    lm_abi::AbiConstructor::Option => self.core.option,
                    lm_abi::AbiConstructor::Result => self.core.result,
                    lm_abi::AbiConstructor::Pair => self.core.pair,
                }
                .ok_or_else(|| {
                    format!(
                        "the module does not carry the pinned core {} definition",
                        constructor.text()
                    )
                })?;
                let mut types = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    types.push(self.abi_ty(*argument)?);
                }
                Ok(self.intern(BcType::Inst(class, types)))
            }
        }
    }

    /// The function type of one fixed operation as a universe index.
    fn fixed_sig_type(&self, op: u32) -> Result<u32, String> {
        let def = lm_abi::op(op);
        let mut params = Vec::with_capacity(def.params.len());
        for p in def.params {
            params.push(self.abi_ty(*p)?);
        }
        let ret = self.abi_ty(def.reply)?;
        let muts = vec![false; params.len()];
        Ok(self.intern(BcType::Fn(params, muts, ret, vec![])))
    }

    /// The argument-view type of one fixed operation: unit for a
    /// zero-parameter operation, a tuple otherwise.
    fn op_args_view(&self, op: u32) -> Result<u32, String> {
        let def = lm_abi::op(op);
        if def.params.is_empty() {
            return Ok(TY_UNIT);
        }
        let mut elems = Vec::with_capacity(def.params.len());
        for p in def.params {
            elems.push(self.abi_ty(*p)?);
        }
        Ok(self.intern(BcType::Tuple(elems)))
    }

    /// One VM event instance type, for example `RunResult[t]`.
    /// The instance type of one core family without type parameters.
    fn plain_inst(&self, parent: Option<u32>, what: &str) -> Result<u32, String> {
        let Some(parent) = parent else {
            return Err(format!(
                "the module does not carry the pinned core {what} definition"
            ));
        };
        Ok(self.intern(BcType::Class(parent)))
    }

    /// One `Result[ok, error]` instance type.
    fn result_inst(&self, ok: u32, error: u32) -> Result<u32, String> {
        let Some(family) = self.core.result else {
            return Err("the module does not carry the pinned core Result definition".to_string());
        };
        Ok(self.intern(BcType::Inst(family, vec![ok, error])))
    }

    /// The mailbox message type of one proc instance type. `None` when
    /// the type is not an instance of a subclass of the core class
    /// `Proc`.
    fn proc_mailbox(&self, ty: u32) -> Option<u32> {
        let proc = self.core.proc_class?;
        let (class, args) = self.as_instance(ty)?;
        let found = self.ancestor_args(class, &args, proc)?;
        found.first().copied()
    }

    fn event_inst(&self, parent: Option<u32>, what: &str, arg: u32) -> Result<u32, String> {
        let Some(parent) = parent else {
            return Err(format!(
                "the module does not carry the pinned core {what} definition"
            ));
        };
        Ok(self.intern(BcType::Inst(parent, vec![arg])))
    }
}

/// The verifier version. It takes part in the verified-code cache
/// key: a rule change invalidates every cached admission.
///
/// Version 9 adds byte types, resource types, and their operations.
/// Version 10 adds final class rules. Version 11 adds the `Int` role.
/// Version 12 adds the `Bool` role. Version 13 adds the `String` role
/// and String instructions. Version 14 adds Bytes and builder roles.
/// Version 15 adds the sealed Text family and immediate Char rules.
/// Version 16 adds the text extraction rules and structural enum
/// equality. Version 16 also named native TLS resources and their
/// service control on a separate branch, so version 17 is the first
/// that accepts both.
pub const VERIFIER_VERSION: u32 = 17;

/// Verify a full module. Every table and every function must pass.
///
/// The core layout comes from the core role table the artifact
/// carries. The verifier proves the shape of every filled slot, so it
/// reads no definition hash and no source name.
pub fn verify_module(module: &Module) -> Result<(), VerifyError> {
    let ctx = verify_structure(module)?;
    let imported = module.extern_funcs();
    for (idx, func) in module.funcs.iter().enumerate() {
        // An imported function has no body to check. The structural
        // pass already proved it carries a signature only.
        if imported[idx] {
            continue;
        }
        verify_func(&ctx, func, idx as u32)?;
    }
    Ok(())
}

/// Validate every module-level rule without the per-function
/// dataflow: the tables and the entry shape. The verified-code cache
/// may skip only the dataflow, never this pass, so a hash-equal
/// byte stream with a non-canonical table is rejected on every load.
pub fn verify_structure_only(module: &Module) -> Result<(), VerifyError> {
    verify_structure(module).map(|_| ())
}

fn verify_structure(module: &Module) -> Result<Ctx<'_>, VerifyError> {
    let core = lm_bytecode::corepin::declared_layout(module);
    let ctx = verify_tables(module, core)?;
    let entry = module.entry as usize;
    if entry >= module.funcs.len() {
        return Err(err(
            module.entry,
            format!(
                "entry index {} is not inside the function table of length {}",
                module.entry,
                module.funcs.len()
            ),
        ));
    }
    let entry_func = &module.funcs[entry];
    if !entry_func.params.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have parameters",
        ));
    }
    if !entry_func.captures.is_empty() {
        return Err(err(
            module.entry,
            "the entry function must not have captures",
        ));
    }
    if entry_func.type_params != 0 || entry_func.effect_params != 0 {
        return Err(err(module.entry, "the entry function must not be generic"));
    }
    Ok(ctx)
}

// ----------------------------------------------------------------
// The core role slots.
// ----------------------------------------------------------------

/// The core role indices. The order is `corepin::PINNED_LABELS`, and
/// the test `the_role_indices_match_the_pinned_labels` proves it.
const ROLE_OPTION: usize = 0;
const ROLE_OPTION_SOME: usize = 1;
const ROLE_OPTION_NONE: usize = 2;
const ROLE_RESULT: usize = 3;
const ROLE_RESULT_OK: usize = 4;
const ROLE_RESULT_ERR: usize = 5;
const ROLE_IO_ERROR: usize = 6;
const ROLE_IO_ERROR_FAILED: usize = 7;
const ROLE_RUN_RESULT: usize = 8;
const ROLE_RUN_DONE: usize = 9;
const ROLE_RUN_FAULT: usize = 10;
const ROLE_STEP_EVENT: usize = 11;
const ROLE_STEP_RAN: usize = 12;
const ROLE_STEP_WAITING: usize = 13;
const ROLE_STEP_DONE: usize = 14;
const ROLE_STEP_FAULT: usize = 15;
const ROLE_DRIVE_EVENT: usize = 16;
const ROLE_DRIVE_ASKED: usize = 17;
const ROLE_DRIVE_DONE: usize = 18;
const ROLE_DRIVE_FAULT: usize = 19;
const ROLE_RECV: usize = 20;
const ROLE_RECV_MSG: usize = 21;
const ROLE_RECV_CLOSED: usize = 22;
const ROLE_SEND_RESULT: usize = 23;
const ROLE_SEND_SENT: usize = 24;
const ROLE_SEND_CLOSED: usize = 25;
const ROLE_SEND_FAULT: usize = 26;
const ROLE_PROC_RESULT: usize = 27;
const ROLE_PROC_DONE: usize = 28;
const ROLE_PROC_FAULT: usize = 29;
const ROLE_PROC_ERROR: usize = 30;
const ROLE_PROC_ERROR_DEAD: usize = 31;
const ROLE_PROC_ERROR_NOT_PAUSED: usize = 32;
const ROLE_PROC_ERROR_ALREADY_PAUSED: usize = 33;
const ROLE_PROC_ERROR_IN_USE: usize = 34;
const ROLE_PROC_CLASS: usize = 35;
const ROLE_SNAPSHOT_ERROR: usize = 36;
const ROLE_SNAPSHOT_RESOURCE_ACTIVE: usize = 37;
const ROLE_SNAPSHOT_LIMIT_EXCEEDED: usize = 38;
const ROLE_SNAPSHOT_BAD_IMAGE: usize = 39;
const ROLE_RESTORE_ERROR: usize = 40;
const ROLE_RESTORE_LIMIT_EXCEEDED: usize = 41;
const ROLE_FS_ERROR: usize = 42;
const ROLE_FS_ERROR_CLOSED: usize = 43;
const ROLE_FS_ERROR_FAILED: usize = 44;
const ROLE_OPEN_OPTIONS: usize = 45;
const ROLE_OPEN_READ_ONLY: usize = 46;
const ROLE_OPEN_WRITE_ONLY: usize = 47;
const ROLE_OPEN_READ_WRITE: usize = 48;
const ROLE_OPEN_CREATE: usize = 49;
const ROLE_OPEN_CREATE_TRUNCATE: usize = 50;
const ROLE_OPEN_APPEND: usize = 51;
const ROLE_SEEK_FROM: usize = 52;
const ROLE_SEEK_START: usize = 53;
const ROLE_SEEK_CURRENT: usize = 54;
const ROLE_SEEK_END: usize = 55;
const ROLE_PAIR: usize = 68;
const ROLE_IP_ADDRESS: usize = 69;
const ROLE_IP_V4: usize = 70;
const ROLE_IP_V6: usize = 71;
const ROLE_SOCKET_ADDRESS: usize = 72;
const ROLE_NET_ERROR: usize = 73;
const ROLE_NET_INVALID_INPUT: usize = 74;
const ROLE_NET_NAME_NOT_FOUND: usize = 75;
const ROLE_NET_UNAVAILABLE: usize = 76;
const ROLE_NET_PERMISSION_DENIED: usize = 77;
const ROLE_NET_ADDRESS_IN_USE: usize = 78;
const ROLE_NET_CONNECTION_REFUSED: usize = 79;
const ROLE_NET_CONNECTION_RESET: usize = 80;
const ROLE_NET_NOT_CONNECTED: usize = 81;
const ROLE_NET_TIMED_OUT: usize = 82;
const ROLE_NET_CLOSED: usize = 83;
const ROLE_NET_LIMIT_EXCEEDED: usize = 84;
const ROLE_NET_UNSUPPORTED: usize = 85;
const ROLE_NET_FAILED: usize = 86;
const ROLE_TCP_READ: usize = 87;
const ROLE_TCP_READ_DATA: usize = 88;
const ROLE_TCP_READ_END: usize = 89;
const ROLE_SHUTDOWN: usize = 90;
const ROLE_SHUTDOWN_READ: usize = 91;
const ROLE_SHUTDOWN_WRITE: usize = 92;
const ROLE_SHUTDOWN_BOTH: usize = 93;
const ROLE_TCP_RESOURCE: usize = 94;
const ROLE_TCP_STREAM: usize = 95;
const ROLE_TCP_LISTENER: usize = 96;
const ROLE_TLS_ERROR: usize = 97;
const ROLE_TLS_INVALID_CONFIG: usize = 98;
const ROLE_TLS_HANDSHAKE: usize = 99;
const ROLE_TLS_CERTIFICATE: usize = 100;
const ROLE_TLS_PROTOCOL: usize = 101;
const ROLE_TLS_NETWORK: usize = 102;
const ROLE_TLS_CLOSED: usize = 103;
const ROLE_TLS_LIMIT_EXCEEDED: usize = 104;

/// The field shape one core arm must carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldShape {
    /// The type variable at this position of the family arity.
    Var(u32),
    Str,
    Int,
    Bytes,
    Fault,
    Request,
    /// A list of integers, for example the bounded machine path of
    /// `SnapshotError.ResourceActive`.
    ListInt,
    NetError,
}

/// One core family: the parent role, the generic arity, and the arm
/// roles in declaration order.
const CORE_FAMILIES: [(usize, u32, &[usize], &str); 20] = [
    (
        ROLE_OPTION,
        1,
        &[ROLE_OPTION_SOME, ROLE_OPTION_NONE],
        "Option",
    ),
    (ROLE_RESULT, 2, &[ROLE_RESULT_OK, ROLE_RESULT_ERR], "Result"),
    (ROLE_IO_ERROR, 0, &[ROLE_IO_ERROR_FAILED], "IoError"),
    (
        ROLE_RUN_RESULT,
        1,
        &[ROLE_RUN_DONE, ROLE_RUN_FAULT],
        "RunResult",
    ),
    (
        ROLE_STEP_EVENT,
        1,
        &[
            ROLE_STEP_RAN,
            ROLE_STEP_WAITING,
            ROLE_STEP_DONE,
            ROLE_STEP_FAULT,
        ],
        "StepEvent",
    ),
    (
        ROLE_DRIVE_EVENT,
        1,
        &[ROLE_DRIVE_ASKED, ROLE_DRIVE_DONE, ROLE_DRIVE_FAULT],
        "DriveEvent",
    ),
    (ROLE_RECV, 1, &[ROLE_RECV_MSG, ROLE_RECV_CLOSED], "Recv"),
    (
        ROLE_SEND_RESULT,
        0,
        &[ROLE_SEND_SENT, ROLE_SEND_CLOSED, ROLE_SEND_FAULT],
        "SendResult",
    ),
    (
        ROLE_PROC_RESULT,
        1,
        &[ROLE_PROC_DONE, ROLE_PROC_FAULT],
        "ProcResult",
    ),
    (
        ROLE_PROC_ERROR,
        0,
        &[
            ROLE_PROC_ERROR_DEAD,
            ROLE_PROC_ERROR_NOT_PAUSED,
            ROLE_PROC_ERROR_ALREADY_PAUSED,
            ROLE_PROC_ERROR_IN_USE,
        ],
        "ProcError",
    ),
    (
        ROLE_SNAPSHOT_ERROR,
        0,
        &[
            ROLE_SNAPSHOT_RESOURCE_ACTIVE,
            ROLE_SNAPSHOT_LIMIT_EXCEEDED,
            ROLE_SNAPSHOT_BAD_IMAGE,
        ],
        "SnapshotError",
    ),
    (
        ROLE_RESTORE_ERROR,
        0,
        &[ROLE_RESTORE_LIMIT_EXCEEDED],
        "RestoreError",
    ),
    (
        ROLE_FS_ERROR,
        0,
        &[ROLE_FS_ERROR_CLOSED, ROLE_FS_ERROR_FAILED],
        "FsError",
    ),
    (
        ROLE_OPEN_OPTIONS,
        0,
        &[
            ROLE_OPEN_READ_ONLY,
            ROLE_OPEN_WRITE_ONLY,
            ROLE_OPEN_READ_WRITE,
            ROLE_OPEN_CREATE,
            ROLE_OPEN_CREATE_TRUNCATE,
            ROLE_OPEN_APPEND,
        ],
        "OpenOptions",
    ),
    (
        ROLE_SEEK_FROM,
        0,
        &[ROLE_SEEK_START, ROLE_SEEK_CURRENT, ROLE_SEEK_END],
        "SeekFrom",
    ),
    (ROLE_IP_ADDRESS, 0, &[ROLE_IP_V4, ROLE_IP_V6], "IpAddress"),
    (
        ROLE_NET_ERROR,
        0,
        &[
            ROLE_NET_INVALID_INPUT,
            ROLE_NET_NAME_NOT_FOUND,
            ROLE_NET_UNAVAILABLE,
            ROLE_NET_PERMISSION_DENIED,
            ROLE_NET_ADDRESS_IN_USE,
            ROLE_NET_CONNECTION_REFUSED,
            ROLE_NET_CONNECTION_RESET,
            ROLE_NET_NOT_CONNECTED,
            ROLE_NET_TIMED_OUT,
            ROLE_NET_CLOSED,
            ROLE_NET_LIMIT_EXCEEDED,
            ROLE_NET_UNSUPPORTED,
            ROLE_NET_FAILED,
        ],
        "NetError",
    ),
    (
        ROLE_TCP_READ,
        0,
        &[ROLE_TCP_READ_DATA, ROLE_TCP_READ_END],
        "TcpRead",
    ),
    (
        ROLE_SHUTDOWN,
        0,
        &[ROLE_SHUTDOWN_READ, ROLE_SHUTDOWN_WRITE, ROLE_SHUTDOWN_BOTH],
        "Shutdown",
    ),
    (
        ROLE_TLS_ERROR,
        0,
        &[
            ROLE_TLS_INVALID_CONFIG,
            ROLE_TLS_HANDSHAKE,
            ROLE_TLS_CERTIFICATE,
            ROLE_TLS_PROTOCOL,
            ROLE_TLS_NETWORK,
            ROLE_TLS_CLOSED,
            ROLE_TLS_LIMIT_EXCEEDED,
        ],
        "TlsError",
    ),
];

/// The field layout every core arm must carry, by role.
const CORE_ARM_FIELDS: [(usize, &[FieldShape]); 67] = [
    (ROLE_OPTION_SOME, &[FieldShape::Var(0)]),
    (ROLE_OPTION_NONE, &[]),
    (ROLE_RESULT_OK, &[FieldShape::Var(0)]),
    (ROLE_RESULT_ERR, &[FieldShape::Var(1)]),
    (ROLE_IO_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_RUN_DONE, &[FieldShape::Var(0)]),
    (ROLE_RUN_FAULT, &[FieldShape::Fault]),
    (ROLE_STEP_RAN, &[]),
    (ROLE_STEP_WAITING, &[]),
    (ROLE_STEP_DONE, &[FieldShape::Var(0)]),
    (ROLE_STEP_FAULT, &[FieldShape::Fault]),
    (ROLE_DRIVE_ASKED, &[FieldShape::Request]),
    (ROLE_DRIVE_DONE, &[FieldShape::Var(0)]),
    (ROLE_DRIVE_FAULT, &[FieldShape::Fault]),
    (ROLE_RECV_MSG, &[FieldShape::Var(0)]),
    (ROLE_RECV_CLOSED, &[]),
    (ROLE_SEND_SENT, &[]),
    (ROLE_SEND_CLOSED, &[]),
    (ROLE_SEND_FAULT, &[FieldShape::Fault]),
    (ROLE_PROC_DONE, &[FieldShape::Var(0)]),
    (ROLE_PROC_FAULT, &[FieldShape::Fault]),
    (ROLE_PROC_ERROR_DEAD, &[]),
    (ROLE_PROC_ERROR_NOT_PAUSED, &[]),
    (ROLE_PROC_ERROR_ALREADY_PAUSED, &[]),
    (ROLE_PROC_ERROR_IN_USE, &[]),
    (
        ROLE_SNAPSHOT_RESOURCE_ACTIVE,
        &[FieldShape::ListInt, FieldShape::Str],
    ),
    (ROLE_SNAPSHOT_LIMIT_EXCEEDED, &[]),
    (ROLE_SNAPSHOT_BAD_IMAGE, &[FieldShape::Str]),
    (ROLE_RESTORE_LIMIT_EXCEEDED, &[]),
    (ROLE_FS_ERROR_CLOSED, &[]),
    (ROLE_FS_ERROR_FAILED, &[FieldShape::Str]),
    (ROLE_OPEN_READ_ONLY, &[]),
    (ROLE_OPEN_WRITE_ONLY, &[]),
    (ROLE_OPEN_READ_WRITE, &[]),
    (ROLE_OPEN_CREATE, &[]),
    (ROLE_OPEN_CREATE_TRUNCATE, &[]),
    (ROLE_OPEN_APPEND, &[]),
    (ROLE_SEEK_START, &[FieldShape::Int]),
    (ROLE_SEEK_CURRENT, &[FieldShape::Int]),
    (ROLE_SEEK_END, &[FieldShape::Int]),
    (ROLE_IP_V4, &[FieldShape::Bytes]),
    (ROLE_IP_V6, &[FieldShape::Bytes]),
    (ROLE_NET_INVALID_INPUT, &[FieldShape::Str]),
    (ROLE_NET_NAME_NOT_FOUND, &[FieldShape::Str]),
    (ROLE_NET_UNAVAILABLE, &[FieldShape::Str]),
    (ROLE_NET_PERMISSION_DENIED, &[FieldShape::Str]),
    (ROLE_NET_ADDRESS_IN_USE, &[FieldShape::Str]),
    (ROLE_NET_CONNECTION_REFUSED, &[FieldShape::Str]),
    (ROLE_NET_CONNECTION_RESET, &[FieldShape::Str]),
    (ROLE_NET_NOT_CONNECTED, &[FieldShape::Str]),
    (ROLE_NET_TIMED_OUT, &[FieldShape::Str]),
    (ROLE_NET_CLOSED, &[]),
    (ROLE_NET_LIMIT_EXCEEDED, &[FieldShape::Str]),
    (ROLE_NET_UNSUPPORTED, &[FieldShape::Str]),
    (ROLE_NET_FAILED, &[FieldShape::Str]),
    (ROLE_TCP_READ_DATA, &[FieldShape::Bytes]),
    (ROLE_TCP_READ_END, &[]),
    (ROLE_SHUTDOWN_READ, &[]),
    (ROLE_SHUTDOWN_WRITE, &[]),
    (ROLE_SHUTDOWN_BOTH, &[]),
    (ROLE_TLS_INVALID_CONFIG, &[FieldShape::Str]),
    (ROLE_TLS_HANDSHAKE, &[FieldShape::Str]),
    (ROLE_TLS_CERTIFICATE, &[FieldShape::Str]),
    (ROLE_TLS_PROTOCOL, &[FieldShape::Str]),
    (ROLE_TLS_NETWORK, &[FieldShape::NetError]),
    (ROLE_TLS_CLOSED, &[]),
    (ROLE_TLS_LIMIT_EXCEEDED, &[FieldShape::Str]),
];

/// Prove the shape of every declared core role slot.
///
/// The artifact declares which class fills each role. The verifier
/// never trusts that claim: it proves the kind, the generic arity, the
/// parent slot, and the exact field layout of every filled slot. A
/// crafted table therefore rejects instead of handing the runtime a
/// class it cannot allocate through.
///
/// The rules read structure only. No name and no definition hash takes
/// part, so a rename changes nothing the verifier reads.
fn verify_core_roles(module: &Module) -> Result<(), VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
    let slot = |role: usize| -> Option<u32> {
        let idx = module.core_roles[role];
        if idx == lm_bytecode::NO_ROLE {
            None
        } else {
            Some(idx)
        }
    };
    // A role slot names a class of this module, and no two roles name
    // one class. The decoder proves the same rule; a hand-built module
    // reaches the verifier without a decoder.
    let mut taken: Vec<u32> = Vec::new();
    for role in 0..lm_bytecode::CORE_ROLE_COUNT {
        let Some(idx) = slot(role) else { continue };
        if idx as usize >= module.classes.len() {
            return Err(terr(format!(
                "core role {role} names a class outside the table"
            )));
        }
        if taken.contains(&idx) {
            return Err(terr(format!(
                "core role {role} names a class another role took"
            )));
        }
        taken.push(idx);
    }
    for (family_role, arity, arm_roles, family) in CORE_FAMILIES {
        let Some(parent) = slot(family_role) else {
            // A family the artifact does not declare must declare no
            // arm either. The runtime allocates through the arms.
            for arm in arm_roles {
                if slot(*arm).is_some() {
                    return Err(terr(format!(
                        "the core family `{family}` declares an arm without its parent"
                    )));
                }
            }
            continue;
        };
        let class = &module.classes[parent as usize];
        if class.kind != BcClassKind::Abstract
            || class.type_params != arity
            || class.parent().is_some()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core family `{family}` names a class that is not its enum parent"
            )));
        }
        for arm_role in arm_roles {
            let Some(arm) = slot(*arm_role) else {
                return Err(terr(format!(
                    "the core family `{family}` resolves without every arm"
                )));
            };
            let arm_class = &module.classes[arm as usize];
            if arm_class.kind != BcClassKind::Case
                || arm_class.type_params != arity
                || arm_class.parent() != Some(parent)
            {
                return Err(terr(format!(
                    "the core family `{family}` names an arm that is not its case class"
                )));
            }
            let fields = CORE_ARM_FIELDS
                .iter()
                .find(|(role, _)| role == arm_role)
                .map(|(_, fields)| *fields)
                .expect("every arm role states its field layout");
            if arm_class.fields.len() != fields.len() {
                return Err(terr(format!(
                    "the core family `{family}` names an arm with the wrong field count"
                )));
            }
            for (position, want) in fields.iter().enumerate() {
                let found = &module.types[arm_class.fields[position].1 as usize];
                let ok = match want {
                    FieldShape::Var(i) => found == &BcType::Var(*i),
                    FieldShape::Str => found == &BcType::Str,
                    FieldShape::Int => found == &BcType::Int,
                    FieldShape::Bytes => found == &BcType::Bytes,
                    FieldShape::Fault => found == &BcType::Fault,
                    FieldShape::Request => found == &BcType::Request,
                    // The element index is read through `get`, because
                    // this pass must reject a crafted table instead of
                    // reaching outside the type table.
                    FieldShape::ListInt => match found {
                        BcType::List(elem) => {
                            module.types.get(*elem as usize) == Some(&BcType::Int)
                        }
                        _ => false,
                    },
                    FieldShape::NetError => {
                        slot(ROLE_NET_ERROR).is_some_and(|class| found == &BcType::Class(class))
                    }
                };
                if !ok {
                    return Err(terr(format!(
                        "the core family `{family}` names an arm whose field {position} \
                         has the wrong type"
                    )));
                }
            }
        }
    }
    // The proc class is not an enum family. It is one ordinary generic
    // class with one type parameter, no parent, and no field. The
    // mailbox rules of `Proc.Spawn` and `Proc.Recv` read the class
    // table through it, so its shape is proved here.
    if let Some(idx) = slot(ROLE_PROC_CLASS) {
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || class.type_params != 1
            || class.parent().is_some()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the core class `Proc` names a class that is not the proc parent".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(ROLE_PAIR) {
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || class.is_final
            || class.type_params != 2
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 2
            || fields[0] != &BcType::Var(0)
            || fields[1] != &BcType::Var(1)
        {
            return Err(terr(
                "the core role `Pair` does not name its two-field generic class".to_string(),
            ));
        }
    }
    if let Some(idx) = slot(ROLE_SOCKET_ADDRESS) {
        let Some(ip) = slot(ROLE_IP_ADDRESS) else {
            return Err(terr(
                "the SocketAddress role requires the IpAddress role".to_string(),
            ));
        };
        let class = &module.classes[idx as usize];
        let fields: Vec<&BcType> = class
            .fields
            .iter()
            .filter_map(|(_, ty)| module.types.get(*ty as usize))
            .collect();
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || fields.len() != 4
            || fields[0] != &BcType::Class(ip)
            || fields[1] != &BcType::Int
            || fields[2] != &BcType::Int
            || fields[3] != &BcType::Int
        {
            return Err(terr(
                "the SocketAddress role does not name its final value class".to_string(),
            ));
        }
    }
    let tcp_roles = [
        slot(ROLE_TCP_RESOURCE),
        slot(ROLE_TCP_STREAM),
        slot(ROLE_TCP_LISTENER),
    ];
    if tcp_roles.iter().any(Option::is_some) && tcp_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the TCP resource family resolves without every class".to_string(),
        ));
    }
    if let [Some(resource), Some(stream), Some(listener)] = tcp_roles {
        let base = &module.classes[resource as usize];
        if base.kind != BcClassKind::Normal
            || base.is_final
            || base.type_params != 0
            || base.parent().is_some()
            || !base.parent_args.is_empty()
            || !base.fields.is_empty()
        {
            return Err(terr(
                "the TcpResource role does not name its stateless base class".to_string(),
            ));
        }
        for (idx, name) in [(stream, "TcpStream"), (listener, "TcpListener")] {
            let class = &module.classes[idx as usize];
            if class.kind != BcClassKind::Normal
                || !class.is_final
                || class.type_params != 0
                || class.parent() != Some(resource)
                || !class.parent_args.is_empty()
                || !class.fields.is_empty()
            {
                return Err(terr(format!(
                    "the {name} role does not name its final resource class"
                )));
            }
        }
        for (idx, class) in module.classes.iter().enumerate() {
            if class.parent() == Some(resource) && idx as u32 != stream && idx as u32 != listener {
                return Err(terr(
                    "a class other than TcpStream or TcpListener extends TcpResource".to_string(),
                ));
            }
        }
    }
    let native_roles = [
        (lm_bytecode::corepin::ROLE_INT, "Int"),
        (lm_bytecode::corepin::ROLE_BOOL, "Bool"),
        (lm_bytecode::corepin::ROLE_BYTES, "Bytes"),
        (lm_bytecode::corepin::ROLE_STRING_BUILDER, "StringBuilder"),
        (lm_bytecode::corepin::ROLE_BYTE_BUFFER, "ByteBuffer"),
        (lm_bytecode::corepin::ROLE_CHAR, "Char"),
        (lm_bytecode::corepin::ROLE_TLS_STREAM, "TlsStream"),
    ];
    for (role, name) in native_roles {
        let Some(idx) = slot(role) else { continue };
        let class = &module.classes[idx as usize];
        if class.kind != BcClassKind::Normal
            || !class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(format!(
                "the core role `{name}` does not name a final stateless class"
            )));
        }
    }
    let text_roles = [
        slot(lm_bytecode::corepin::ROLE_TEXT),
        slot(lm_bytecode::corepin::ROLE_STRING),
        slot(lm_bytecode::corepin::ROLE_SUBSTRING),
    ];
    if text_roles.iter().any(Option::is_some) && text_roles.iter().any(Option::is_none) {
        return Err(terr(
            "the Text family resolves without every concrete class".to_string(),
        ));
    }
    if let [Some(text), Some(string), Some(substring)] = text_roles {
        let class = &module.classes[text as usize];
        if class.kind != BcClassKind::Abstract
            || class.is_final
            || class.type_params != 0
            || class.parent().is_some()
            || !class.parent_args.is_empty()
            || !class.fields.is_empty()
        {
            return Err(terr(
                "the core role `Text` does not name its abstract stateless parent".to_string(),
            ));
        }
        for (idx, name) in [(string, "String"), (substring, "Substring")] {
            let class = &module.classes[idx as usize];
            if class.kind != BcClassKind::Normal
                || !class.is_final
                || class.type_params != 0
                || class.parent() != Some(text)
                || !class.parent_args.is_empty()
                || !class.fields.is_empty()
            {
                return Err(terr(format!(
                    "the core role `{name}` does not name a final stateless Text class"
                )));
            }
        }
        for (idx, class) in module.classes.iter().enumerate() {
            if class.parent() == Some(text) && idx as u32 != string && idx as u32 != substring {
                return Err(terr(
                    "a class other than String or Substring extends Text".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// Validate the type, selector, application, class, and function
/// tables.
fn verify_tables(module: &Module, core: CoreLayout) -> Result<Ctx<'_>, VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
    // The selector table must hold no duplicate name. The canonical
    // identity encoding replaces a selector index with its name, so a
    // duplicate name lets two different dispatch keys hash alike. The
    // verified-code cache keys on that hash, so this rule keeps the
    // index-to-name map injective and belongs in the structural pass.
    let mut selector_names: HashMap<&str, u32> = HashMap::new();
    for (idx, name) in module.selectors.iter().enumerate() {
        if let Some(first) = selector_names.insert(name.as_str(), idx as u32) {
            return Err(terr(format!(
                "selector {idx} duplicates the name of selector {first}"
            )));
        }
    }
    // The type table must start with the canonical primitive prefix.
    let prefix = [BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
    if module.types.len() < prefix.len() || module.types[..prefix.len()] != prefix[..] {
        return Err(terr(
            "the type table does not start with Unit, Bool, Int, String".to_string(),
        ));
    }
    let mut index: HashMap<BcType, u32> = HashMap::new();
    for (idx, ty) in module.types.iter().enumerate() {
        if index.insert(ty.clone(), idx as u32).is_some() {
            return Err(terr(format!("type {idx} duplicates an earlier type entry")));
        }
        let check_ref = |child: u32| -> Result<(), VerifyError> {
            if child as usize >= idx {
                return Err(terr(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )));
            }
            Ok(())
        };
        match ty {
            BcType::Unit | BcType::Bool | BcType::Int | BcType::Str => {}
            BcType::Digest | BcType::Bytes | BcType::FileHandle | BcType::ResourceHandle => {}
            BcType::Var(_) => {}
            BcType::Class(c) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                if module.classes[*c as usize].type_params != 0 {
                    return Err(terr(format!(
                        "type {idx} names the generic class {c} without arguments"
                    )));
                }
            }
            BcType::Inst(c, args) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                let arity = module.classes[*c as usize].type_params;
                if arity == 0 || args.len() != arity as usize {
                    return Err(terr(format!(
                        "type {idx} applies class {c} with the wrong argument count"
                    )));
                }
                for a in args {
                    check_ref(*a)?;
                }
            }
            BcType::List(e) => check_ref(*e)?,
            BcType::Map(k, v) => {
                check_ref(*k)?;
                check_ref(*v)?;
                let text_key = match module.types[*k as usize] {
                    BcType::Class(class) => {
                        Some(class) == core.text || Some(class) == core.substring
                    }
                    _ => false,
                };
                if !text_key
                    && !matches!(
                        module.types[*k as usize],
                        BcType::Bool | BcType::Int | BcType::Str | BcType::Bytes
                    )
                {
                    return Err(terr(format!(
                        "type {idx} has a map key type outside Bool, Int, Text, String, Substring, or Bytes"
                    )));
                }
            }
            BcType::Tuple(elems) => {
                if elems.is_empty() || elems.len() > MAX_TUPLE_ARITY {
                    return Err(terr(format!(
                        "type {idx} is a tuple with an invalid arity {}",
                        elems.len()
                    )));
                }
                for e in elems {
                    check_ref(*e)?;
                }
            }
            BcType::Fn(params, muts, ret, row) => {
                if muts.len() != params.len() {
                    return Err(terr(format!(
                        "type {idx} has a function type whose mut markers do \
                         not align with the parameters"
                    )));
                }
                for p in params {
                    check_ref(*p)?;
                }
                check_ref(*ret)?;
                for elem in row {
                    if let BcRow::Op(s) = elem {
                        if *s as usize >= module.strings.len() {
                            return Err(terr(format!(
                                "type {idx} has a row with an invalid string index"
                            )));
                        }
                        if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                            return Err(terr(format!(
                                "type {idx} has a row that names `{}`, which is \
                                 not in the operation manifest",
                                module.strings[*s as usize]
                            )));
                        }
                    }
                }
            }
            BcType::Fault
            | BcType::Request
            | BcType::PolicyTable
            | BcType::EmptyVm
            | BcType::SnapshotImage => {}
            BcType::Vm(t) | BcType::Wait(t) | BcType::Snapshot(t) => check_ref(*t)?,
            BcType::PendingCall(a, r) | BcType::Handle(a, r) => {
                check_ref(*a)?;
                check_ref(*r)?;
            }
            BcType::Op(op, f) => {
                if *op >= lm_abi::OP_COUNT || lm_abi::op(*op).kind != lm_abi::OpKind::Fixed {
                    return Err(terr(format!(
                        "type {idx} names an invalid first-class operation slot {op}"
                    )));
                }
                check_ref(*f)?;
                // The function type must equal the manifest
                // signature; the check runs after the context exists.
            }
        }
    }
    // Class types by class index.
    let mut class_ty = vec![None; module.classes.len()];
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Class(c) = ty {
            class_ty[*c as usize] = Some(idx as u32);
        }
    }
    // Bound the nesting depth of the type table. A type child names an
    // earlier entry, so one forward pass gives the depth of each entry.
    let mut depth: Vec<u32> = Vec::with_capacity(module.types.len());
    let mut kids: Vec<u32> = Vec::new();
    for (idx, ty) in module.types.iter().enumerate() {
        kids.clear();
        lm_bytecode::closed::bc_children(ty, &mut kids);
        let mut own = 1u32;
        for child in &kids {
            let Some(child_depth) = depth.get(*child as usize) else {
                return Err(terr(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )));
            };
            own = own.max(child_depth + 1);
        }
        if own > MAX_TYPE_DEPTH {
            return Err(terr(format!(
                "type {idx} nests {own} deep, and the limit is {MAX_TYPE_DEPTH}"
            )));
        }
        depth.push(own);
    }
    let ctx = Ctx {
        module,
        class_ty,
        uni: RefCell::new(Universe {
            types: module.types.clone(),
            index,
        }),
        core,
    };
    // Row canonicality inside function types.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Fn(_, _, _, row) = ty {
            if !ctx.row_canonical(row) {
                return Err(terr(format!("type {idx} has a non-canonical row")));
            }
        }
    }
    // First-class operation types must carry the exact manifest
    // signature.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Op(op, f) = ty {
            let sig = ctx.fixed_sig_type(*op).map_err(&terr)?;
            if *f != sig {
                return Err(terr(format!(
                    "type {idx} claims a wrong signature for operation {}",
                    lm_abi::op_name(*op)
                )));
            }
        }
    }
    // Validate the type applications.
    for (aidx, app) in module.apps.iter().enumerate() {
        for t in &app.types {
            if *t as usize >= module.types.len() {
                return Err(terr(format!(
                    "application {aidx} references an invalid type index"
                )));
            }
        }
        for row in &app.rows {
            for elem in row {
                if let BcRow::Op(s) = elem {
                    if *s as usize >= module.strings.len() {
                        return Err(terr(format!(
                            "application {aidx} has a row with an invalid string index"
                        )));
                    }
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                        return Err(terr(format!(
                            "application {aidx} has a row that names `{}`, which \
                             is not in the operation manifest",
                            module.strings[*s as usize]
                        )));
                    }
                }
            }
            if !ctx.row_canonical(row) {
                return Err(terr(format!("application {aidx} has a non-canonical row")));
            }
        }
    }
    // Validate the import slots. Each slot names one definition of its
    // own kind, and no definition takes two slots. An imported
    // definition carries a signature and no body: the linker replaces
    // it with the provider definition, and the loader admits a module
    // only when the import table is empty.
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    {
        let mut claimed_classes = vec![false; module.classes.len()];
        let mut claimed_funcs = vec![false; module.funcs.len()];
        for (idx, import) in module.imports.iter().enumerate() {
            let ierr = |message: String| terr(format!("import {idx}: {message}"));
            if import.module.is_empty() || import.name.is_empty() {
                return Err(ierr("the slot needs a module path and a name".to_string()));
            }
            let claimed = if import.kind.is_func() {
                &mut claimed_funcs
            } else {
                &mut claimed_classes
            };
            let at = import.def as usize;
            if at >= claimed.len() {
                return Err(ierr("the definition index is out of range".to_string()));
            }
            if claimed[at] {
                return Err(ierr(
                    "the definition already has an import slot".to_string(),
                ));
            }
            claimed[at] = true;
        }
    }
    if extern_funcs.get(module.entry as usize) == Some(&true) {
        return Err(terr("the entry function cannot be imported".to_string()));
    }
    // Validate classes.
    for (cidx, class) in module.classes.iter().enumerate() {
        let cerr = |message: String| terr(format!("class {cidx}: {message}"));
        if extern_classes[cidx] {
            if let Some(p) = class.parent() {
                if !extern_classes[p as usize] {
                    return Err(cerr(
                        "an imported class cannot inherit a local class".to_string(),
                    ));
                }
            }
        }
        match class.kind {
            BcClassKind::Abstract => {
                if class.is_final {
                    return Err(cerr("an abstract class cannot be final".to_string()));
                }
                if class.parent().is_some() {
                    return Err(cerr("an abstract class cannot inherit".to_string()));
                }
            }
            BcClassKind::Case => {
                if class.is_final {
                    return Err(cerr("a case class cannot use the final flag".to_string()));
                }
                let Some(p) = class.parent() else {
                    return Err(cerr("a case class needs its enum parent".to_string()));
                };
                if p as usize >= cidx {
                    return Err(cerr(format!("parent {p} is not an earlier class entry")));
                }
                if module.classes[p as usize].kind != BcClassKind::Abstract {
                    return Err(cerr(
                        "a case class parent must be an abstract enum parent".to_string(),
                    ));
                }
                if module.classes[p as usize].type_params != class.type_params {
                    return Err(cerr(
                        "a case class must keep the family type arity".to_string(),
                    ));
                }
            }
            BcClassKind::Normal => {}
        }
        if let Some(p) = class.parent() {
            if p as usize >= cidx {
                return Err(cerr(format!("parent {p} is not an earlier class entry")));
            }
            let parent = &module.classes[p as usize];
            if parent.is_final {
                return Err(cerr("a class cannot inherit a final class".to_string()));
            }
            if class.kind != BcClassKind::Case {
                let native_text_child =
                    Some(cidx as u32) == core.string || Some(cidx as u32) == core.substring;
                if Some(p) == core.text && !native_text_child {
                    return Err(cerr(
                        "only String and Substring can inherit Text".to_string(),
                    ));
                }
                if parent.kind != BcClassKind::Normal && Some(p) != core.text {
                    return Err(cerr(
                        "only a case class may inherit a sealed enum class".to_string(),
                    ));
                }
                // A generic parent carries one closed type argument per
                // parameter. A generic class still declares no parent,
                // so no parent argument holds a type variable.
                if class.type_params != 0 {
                    return Err(cerr("a generic class cannot declare a parent".to_string()));
                }
                if class.parent_args.len() != parent.type_params as usize {
                    return Err(cerr(
                        "the parent type argument count does not match the parent arity"
                            .to_string(),
                    ));
                }
                for arg in &class.parent_args {
                    if *arg as usize >= module.types.len() {
                        return Err(cerr("a parent type argument is out of range".to_string()));
                    }
                    if !ctx.vars_bounded(*arg, 0, 0) {
                        return Err(cerr(
                            "a parent type argument holds a type variable".to_string(),
                        ));
                    }
                }
            } else if !class.parent_args.is_empty() {
                return Err(cerr(
                    "a case class carries no parent type argument".to_string(),
                ));
            }
            // The field layout must extend the parent layout exactly.
            // A generic parent contributes its fields with the declared
            // arguments applied.
            let inherited: Vec<(String, u32)> = parent
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), ctx.subst(*ty, &class.parent_args, &[])))
                .collect();
            if class.fields.len() < inherited.len()
                || class.fields[..inherited.len()] != inherited[..]
            {
                return Err(cerr(
                    "the field layout does not extend the parent layout".to_string(),
                ));
            }
        } else if !class.parent_args.is_empty() {
            return Err(cerr(
                "a class without a parent carries no parent type argument".to_string(),
            ));
        }
        for (fname, fty) in &class.fields {
            if *fty as usize >= module.types.len() {
                return Err(cerr(format!("field `{fname}` has an invalid type index")));
            }
            if !ctx.vars_bounded(*fty, class.type_params, 0) {
                return Err(cerr(format!(
                    "field `{fname}` uses a type variable outside the class arity"
                )));
            }
        }
        // The canonical self type of the class.
        let own_ty = if core.int == Some(cidx as u32) {
            Some(TY_INT)
        } else if core.boolean == Some(cidx as u32) {
            Some(TY_BOOL)
        } else if core.string == Some(cidx as u32) {
            Some(TY_STR)
        } else if core.bytes == Some(cidx as u32) {
            ctx.uni.borrow().index.get(&BcType::Bytes).copied()
        } else if class.type_params == 0 {
            ctx.class_ty[cidx]
        } else {
            let vars: Vec<u32> = (0..class.type_params)
                .map(|i| ctx.uni.borrow().index.get(&BcType::Var(i)).copied())
                .collect::<Option<Vec<u32>>>()
                .unwrap_or_default()
                .to_vec();
            if vars.len() == class.type_params as usize {
                ctx.uni
                    .borrow()
                    .index
                    .get(&BcType::Inst(cidx as u32, vars))
                    .copied()
            } else {
                None
            }
        };
        let mut seen = Vec::new();
        for (sel, func) in &class.methods {
            if *sel as usize >= module.selectors.len() {
                return Err(cerr(format!("selector {sel} does not exist")));
            }
            if seen.contains(sel) {
                return Err(cerr(format!("selector {sel} appears twice")));
            }
            seen.push(*sel);
            if *func as usize >= module.funcs.len() {
                return Err(cerr(format!("method function {func} does not exist")));
            }
            if extern_funcs[*func as usize] != extern_classes[cidx] {
                return Err(cerr(format!(
                    "method function {func} does not follow the import state of \
                     its class"
                )));
            }
            let f = &module.funcs[*func as usize];
            if !f.captures.is_empty() {
                return Err(cerr("a method function must not have captures".to_string()));
            }
            if f.type_params < class.type_params {
                return Err(cerr(format!(
                    "method function {func} does not carry the class type arity"
                )));
            }
            let self_ok = match (f.params.first(), own_ty) {
                (Some(p0), Some(t)) => *p0 == t,
                _ => false,
            };
            if !self_ok {
                return Err(cerr(format!(
                    "method function {func} does not receive this class as `self`"
                )));
            }
            // Override compatibility with the nearest ancestor method.
            // The base signature is read in the subclass view, so a
            // generic ancestor compares with its arguments applied.
            if let Some(parent) = class.parent() {
                if let Some(base_func) = ctx.find_method(parent, *sel) {
                    let base = &module.funcs[base_func as usize];
                    let owner = ctx
                        .method_owner(parent, *sel)
                        .expect("the base method has a declaring class");
                    let owner_arity = module.classes[owner as usize].type_params;
                    let start_args = ctx.declared_parent_args(cidx as u32);
                    let owner_args = ctx
                        .ancestor_args(parent, &start_args, owner)
                        .expect("the declaring class is an ancestor");
                    let Some(own_count) = base.type_params.checked_sub(owner_arity) else {
                        return Err(cerr(format!(
                            "the base of selector {sel} does not carry its class type arity"
                        )));
                    };
                    if f.type_params != class.type_params + own_count
                        || base.effect_params != f.effect_params
                    {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the generic arity"
                        )));
                    }
                    let mut targs = owner_args;
                    for i in 0..own_count {
                        targs.push(ctx.intern(BcType::Var(class.type_params + i)));
                    }
                    let base_params: Vec<u32> = base
                        .params
                        .iter()
                        .map(|p| ctx.subst(*p, &targs, &[]))
                        .collect();
                    if base_params.len() != f.params.len() || base_params[1..] != f.params[1..] {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter types"
                        )));
                    }
                    // `get` keeps a malformed marker vector from
                    // panicking before the signature validation runs.
                    if base.param_muts.get(1..) != f.param_muts.get(1..) {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter mut markers"
                        )));
                    }
                    let base_ret = ctx.subst(base.ret, &targs, &[]);
                    if !ctx.is_subtype(f.ret, base_ret) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the result type"
                        )));
                    }
                    if !ctx.row_included(&f.row, &base.row) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the effect row"
                        )));
                    }
                }
            }
        }
    }
    // Validate the declared core role slots. The class table is
    // validated above, so every field type index is inside the type
    // table by now.
    verify_core_roles(module)?;
    // Validate function signatures and the declared local-type
    // tables. The verifier validates the table instead of trusting
    // it: entries must be valid types, the parameter prefix must
    // equal the signature, and every variable must be in scope.
    for (fidx, func) in module.funcs.iter().enumerate() {
        if extern_funcs[fidx] {
            // An imported function is a declaration: a signature with
            // no body, no captures, and no extra local slots.
            if !func.blocks.is_empty() {
                return Err(err(fidx as u32, "an imported function must have no body"));
            }
            if !func.captures.is_empty() {
                return Err(err(
                    fidx as u32,
                    "an imported function must have no captures",
                ));
            }
            if func.local_types.len() != func.params.len() {
                return Err(err(
                    fidx as u32,
                    "an imported function must declare only its parameter slots",
                ));
            }
        }
        if func.param_muts.len() != func.params.len() {
            return Err(err(
                fidx as u32,
                "the parameter mut markers do not align with the parameters",
            ));
        }
        if func.local_types.len() < func.params.len() {
            return Err(err(fidx as u32, "more parameters than local slots"));
        }
        if func.local_types[..func.params.len()] != func.params[..] {
            return Err(err(
                fidx as u32,
                "the local-type table prefix does not equal the parameter types",
            ));
        }
        for t in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain(func.local_types.iter())
            .chain([&func.ret])
        {
            if *t as usize >= module.types.len() {
                return Err(err(
                    fidx as u32,
                    "the signature references an invalid type index",
                ));
            }
            if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                return Err(err(
                    fidx as u32,
                    "the signature uses a variable outside the declared generic arity",
                ));
            }
        }
        for elem in &func.row {
            match elem {
                BcRow::Op(s) => {
                    if *s as usize >= module.strings.len() {
                        return Err(err(
                            fidx as u32,
                            "the declared row references an invalid string index",
                        ));
                    }
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
                        return Err(err(
                            fidx as u32,
                            format!(
                                "the declared row names `{}`, which is not in the \
                                 operation manifest",
                                module.strings[*s as usize]
                            ),
                        ));
                    }
                }
                BcRow::Var(v) => {
                    if *v >= func.effect_params {
                        return Err(err(
                            fidx as u32,
                            "the declared row uses an effect variable outside the arity",
                        ));
                    }
                }
            }
        }
        if !ctx.row_canonical(&func.row) {
            return Err(err(fidx as u32, "the declared row is not canonical"));
        }
    }
    Ok(ctx)
}

/// The expected operand count of one perform instruction. VM control
/// operations count their receiver.
fn perform_argc(op: u32) -> u32 {
    let def = lm_abi::op(op);
    match def.kind {
        lm_abi::OpKind::Fixed => def.params.len() as u32,
        lm_abi::OpKind::VmControl => match op {
            lm_abi::OP_VM_NEW => 0,
            lm_abi::OP_VM_RUN
            | lm_abi::OP_VM_STEP
            | lm_abi::OP_VM_DRIVE
            | lm_abi::OP_VM_DRIVE_WAIT
            | lm_abi::OP_VM_TABLE
            | lm_abi::OP_VM_HANDLES
            | lm_abi::OP_VM_RESOURCE_IS_OPEN
            | lm_abi::OP_VM_RESOURCE_CLOSE
            | lm_abi::OP_VM_RESOURCE_KIND => 1,
            lm_abi::OP_VM_DISPATCH => 2,
            lm_abi::OP_VM_FROM_FN
            | lm_abi::OP_VM_ANSWER
            | lm_abi::OP_VM_REJECT
            | lm_abi::OP_VM_SERVE_TCP_STREAM => 3,
            lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_CLOSE
            | lm_abi::OP_PROC_DONE
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME
            | lm_abi::OP_PROC_RECV
            | lm_abi::OP_PROC_RECV_WAIT
            | lm_abi::OP_WAIT_WAIT
            | lm_abi::OP_WAIT_CANCEL => 1,
            lm_abi::OP_PROC_SEND => 2,
            lm_abi::OP_PROC_SPAWN => 3,
            lm_abi::OP_VM_SNAPSHOT_SELF => 0,
            lm_abi::OP_VM_SNAPSHOT_HELD | lm_abi::OP_VM_LOAD_SNAPSHOT => 1,
            lm_abi::OP_VM_RESTORE
            | lm_abi::OP_VM_RESOURCE
            | lm_abi::OP_VM_SERVE_FILE
            | lm_abi::OP_VM_SERVE_TCP_LISTENER
            | lm_abi::OP_VM_SERVE_TLS_STREAM
            | lm_abi::OP_VM_DRIVE_FOR
            | lm_abi::OP_VM_SNAPSHOT_WAIT_HELD
            | lm_abi::OP_PROC_SNAPSHOT_WAIT
            | lm_abi::OP_VM_RESOURCE_SAME
            | lm_abi::OP_WAIT_CHOOSE => 2,
            _ => unreachable!("every VmControl slot has an arity"),
        },
    }
}

/// Validate one type application against a callee's generic arity and
/// the caller's variable scope.
fn check_app(
    ctx: &Ctx<'_>,
    caller: &Func,
    fidx: u32,
    at: &dyn Fn(&str) -> String,
    app_idx: u32,
    want_types: u32,
    want_rows: u32,
) -> Result<(), VerifyError> {
    let app = ctx
        .module
        .apps
        .get(app_idx as usize)
        .ok_or_else(|| err(fidx, at("type application index out of range")))?;
    if app.types.len() != want_types as usize {
        return Err(err(fidx, at("type application arity mismatch")));
    }
    if app.rows.len() != want_rows as usize {
        return Err(err(fidx, at("type application row arity mismatch")));
    }
    for t in &app.types {
        if !ctx.vars_bounded(*t, caller.type_params, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application uses a variable outside the caller scope"),
            ));
        }
    }
    for row in &app.rows {
        if !ctx.row_vars_bounded(row, caller.effect_params) {
            return Err(err(
                fidx,
                at("type application row uses a variable outside the caller scope"),
            ));
        }
    }
    Ok(())
}

fn verify_func(ctx: &Ctx<'_>, func: &Func, fidx: u32) -> Result<(), VerifyError> {
    let module = ctx.module;
    // Reject a forged slot count before any allocation is sized from
    // it. The dataflow pass allocates one state cell per block and
    // local, so both bounds run first.
    if func.local_count() > MAX_LOCAL_SLOTS {
        return Err(err(
            fidx,
            format!(
                "the local slot count {} exceeds the portable limit {MAX_LOCAL_SLOTS}",
                func.local_count()
            ),
        ));
    }
    if (func.blocks.len() as u64) * (func.local_count() as u64 + 1) > MAX_DATAFLOW_CELLS {
        return Err(err(
            fidx,
            "the function exceeds the verifier state budget; split it",
        ));
    }
    if func.blocks.is_empty() {
        return Err(err(fidx, "the function has no blocks"));
    }
    // Structural pass: every block ends with a terminator and every
    // operand index is inside its table.
    for (bidx, block) in func.blocks.iter().enumerate() {
        match block.last() {
            Some(last) if last.is_terminator() => {}
            _ => {
                return Err(err(
                    fidx,
                    format!("block {bidx} does not end with a terminator"),
                ));
            }
        }
        for (iidx, instr) in block.iter().enumerate() {
            if instr.is_terminator() && iidx + 1 != block.len() {
                return Err(err(
                    fidx,
                    format!("block {bidx} has a terminator before its end"),
                ));
            }
            let at = |what: &str| format!("block {bidx}, instruction {iidx}: {what}");
            let at_dyn: &dyn Fn(&str) -> String = &at;
            match instr {
                Instr::ConstStr(idx) => {
                    if *idx as usize >= module.strings.len() {
                        return Err(err(fidx, at("string index out of range")));
                    }
                }
                Instr::LoadLocal(slot) | Instr::StoreLocal(slot) => {
                    if *slot >= func.local_count() {
                        return Err(err(fidx, at("local slot out of range")));
                    }
                }
                Instr::Call(callee) => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if target.type_params != 0 || target.effect_params != 0 {
                        return Err(err(fidx, at("a generic callee needs a type application")));
                    }
                }
                Instr::CallG { func: callee, app } => {
                    let Some(target) = module.funcs.get(*callee as usize) else {
                        return Err(err(fidx, at("call target out of range")));
                    };
                    if !target.captures.is_empty() {
                        return Err(err(fidx, at("direct call to a function with captures")));
                    }
                    if target.type_params == 0 && target.effect_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic callee")));
                    }
                    check_app(
                        ctx,
                        func,
                        fidx,
                        at_dyn,
                        *app,
                        target.type_params,
                        target.effect_params,
                    )?;
                }
                Instr::CallVirtual { selector, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                }
                Instr::CallVirtualG { selector, app, .. } => {
                    if *selector as usize >= module.selectors.len() {
                        return Err(err(fidx, at("selector index out of range")));
                    }
                    // The full arity check needs the receiver type and
                    // runs in the dataflow pass. The structural pass
                    // bounds the index and the variable scopes, so the
                    // dataflow pass can index the table safely.
                    let Some(a) = module.apps.get(*app as usize) else {
                        return Err(err(fidx, at("type application index out of range")));
                    };
                    for t in &a.types {
                        if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application uses a variable outside the caller scope"),
                            ));
                        }
                    }
                    for row in &a.rows {
                        if !ctx.row_vars_bounded(row, func.effect_params) {
                            return Err(err(
                                fidx,
                                at("type application row uses a variable outside the caller scope"),
                            ));
                        }
                    }
                }
                Instr::MakeClosure { func: f, captures } => {
                    let Some(target) = module.funcs.get(*f as usize) else {
                        return Err(err(fidx, at("closure function out of range")));
                    };
                    if target.captures.len() != *captures as usize {
                        return Err(err(fidx, at("closure capture count mismatch")));
                    }
                    // A closure body shares the generic scope of the
                    // function that creates it, so it must keep the
                    // same arity. A target that declares no generic
                    // parameter at all has no free variable to bind:
                    // its signature is closed, so any scope may close
                    // over it. The `spawn` sugar takes that path.
                    let closed = target.type_params == 0 && target.effect_params == 0;
                    if !closed
                        && (target.type_params != func.type_params
                            || target.effect_params != func.effect_params)
                    {
                        return Err(err(
                            fidx,
                            at("a closure body must keep the enclosing generic arity"),
                        ));
                    }
                }
                Instr::LoadCapture(idx) => {
                    if *idx as usize >= func.captures.len() {
                        return Err(err(fidx, at("capture index out of range")));
                    }
                }
                Instr::New(class) => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    let native = [
                        ctx.core.int,
                        ctx.core.boolean,
                        ctx.core.string,
                        ctx.core.substring,
                        ctx.core.char_value,
                        ctx.core.bytes,
                        ctx.core.string_builder,
                        ctx.core.byte_buffer,
                    ];
                    if native.contains(&Some(*class)) {
                        return Err(err(fidx, at("New cannot allocate a native core class")));
                    }
                    if c.type_params != 0 {
                        return Err(err(fidx, at("a generic class needs a type application")));
                    }
                }
                Instr::NewG { class, app } => {
                    let Some(c) = module.classes.get(*class as usize) else {
                        return Err(err(fidx, at("class index out of range")));
                    };
                    if c.kind == BcClassKind::Abstract {
                        return Err(err(fidx, at("cannot allocate an abstract enum parent")));
                    }
                    let native = [
                        ctx.core.int,
                        ctx.core.boolean,
                        ctx.core.string,
                        ctx.core.substring,
                        ctx.core.char_value,
                        ctx.core.bytes,
                        ctx.core.string_builder,
                        ctx.core.byte_buffer,
                    ];
                    if native.contains(&Some(*class)) {
                        return Err(err(fidx, at("NewG cannot allocate a native core class")));
                    }
                    if c.type_params == 0 {
                        return Err(err(fidx, at("a type application on a non-generic class")));
                    }
                    check_app(ctx, func, fidx, at_dyn, *app, c.type_params, 0)?;
                }
                Instr::ListNew { ty, .. }
                | Instr::MapNew { ty, .. }
                | Instr::TupleNew { ty, .. }
                | Instr::IsType(ty)
                | Instr::CastType(ty) => {
                    if *ty as usize >= module.types.len() {
                        return Err(err(fidx, at("type index out of range")));
                    }
                }
                Instr::Jump(target) | Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
                    if *target as usize >= func.blocks.len() {
                        return Err(err(fidx, at("jump target is not a block")));
                    }
                }
                Instr::Perform { op, argc, reply_ty } => {
                    if *op >= lm_abi::OP_COUNT {
                        return Err(err(fidx, at("perform operation slot out of range")));
                    }
                    let want = perform_argc(*op);
                    if *argc != want {
                        return Err(err(fidx, at("perform argument count mismatch")));
                    }
                    if *reply_ty as usize >= module.types.len() {
                        return Err(err(fidx, at("perform reply type index out of range")));
                    }
                }
                Instr::PerformValue { reply_ty, .. } => {
                    if *reply_ty as usize >= module.types.len() {
                        return Err(err(fidx, at("perform reply type index out of range")));
                    }
                }
                Instr::OpConst(op) => {
                    if *op >= lm_abi::OP_COUNT || lm_abi::op(*op).kind != lm_abi::OpKind::Fixed {
                        return Err(err(
                            fidx,
                            at("first-class operation slot is out of range or not fixed"),
                        ));
                    }
                }
                // A typed call token names a fixed host operation, or
                // the receiverless self snapshot. A restored self
                // snapshot holds that request pending, and the
                // restorer answers it through the ordinary typed call
                // path (specification 17.6).
                Instr::AsCall(op) => {
                    let answerable = *op < lm_abi::OP_COUNT
                        && (lm_abi::op(*op).kind == lm_abi::OpKind::Fixed
                            || *op == lm_abi::OP_VM_SNAPSHOT_SELF);
                    if !answerable {
                        return Err(err(
                            fidx,
                            at("as_call operation slot is out of range or not answerable"),
                        ));
                    }
                }
                Instr::TableEdit { action, kind, slot } => {
                    if *action > 3 || *kind > 1 {
                        return Err(err(fidx, at("invalid table edit encoding")));
                    }
                    let bound = if *kind == 0 {
                        lm_abi::OP_COUNT
                    } else {
                        lm_abi::GROUP_COUNT
                    };
                    if *slot >= bound {
                        return Err(err(fidx, at("table edit target out of range")));
                    }
                    if *action == 2
                        && (*kind != 0 || lm_abi::op(*slot).kind != lm_abi::OpKind::Fixed)
                    {
                        return Err(err(
                            fidx,
                            at("a mock target must be an exact fixed operation"),
                        ));
                    }
                }
                _ => {}
            }
        }
    }
    dataflow(ctx, func, fidx)?;
    Ok(())
}

/// Reconstruct the abstract state at every reachable block entry.
///
/// The pass is the type proof of one function body. It also serves the
/// snapshot loader, which reads the operand types of a stopped frame
/// from exactly this state, so the loader and the verifier can never
/// disagree about what a program point holds.
fn dataflow(ctx: &Ctx<'_>, func: &Func, fidx: u32) -> Result<Vec<Option<State>>, VerifyError> {
    let mut states: Vec<Option<State>> = vec![None; func.blocks.len()];
    let mut locals = vec![None; func.local_count() as usize];
    for (i, p) in func.params.iter().enumerate() {
        locals[i] = Some(*p);
    }
    states[0] = Some(State {
        locals,
        stack: Vec::new(),
    });
    let mut worklist = VecDeque::new();
    worklist.push_back(0usize);
    while let Some(bidx) = worklist.pop_front() {
        let mut state = states[bidx].clone().expect("queued block has a state");
        for (iidx, instr) in func.blocks[bidx].iter().enumerate() {
            step(
                ctx,
                func,
                fidx,
                bidx,
                iidx,
                instr,
                &mut state,
                |target, edge_state| {
                    merge(ctx, fidx, target, edge_state, &mut states, &mut worklist)
                },
            )?;
        }
    }
    Ok(states)
}
/// Merge an edge state into a target block. Queue the block again when
/// its entry state changes.
/// Prove that a perform instruction states the reply type its program
/// point proves.
///
/// The world reads `reply_ty` at run time and checks the reply value
/// against it at every boundary crossing. The check is worth nothing
/// unless the stated index is the type the dataflow pushes, so this
/// rule ties the two together. The rule reads the module type table
/// and the dataflow state alone, so no snapshot container takes part.
///
/// The test is equality, never subtyping. The consumer of the reply
/// reads it at exactly the type the dataflow pushed, so a wider stated
/// type would weaken the run-time check.
fn check_reply_ty(
    ctx: &Ctx<'_>,
    state: &State,
    reply_ty: u32,
    fail: &dyn Fn(String) -> VerifyError,
) -> Result<(), VerifyError> {
    let Some(pushed) = state.stack.last().copied() else {
        return Err(fail("a perform pushed no reply".to_string()));
    };
    if reply_ty as usize >= ctx.module.types.len() {
        return Err(fail(format!(
            "the perform states reply type {reply_ty}, which the module has not"
        )));
    }
    // The universe starts with the module type table and interns by
    // content, so equal types take one index.
    if pushed != reply_ty {
        return Err(fail(format!(
            "the perform states reply type {reply_ty} and the program point proves {pushed}"
        )));
    }
    Ok(())
}

fn merge(
    ctx: &Ctx<'_>,
    fidx: u32,
    target: usize,
    edge: State,
    states: &mut [Option<State>],
    worklist: &mut VecDeque<usize>,
) -> Result<(), VerifyError> {
    match &mut states[target] {
        slot @ None => {
            *slot = Some(edge);
            worklist.push_back(target);
        }
        Some(existing) => {
            if existing.stack.len() != edge.stack.len() {
                return Err(err(
                    fidx,
                    format!("block {target} entry stack shapes do not agree"),
                ));
            }
            let mut changed = false;
            for (have, new) in existing.stack.iter_mut().zip(edge.stack.iter()) {
                if *have != *new {
                    let joined = ctx.join(*have, *new).ok_or_else(|| {
                        err(
                            fidx,
                            format!("block {target} entry stack types have no common type"),
                        )
                    })?;
                    if joined != *have {
                        *have = joined;
                        changed = true;
                    }
                }
            }
            for (have, new) in existing.locals.iter_mut().zip(edge.locals.iter()) {
                let merged = match (*have, *new) {
                    (Some(a), Some(b)) => {
                        if a == b {
                            Some(a)
                        } else {
                            ctx.join(a, b)
                        }
                    }
                    _ => None,
                };
                if merged != *have {
                    *have = merged;
                    changed = true;
                }
            }
            if changed {
                worklist.push_back(target);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn step(
    ctx: &Ctx<'_>,
    func: &Func,
    fidx: u32,
    bidx: usize,
    iidx: usize,
    instr: &Instr,
    state: &mut State,
    mut edge: impl FnMut(usize, State) -> Result<(), VerifyError>,
) -> Result<(), VerifyError> {
    let module = ctx.module;
    let fail = |what: String| err(fidx, format!("block {bidx}, instruction {iidx}: {what}"));
    let pop = |state: &mut State| -> Result<u32, VerifyError> {
        state
            .stack
            .pop()
            .ok_or_else(|| fail("pop from an empty stack".to_string()))
    };
    let pop_expect = |state: &mut State, want: u32| -> Result<u32, VerifyError> {
        let ty = pop(state)?;
        if !ctx.is_subtype(ty, want) {
            return Err(fail(format!(
                "expected type {want} on the stack, found type {ty}"
            )));
        }
        Ok(ty)
    };
    let push = |state: &mut State, ty: u32| -> Result<(), VerifyError> {
        if state.stack.len() >= MAX_STATIC_STACK {
            return Err(fail("static stack depth limit exceeded".to_string()));
        }
        state.stack.push(ty);
        Ok(())
    };
    // Pop `count` values that must match `params` in declaration order.
    let pop_args = |state: &mut State, params: &[u32]| -> Result<(), VerifyError> {
        for want in params.iter().rev() {
            pop_expect(state, *want)?;
        }
        Ok(())
    };
    let as_list = |ty: u32| -> Result<u32, VerifyError> {
        match ctx.ty(ty) {
            BcType::List(e) => Ok(e),
            _ => Err(fail(format!("expected a list type, found type {ty}"))),
        }
    };
    let as_map = |ty: u32| -> Result<(u32, u32), VerifyError> {
        match ctx.ty(ty) {
            BcType::Map(k, v) => Ok((k, v)),
            _ => Err(fail(format!("expected a map type, found type {ty}"))),
        }
    };
    // The claimed row of a call must sit inside the caller's row.
    let charge_row = |row: &[BcRow]| -> Result<(), VerifyError> {
        if ctx.row_included(row, &func.row) {
            Ok(())
        } else {
            Err(fail(
                "the callee row is not inside the caller's declared row".to_string(),
            ))
        }
    };
    match instr {
        Instr::ConstUnit => push(state, TY_UNIT)?,
        Instr::ConstBool(_) => push(state, TY_BOOL)?,
        Instr::ConstInt(_) => push(state, TY_INT)?,
        Instr::ConstStr(_) => push(state, TY_STR)?,
        Instr::LoadLocal(slot) => {
            let ty = state.locals[*slot as usize]
                .ok_or_else(|| fail("load from a local without a value".to_string()))?;
            push(state, ty)?;
        }
        Instr::StoreLocal(slot) => {
            // The declared local-type table is the typing judgment:
            // a store must fit the declared slot type, and the slot
            // holds the declared type afterwards. This keeps a
            // widened local at its declared type instead of the
            // concrete stored type.
            let ty = pop(state)?;
            let declared = func.local_types[*slot as usize];
            if !ctx.is_subtype(ty, declared) {
                return Err(fail(format!(
                    "store to local {slot} expects the declared type {declared}, \
                     found type {ty}"
                )));
            }
            state.locals[*slot as usize] = Some(declared);
        }
        Instr::Pop => {
            pop(state)?;
        }
        Instr::Add | Instr::Sub | Instr::Mul | Instr::Div | Instr::Rem => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Neg => {
            pop_expect(state, TY_INT)?;
            push(state, TY_INT)?;
        }
        Instr::Not => {
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::LtInt | Instr::LeInt | Instr::GtInt | Instr::GeInt | Instr::EqInt | Instr::NeInt => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqBool | Instr::NeBool => {
            pop_expect(state, TY_BOOL)?;
            pop_expect(state, TY_BOOL)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::EqStr)
        | Instr::Native(lm_bytecode::NativeInstr::NeStr) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::StrCharCount) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrConcat) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrContains) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextLt
            | lm_bytecode::NativeInstr::TextLe
            | lm_bytecode::NativeInstr::TextGt
            | lm_bytecode::NativeInstr::TextGe,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextAt | lm_bytecode::NativeInstr::TextAtByte) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSlice) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextBytes) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, ctx.intern(BcType::Bytes))?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextTrim
            | lm_bytecode::NativeInstr::TextTrimStart
            | lm_bytecode::NativeInstr::TextTrimEnd,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, value)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextToLowerAscii | lm_bytecode::NativeInstr::TextToUpperAscii,
        ) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextReplace) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            push(state, TY_STR)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::TextParseIntStatus
            | lm_bytecode::NativeInstr::TextParseIntValue,
        ) => {
            pop_expect(state, TY_INT)?;
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextSplit) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            pop_expect(state, text)?;
            let piece = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            let list = ctx.intern(BcType::List(piece));
            push(state, list)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::TextLines) => {
            let text = ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?;
            pop_expect(state, text)?;
            let piece = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            let list = ctx.intern(BcType::List(piece));
            push(state, list)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::BytesEndsWith | lm_bytecode::NativeInstr::BytesContains,
        ) => {
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            pop_expect(state, bytes)?;
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SubstringToString) => {
            let value = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            pop_expect(state, value)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
        | Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len) => {
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            pop_expect(state, value)?;
            push(state, TY_INT)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::EqChar
            | lm_bytecode::NativeInstr::NeChar
            | lm_bytecode::NativeInstr::LtChar
            | lm_bytecode::NativeInstr::LeChar
            | lm_bytecode::NativeInstr::GtChar
            | lm_bytecode::NativeInstr::GeChar,
        ) => {
            let value = ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?;
            pop_expect(state, value)?;
            pop_expect(state, value)?;
            push(state, TY_BOOL)?;
        }
        Instr::EqValue | Instr::NeValue => {
            // Structural equality reads two related enum values. The
            // machine walks the case and the fields, so the verifier
            // proves the operand kind and nothing about the walk.
            let b = pop(state)?;
            let a = pop(state)?;
            let enum_side = |t: u32| {
                ctx.as_instance(t)
                    .map(|(class, _)| ctx.is_enum_class(class))
                    .unwrap_or(false)
            };
            if !enum_side(a) || !enum_side(b) {
                return Err(fail(format!(
                    "structural equality needs two enum values, found {a} and {b}"
                )));
            }
            if !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "structural equality needs related types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::EqRef | Instr::NeRef => {
            let b = pop(state)?;
            let a = pop(state)?;
            let excluded = |t: u32| {
                if matches!(ctx.ty(t), BcType::Str | BcType::Bytes | BcType::Tuple(_)) {
                    return true;
                }
                let Some((class, args)) = ctx.as_instance(t) else {
                    return false;
                };
                args.is_empty()
                    && (ctx
                        .core
                        .text
                        .and_then(|text| ctx.ancestor_args(class, &[], text))
                        .is_some()
                        || ctx.core.char_value == Some(class))
            };
            let heap_ok = ctx.is_heap(a) && ctx.is_heap(b) && !excluded(a) && !excluded(b);
            if !heap_ok || !(ctx.is_subtype(a, b) || ctx.is_subtype(b, a)) {
                return Err(fail(format!(
                    "reference equality needs related object types, found {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Call(callee) => {
            let sig = &module.funcs[*callee as usize];
            charge_row(&sig.row)?;
            pop_args(state, &sig.params)?;
            push(state, sig.ret)?;
        }
        Instr::CallG { func: callee, app } => {
            let sig = &module.funcs[*callee as usize];
            let app = &module.apps[*app as usize];
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &app.types, &app.rows))
                .collect();
            pop_args(state, &params)?;
            let ret = ctx.subst(sig.ret, &app.types, &app.rows);
            push(state, ret)?;
        }
        Instr::CallVirtual { selector, argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let class = match ctx.ty(recv_ty) {
                BcType::Class(c) => c,
                BcType::Int => ctx.core.int.ok_or_else(|| {
                    fail("an Int method call needs the Int core role".to_string())
                })?,
                BcType::Bool => ctx.core.boolean.ok_or_else(|| {
                    fail("a Bool method call needs the Bool core role".to_string())
                })?,
                BcType::Str => ctx.core.string.ok_or_else(|| {
                    fail("a String method call needs the String core role".to_string())
                })?,
                BcType::Bytes => ctx.core.bytes.ok_or_else(|| {
                    fail("a Bytes method call needs the Bytes core role".to_string())
                })?,
                _ => {
                    return Err(fail(format!(
                        "virtual call receiver type {recv_ty} needs the generic form \
                         or is not a class"
                    )));
                }
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            if sig.type_params != 0 || sig.effect_params != 0 {
                return Err(fail(
                    "a generic method call needs a type application".to_string(),
                ));
            }
            charge_row(&sig.row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            pop_args(state, &sig.params[1..])?;
            pop_expect(state, sig.params[0])?;
            push(state, sig.ret)?;
        }
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("virtual call on a short stack".to_string()));
            }
            let recv_ty = state.stack[state.stack.len() - 1 - argc];
            let Some((class, class_args)) = ctx.as_instance(recv_ty) else {
                return Err(fail(format!(
                    "virtual call receiver type {recv_ty} is not a class"
                )));
            };
            let target = ctx
                .find_method(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let sig = &module.funcs[target as usize];
            let app = &module.apps[*app as usize];
            // The declaring class may be a generic ancestor. Its type
            // arguments come from the class table, not from the call
            // site, so no application can forge them.
            let owner = ctx
                .method_owner(class, *selector)
                .ok_or_else(|| fail(format!("selector {selector} is not a class method")))?;
            let mut targs = ctx
                .ancestor_args(class, &class_args, owner)
                .ok_or_else(|| fail("the method owner is not an ancestor".to_string()))?;
            targs.extend_from_slice(&app.types);
            if sig.type_params as usize != targs.len()
                || sig.effect_params as usize != app.rows.len()
            {
                return Err(fail(
                    "virtual call type application arity mismatch".to_string(),
                ));
            }
            let row = ctx.row_subst(&sig.row, &app.rows);
            charge_row(&row)?;
            if sig.params.len() != argc + 1 {
                return Err(fail("virtual call argument count mismatch".to_string()));
            }
            let params: Vec<u32> = sig
                .params
                .iter()
                .map(|p| ctx.subst(*p, &targs, &app.rows))
                .collect();
            pop_args(state, &params[1..])?;
            pop_expect(state, params[0])?;
            let ret = ctx.subst(sig.ret, &targs, &app.rows);
            push(state, ret)?;
        }
        Instr::CallValue { argc } => {
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("closure call on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let (params, ret, row) = match ctx.ty(callee_ty) {
                BcType::Fn(params, _, ret, row) => (params, ret, row),
                _ => {
                    return Err(fail(format!(
                        "closure call target type {callee_ty} is not a function type"
                    )));
                }
            };
            charge_row(&row)?;
            if params.len() != argc {
                return Err(fail("closure call argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
        }
        Instr::MakeClosure { func: f, .. } => {
            let target = &module.funcs[*f as usize];
            pop_args(state, &target.captures)?;
            let fn_ty = BcType::Fn(
                target.params.clone(),
                target.param_muts.clone(),
                target.ret,
                target.row.clone(),
            );
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&fn_ty).copied()
            };
            let idx = idx.filter(|i| (*i as usize) < module.types.len());
            let idx = idx.ok_or_else(|| {
                fail("the closure function type is not in the type table".to_string())
            })?;
            push(state, idx)?;
        }
        Instr::LoadCapture(idx) => {
            let ty = func.captures[*idx as usize];
            push(state, ty)?;
        }
        Instr::New(class) => {
            let native = [
                ctx.core.int,
                ctx.core.boolean,
                ctx.core.string,
                ctx.core.substring,
                ctx.core.char_value,
                ctx.core.bytes,
                ctx.core.string_builder,
                ctx.core.byte_buffer,
            ];
            if native.contains(&Some(*class)) {
                return Err(fail("New cannot allocate a native core class".to_string()));
            }
            let ty = ctx.class_ty[*class as usize]
                .ok_or_else(|| fail("the class type is not in the type table".to_string()))?;
            push(state, ty)?;
        }
        Instr::NewG { class, app } => {
            let native = [
                ctx.core.int,
                ctx.core.boolean,
                ctx.core.string,
                ctx.core.substring,
                ctx.core.char_value,
                ctx.core.bytes,
                ctx.core.string_builder,
                ctx.core.byte_buffer,
            ];
            if native.contains(&Some(*class)) {
                return Err(fail("NewG cannot allocate a native core class".to_string()));
            }
            let app = &module.apps[*app as usize];
            let ty = ctx.intern(BcType::Inst(*class, app.types.clone()));
            push(state, ty)?;
        }
        Instr::LoadField(field) => {
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field load on non-class type {recv}")));
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            push(state, fty)?;
        }
        Instr::StoreField(field) => {
            let value = pop(state)?;
            let recv = pop(state)?;
            let Some((class, class_args)) = ctx.as_instance(recv) else {
                return Err(fail(format!("field store on non-class type {recv}")));
            };
            let fields = &module.classes[class as usize].fields;
            let (_, fty) = fields
                .get(*field as usize)
                .ok_or_else(|| fail("field index out of range".to_string()))?;
            let fty = ctx.subst(*fty, &class_args, &[]);
            if !ctx.is_subtype(value, fty) {
                return Err(fail(format!(
                    "field store expects type {fty}, found type {value}"
                )));
            }
        }
        Instr::TupleNew { ty, count } => {
            let elems = match ctx.ty(*ty) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("expected a tuple type, found type {ty}"))),
            };
            if elems.len() != *count as usize {
                return Err(fail("tuple arity does not match its type".to_string()));
            }
            for want in elems.iter().rev() {
                pop_expect(state, *want)?;
            }
            push(state, *ty)?;
        }
        Instr::TupleGet(index) => {
            let t = pop(state)?;
            let elems = match ctx.ty(t) {
                BcType::Tuple(elems) => elems,
                _ => return Err(fail(format!("tuple read on non-tuple type {t}"))),
            };
            let elem = elems
                .get(*index as usize)
                .ok_or_else(|| fail("tuple index out of range".to_string()))?;
            push(state, *elem)?;
        }
        Instr::IsType(ty) | Instr::CastType(ty) => {
            let value = pop(state)?;
            let Some((vc, va)) = ctx.as_instance(value) else {
                return Err(fail(format!("type test on non-instance type {value}")));
            };
            let Some((tc, ta)) = ctx.as_instance(*ty) else {
                return Err(fail(format!(
                    "type test target {ty} is not an instance type"
                )));
            };
            // Sibling enum cases share their family parent, so a test
            // between them is legal and false at run time. The
            // exhaustiveness backstop emits such tests on flow-narrowed
            // values. Classes without a common ancestor stay rejected.
            if ctx.common_ancestor(vc, tc).is_none() {
                return Err(fail("type test between unrelated classes".to_string()));
            }
            // Class arguments are invariant, and every legal nominal
            // relation in this slice keeps the argument vector. A test
            // that changes an argument would forge a generic type.
            if va != ta {
                return Err(fail("type test changes the generic arguments".to_string()));
            }
            match instr {
                Instr::IsType(_) => push(state, TY_BOOL)?,
                _ => push(state, *ty)?,
            }
        }
        Instr::ListNew { ty, count } => {
            let elem = as_list(*ty)?;
            for _ in 0..*count {
                pop_expect(state, elem)?;
            }
            push(state, *ty)?;
        }
        Instr::ListLen => {
            let l = pop(state)?;
            as_list(l)?;
            push(state, TY_INT)?;
        }
        Instr::ListAt => {
            pop_expect(state, TY_INT)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            push(state, elem)?;
        }
        Instr::ListPush => {
            let value = pop(state)?;
            let l = pop(state)?;
            let elem = as_list(l)?;
            if !ctx.is_subtype(value, elem) {
                return Err(fail(format!(
                    "list push expects element type {elem}, found type {value}"
                )));
            }
            push(state, TY_UNIT)?;
        }
        Instr::MapNew { ty, count } => {
            let (k, v) = as_map(*ty)?;
            for _ in 0..*count {
                pop_expect(state, v)?;
                pop_expect(state, k)?;
            }
            push(state, *ty)?;
        }
        Instr::MapLen => {
            let m = pop(state)?;
            as_map(m)?;
            push(state, TY_INT)?;
        }
        Instr::MapHas => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, _) = as_map(m)?;
            if !ctx.accepts_map_query_key(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, TY_BOOL)?;
        }
        Instr::MapAt => {
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.accepts_map_query_key(key, k) {
                return Err(fail(format!("map key expects type {k}, found type {key}")));
            }
            push(state, v)?;
        }
        Instr::MapPut => {
            let value = pop(state)?;
            let key = pop(state)?;
            let m = pop(state)?;
            let (k, v) = as_map(m)?;
            if !ctx.is_subtype(key, k) || !ctx.is_subtype(value, v) {
                return Err(fail("map put entry types do not match".to_string()));
            }
            push(state, TY_UNIT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbNew) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let idx = ctx.intern(BcType::Class(class));
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
            let want = match instr {
                Instr::Native(lm_bytecode::NativeInstr::SbAppendStr) => {
                    ctx.plain_inst(ctx.core.text, "Text").map_err(&fail)?
                }
                Instr::Native(lm_bytecode::NativeInstr::SbAppendInt) => TY_INT,
                Instr::Native(lm_bytecode::NativeInstr::SbAppendBool) => TY_BOOL,
                Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
                    ctx.plain_inst(ctx.core.char_value, "Char").map_err(&fail)?
                }
                _ => unreachable!("the builder append group is complete"),
            };
            pop_expect(state, want)?;
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            let sb = pop_expect(state, builder)?;
            push(state, sb)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::SbFinish) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbByteLen) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::SbClear) => {
            let class = ctx
                .core
                .string_builder
                .ok_or_else(|| fail("StringBuilder needs its core role".to_string()))?;
            let builder = ctx.intern(BcType::Class(class));
            pop_expect(state, builder)?;
            push(state, builder)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbNew) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let idx = ctx.intern(BcType::Class(class));
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbAppend) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            let bb = pop_expect(state, buffer)?;
            push(state, bb)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbLen) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::BbFinish) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            let bytes = ctx.intern(BcType::Bytes);
            push(state, bytes)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbExtend) => {
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbReserve) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbClear) => {
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, buffer)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbAt) => {
            pop_expect(state, TY_INT)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => {
            pop_expect(state, TY_INT)?;
            let bytes = ctx.intern(BcType::Bytes);
            pop_expect(state, bytes)?;
            let class = ctx
                .core
                .byte_buffer
                .ok_or_else(|| fail("ByteBuffer needs its core role".to_string()))?;
            let buffer = ctx.intern(BcType::Class(class));
            pop_expect(state, buffer)?;
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesNew) => {
            pop_expect(state, TY_STR)?;
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::Bytes).copied()
            };
            let idx = idx.ok_or_else(|| fail("Bytes is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesLen) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("len on non-bytes type {bytes}")));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesText) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("text on non-bytes type {bytes}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesTextView) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("text view on non-bytes type {bytes}")));
            }
            let view = ctx
                .plain_inst(ctx.core.substring, "Substring")
                .map_err(&fail)?;
            push(state, view)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesAt)
        | Instr::Native(lm_bytecode::NativeInstr::BytesGet) => {
            pop_expect(state, TY_INT)?;
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("index on non-bytes type {bytes}")));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesSlice) => {
            pop_expect(state, TY_INT)?;
            pop_expect(state, TY_INT)?;
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("slice on non-bytes type {bytes}")));
            }
            push(state, bytes)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesConcat) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("concat needs two Bytes values".to_string()));
            }
            push(state, left)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("starts_with needs two Bytes values".to_string()));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("find needs two Bytes values".to_string()));
            }
            push(state, TY_INT)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesHex) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("hex on non-bytes type {bytes}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("UTF-8 test on non-bytes type {bytes}")));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(
            lm_bytecode::NativeInstr::EqBytes
            | lm_bytecode::NativeInstr::NeBytes
            | lm_bytecode::NativeInstr::LtBytes
            | lm_bytecode::NativeInstr::LeBytes
            | lm_bytecode::NativeInstr::GtBytes
            | lm_bytecode::NativeInstr::GeBytes,
        ) => {
            let right = pop(state)?;
            let left = pop(state)?;
            if ctx.ty(left) != BcType::Bytes || ctx.ty(right) != BcType::Bytes {
                return Err(fail("Bytes comparison needs two Bytes values".to_string()));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Native(lm_bytecode::NativeInstr::BytesCompact) => {
            let bytes = pop(state)?;
            if ctx.ty(bytes) != BcType::Bytes {
                return Err(fail(format!("compact on non-bytes type {bytes}")));
            }
            push(state, bytes)?;
        }
        Instr::Freeze => {
            let ty = pop(state)?;
            if !ctx.is_heap(ty) {
                return Err(fail(format!("freeze on non-object type {ty}")));
            }
            push(state, ty)?;
        }
        Instr::Digest => {
            let ty = pop(state)?;
            if !ctx.is_heap(ty) {
                return Err(fail(format!("digest on non-object type {ty}")));
            }
            let idx = {
                let uni = ctx.uni.borrow();
                uni.index.get(&BcType::Digest).copied()
            };
            let idx = idx.ok_or_else(|| fail("Digest is not in the type table".to_string()))?;
            push(state, idx)?;
        }
        Instr::EqDigest | Instr::NeDigest => {
            let b = pop(state)?;
            let a = pop(state)?;
            if ctx.ty(a) != BcType::Digest || ctx.ty(b) != BcType::Digest {
                return Err(fail(format!(
                    "digest comparison on non-digest types {a} and {b}"
                )));
            }
            push(state, TY_BOOL)?;
        }
        Instr::Jump(target) => {
            edge(*target as usize, state.clone())?;
        }
        Instr::JumpIfFalse(target) | Instr::JumpIfTrue(target) => {
            pop_expect(state, TY_BOOL)?;
            edge(*target as usize, state.clone())?;
        }
        Instr::Return => {
            pop_expect(state, func.ret)?;
        }
        Instr::Perform { op, reply_ty, .. } => {
            let reply_ty = *reply_ty;
            let op = *op;
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let def = lm_abi::op(op);
            match def.kind {
                lm_abi::OpKind::Fixed => {
                    for want in def.params.iter().rev() {
                        let want = ctx.abi_ty(*want).map_err(&fail)?;
                        pop_expect(state, want)?;
                    }
                    let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
                    push(state, reply)?;
                }
                lm_abi::OpKind::VmControl => {
                    let pop_vm = |state: &mut State| -> Result<u32, VerifyError> {
                        let v = pop(state)?;
                        match ctx.ty(v) {
                            BcType::Vm(t) => Ok(t),
                            _ => Err(fail(format!(
                                "`{name}` needs a loaded Vm receiver, found type {v}"
                            ))),
                        }
                    };
                    match op {
                        lm_abi::OP_VM_NEW => {
                            let empty = ctx.intern(BcType::EmptyVm);
                            push(state, empty)?;
                        }
                        lm_abi::OP_VM_FROM_FN => {
                            let args_ty = pop(state)?;
                            let fn_ty = pop(state)?;
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::EmptyVm {
                                return Err(fail(
                                    "`Vm.FromFn` needs an EmptyVm receiver".to_string(),
                                ));
                            }
                            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                                return Err(fail("`Vm.FromFn` needs a function value".to_string()));
                            };
                            let want = if params.is_empty() {
                                TY_UNIT
                            } else {
                                ctx.intern(BcType::Tuple(params))
                            };
                            if !ctx.is_subtype(args_ty, want) {
                                return Err(fail(
                                    "`Vm.FromFn` arguments do not match the \
                                     program parameters"
                                        .to_string(),
                                ));
                            }
                            let vm = ctx.intern(BcType::Vm(ret));
                            push(state, vm)?;
                        }
                        lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE => {
                            let t = pop_vm(state)?;
                            let (parent, what) = match op {
                                lm_abi::OP_VM_RUN => (ctx.core.run_result, "RunResult"),
                                lm_abi::OP_VM_STEP => (ctx.core.step_event, "StepEvent"),
                                _ => (ctx.core.drive_event, "DriveEvent"),
                            };
                            let event = ctx.event_inst(parent, what, t).map_err(&fail)?;
                            push(state, event)?;
                        }
                        lm_abi::OP_VM_DRIVE_WAIT => {
                            let t = pop_vm(state)?;
                            let event = ctx
                                .event_inst(ctx.core.drive_event, "DriveEvent", t)
                                .map_err(&fail)?;
                            let wait = ctx.intern(BcType::Wait(event));
                            push(state, wait)?;
                        }
                        lm_abi::OP_VM_TABLE => {
                            pop_vm(state)?;
                            let table = ctx.intern(BcType::PolicyTable);
                            push(state, table)?;
                        }
                        lm_abi::OP_VM_HANDLES => {
                            pop_vm(state)?;
                            let control = ctx.intern(BcType::ResourceHandle);
                            let list = ctx.intern(BcType::List(control));
                            push(state, list)?;
                        }
                        lm_abi::OP_VM_RESOURCE => {
                            let handle = pop(state)?;
                            pop_vm(state)?;
                            let tcp = ctx
                                .core
                                .tcp_resource
                                .map(|class| ctx.intern(BcType::Class(class)));
                            let tls = ctx
                                .core
                                .tls_stream
                                .map(|class| ctx.intern(BcType::Class(class)));
                            if ctx.ty(handle) != BcType::FileHandle
                                && tcp.is_none_or(|tcp| !ctx.is_subtype(handle, tcp))
                                && tls.is_none_or(|tls| handle != tls)
                            {
                                return Err(fail(
                                    "`Vm.Resource` needs a file or stream resource".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_FILE => {
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_FS_OPEN).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_FS_OPEN).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeFile` needs an Fs.Open call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TCP_STREAM => {
                            let peer = pop(state)?;
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let address =
                                ctx.abi_ty(lm_abi::AbiType::SOCKET_ADDRESS).map_err(&fail)?;
                            if !ctx.is_subtype(peer, address) {
                                return Err(fail(
                                    "`Vm.ServeTcpStream` needs a SocketAddress".to_string(),
                                ));
                            }
                            let connect_args =
                                ctx.op_args_view(lm_abi::OP_TCP_CONNECT).map_err(&fail)?;
                            let connect_reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_CONNECT).reply)
                                .map_err(&fail)?;
                            let accept_args =
                                ctx.op_args_view(lm_abi::OP_TCP_ACCEPT).map_err(&fail)?;
                            let accept_reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_ACCEPT).reply)
                                .map_err(&fail)?;
                            let valid = ctx.ty(call)
                                == BcType::PendingCall(connect_args, connect_reply)
                                || ctx.ty(call) == BcType::PendingCall(accept_args, accept_reply);
                            if !valid {
                                return Err(fail(
                                    "`Vm.ServeTcpStream` needs a Tcp.Connect or Tcp.Accept call"
                                        .to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TCP_LISTENER => {
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_TCP_LISTEN).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TCP_LISTEN).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeTcpListener` needs a Tcp.Listen call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_SERVE_TLS_STREAM => {
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let args = ctx.op_args_view(lm_abi::OP_TLS_HANDSHAKE).map_err(&fail)?;
                            let reply = ctx
                                .abi_ty(lm_abi::op(lm_abi::OP_TLS_HANDSHAKE).reply)
                                .map_err(&fail)?;
                            if ctx.ty(call) != BcType::PendingCall(args, reply) {
                                return Err(fail(
                                    "`Vm.ServeTlsStream` needs a Tls.Handshake call".to_string(),
                                ));
                            }
                            let control = ctx.intern(BcType::ResourceHandle);
                            push(state, control)?;
                        }
                        lm_abi::OP_VM_RESOURCE_IS_OPEN | lm_abi::OP_VM_RESOURCE_CLOSE => {
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle {
                                return Err(fail(format!("`{name}` needs a ResourceHandle")));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_RESOURCE_KIND => {
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle {
                                return Err(fail(
                                    "`Vm.ResourceKind` needs a ResourceHandle".to_string(),
                                ));
                            }
                            push(state, TY_STR)?;
                        }
                        lm_abi::OP_VM_RESOURCE_SAME => {
                            let other = pop(state)?;
                            let control = pop(state)?;
                            if ctx.ty(control) != BcType::ResourceHandle
                                || ctx.ty(other) != BcType::ResourceHandle
                            {
                                return Err(fail(
                                    "`Vm.ResourceSame` needs two ResourceHandle values".to_string(),
                                ));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_ANSWER => {
                            let value = pop(state)?;
                            let call = pop(state)?;
                            pop_vm(state)?;
                            let BcType::PendingCall(_, reply) = ctx.ty(call) else {
                                return Err(fail(
                                    "`Vm.Answer` needs a PendingCall token".to_string(),
                                ));
                            };
                            if !ctx.is_subtype(value, reply) {
                                return Err(fail(format!(
                                    "`Vm.Answer` reply expects type {reply}, found \
                                     type {value}"
                                )));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_REJECT => {
                            let fault = pop(state)?;
                            let request = pop(state)?;
                            pop_vm(state)?;
                            if ctx.ty(fault) != BcType::Fault || ctx.ty(request) != BcType::Request
                            {
                                return Err(fail(
                                    "`Vm.Reject` needs a Request and a Fault".to_string(),
                                ));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_VM_DISPATCH => {
                            let request = pop(state)?;
                            pop_vm(state)?;
                            if ctx.ty(request) != BcType::Request {
                                return Err(fail("`Vm.Dispatch` needs a Request".to_string()));
                            }
                            push(state, TY_UNIT)?;
                        }
                        lm_abi::OP_PROC_RUN => {
                            let t = pop_vm(state)?;
                            // The mailbox-bearing launch is
                            // `Proc.Spawn`. This form takes no message,
                            // so `M` is the bottom type, which the
                            // bytecode encodes as `()`.
                            let handle = ctx.intern(BcType::Handle(TY_UNIT, t));
                            push(state, handle)?;
                        }
                        lm_abi::OP_PROC_SPAWN => {
                            let args_ty = pop(state)?;
                            let body = pop(state)?;
                            let ctor = pop(state)?;
                            let BcType::Fn(ctor_params, _, proc_ty, _) = ctx.ty(ctor) else {
                                return Err(fail(
                                    "`Proc.Spawn` needs a constructor function".to_string(),
                                ));
                            };
                            // The mailbox type comes from the class
                            // table, through the proc class the
                            // constructor builds. No call site can
                            // claim another one.
                            let mailbox = ctx.proc_mailbox(proc_ty).ok_or_else(|| {
                                fail(
                                    "`Proc.Spawn` needs a constructor of a `Proc` subclass"
                                        .to_string(),
                                )
                            })?;
                            let BcType::Fn(body_params, _, result, _) = ctx.ty(body) else {
                                return Err(fail("`Proc.Spawn` needs a body function".to_string()));
                            };
                            // The body may come from an ancestor of the
                            // proc class, so the constructed instance
                            // must satisfy its receiver, not equal it.
                            if body_params.len() != 1 || !ctx.is_subtype(proc_ty, body_params[0]) {
                                return Err(fail(
                                    "`Proc.Spawn` body does not take the constructed proc"
                                        .to_string(),
                                ));
                            }
                            let want = if ctor_params.is_empty() {
                                TY_UNIT
                            } else {
                                ctx.intern(BcType::Tuple(ctor_params))
                            };
                            if !ctx.is_subtype(args_ty, want) {
                                return Err(fail(
                                    "`Proc.Spawn` arguments do not match the constructor \
                                     parameters"
                                        .to_string(),
                                ));
                            }
                            let handle = ctx.intern(BcType::Handle(mailbox, result));
                            push(state, handle)?;
                        }
                        lm_abi::OP_PROC_SEND => {
                            let message = pop(state)?;
                            let handle = pop(state)?;
                            let BcType::Handle(mailbox, _) = ctx.ty(handle) else {
                                return Err(fail("`Proc.Send` needs a proc handle".to_string()));
                            };
                            if !ctx.is_subtype(message, mailbox) {
                                return Err(fail(format!(
                                    "`Proc.Send` expects a message of type {mailbox}, \
                                     found type {message}"
                                )));
                            }
                            let result = ctx
                                .plain_inst(ctx.core.send_result, "SendResult")
                                .map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_PROC_CLOSE => {
                            let handle = pop(state)?;
                            if !matches!(ctx.ty(handle), BcType::Handle(_, _)) {
                                return Err(fail("`Proc.Close` needs a proc handle".to_string()));
                            }
                            let result = ctx
                                .plain_inst(ctx.core.send_result, "SendResult")
                                .map_err(&fail)?;
                            push(state, result)?;
                        }
                        lm_abi::OP_PROC_DONE => {
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail("`Proc.Done` needs a proc handle".to_string()));
                            };
                            let event = ctx
                                .event_inst(ctx.core.proc_result, "ProcResult", result)
                                .map_err(&fail)?;
                            push(state, event)?;
                        }
                        lm_abi::OP_PROC_PAUSE | lm_abi::OP_PROC_RESUME => {
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail(format!("`{name}` needs a proc handle")));
                            };
                            let ok = if op == lm_abi::OP_PROC_PAUSE {
                                ctx.intern(BcType::Vm(result))
                            } else {
                                TY_UNIT
                            };
                            let error = ctx
                                .plain_inst(ctx.core.proc_error, "ProcError")
                                .map_err(&fail)?;
                            let Some(result_family) = ctx.core.result else {
                                return Err(fail(
                                    "the module does not carry the pinned core Result \
                                     definition"
                                        .to_string(),
                                ));
                            };
                            let out = ctx.intern(BcType::Inst(result_family, vec![ok, error]));
                            push(state, out)?;
                        }
                        lm_abi::OP_PROC_RECV | lm_abi::OP_PROC_RECV_WAIT => {
                            // The receiver is the performing proc. Its
                            // class fixes the mailbox type, so the
                            // rule reads the class table.
                            let recv = pop(state)?;
                            let mailbox = ctx.proc_mailbox(recv).ok_or_else(|| {
                                fail("`Proc.Recv` needs a `Proc` subclass receiver".to_string())
                            })?;
                            let event = ctx
                                .event_inst(ctx.core.recv, "Recv", mailbox)
                                .map_err(&fail)?;
                            if op == lm_abi::OP_PROC_RECV {
                                push(state, event)?;
                            } else {
                                let wait = ctx.intern(BcType::Wait(event));
                                push(state, wait)?;
                            }
                        }
                        lm_abi::OP_WAIT_WAIT => {
                            let wait = pop(state)?;
                            let BcType::Wait(result) = ctx.ty(wait) else {
                                return Err(fail("`Wait.Wait` needs a Wait value".to_string()));
                            };
                            push(state, result)?;
                        }
                        lm_abi::OP_WAIT_CHOOSE => {
                            let right = pop(state)?;
                            let left = pop(state)?;
                            let BcType::Wait(right) = ctx.ty(right) else {
                                return Err(fail(
                                    "`Wait.Choose` needs two Wait values".to_string(),
                                ));
                            };
                            let BcType::Wait(left) = ctx.ty(left) else {
                                return Err(fail(
                                    "`Wait.Choose` needs two Wait values".to_string(),
                                ));
                            };
                            let Some(choice) = ctx.core.choice else {
                                return Err(fail(
                                    "the module does not carry the pinned core Choice definition"
                                        .to_string(),
                                ));
                            };
                            let choice = ctx.intern(BcType::Inst(choice, vec![left, right]));
                            let wait = ctx.intern(BcType::Wait(choice));
                            push(state, wait)?;
                        }
                        lm_abi::OP_WAIT_CANCEL => {
                            let wait = pop(state)?;
                            if !matches!(ctx.ty(wait), BcType::Wait(_)) {
                                return Err(fail("`Wait.Cancel` needs a Wait value".to_string()));
                            }
                            push(state, TY_BOOL)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_HELD => {
                            let t = pop_vm(state)?;
                            let snapshot = ctx.intern(BcType::Snapshot(t));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_DRIVE_FOR => {
                            let count = pop(state)?;
                            if ctx.ty(count) != BcType::Int {
                                return Err(fail(
                                    "`Vm.DriveFor` needs an instruction count".to_string(),
                                ));
                            }
                            let t = pop_vm(state)?;
                            let event = ctx
                                .event_inst(ctx.core.drive_event, "DriveEvent", t)
                                .map_err(&fail)?;
                            let out = ctx
                                .event_inst(ctx.core.option, "Option", event)
                                .map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_WAIT_HELD => {
                            let fuel = pop(state)?;
                            if ctx.ty(fuel) != BcType::Int {
                                return Err(fail(
                                    "`Vm.SnapshotWaitHeld` needs a fuel count".to_string(),
                                ));
                            }
                            let t = pop_vm(state)?;
                            let snapshot = ctx.intern(BcType::Snapshot(t));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_PROC_SNAPSHOT_WAIT => {
                            pop_expect(state, TY_INT)?;
                            let handle = pop(state)?;
                            let BcType::Handle(_, result) = ctx.ty(handle) else {
                                return Err(fail(
                                    "`Proc.SnapshotWait` needs a proc handle".to_string(),
                                ));
                            };
                            let snapshot = ctx.intern(BcType::Snapshot(result));
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(snapshot, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_SNAPSHOT_SELF => {
                            let image = ctx.intern(BcType::SnapshotImage);
                            let error = ctx
                                .plain_inst(ctx.core.snapshot_error, "SnapshotError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(image, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_RESTORE => {
                            let snapshot = pop(state)?;
                            let recv = pop(state)?;
                            if ctx.ty(recv) != BcType::EmptyVm {
                                return Err(fail(
                                    "`Vm.Restore` needs an EmptyVm receiver".to_string(),
                                ));
                            }
                            let BcType::Snapshot(t) = ctx.ty(snapshot) else {
                                return Err(fail(
                                    "`Vm.Restore` needs a typed snapshot".to_string(),
                                ));
                            };
                            let vm = ctx.intern(BcType::Vm(t));
                            let error = ctx
                                .plain_inst(ctx.core.restore_error, "RestoreError")
                                .map_err(&fail)?;
                            let out = ctx.result_inst(vm, error).map_err(&fail)?;
                            push(state, out)?;
                        }
                        lm_abi::OP_VM_LOAD_SNAPSHOT => {
                            // This build has no guest snapshot decoder.
                            return Err(fail(
                                "`Vm.LoadSnapshot` is not available in this build".to_string(),
                            ));
                        }
                        _ => unreachable!("every VmControl slot has a rule"),
                    }
                }
            }
            check_reply_ty(ctx, state, reply_ty, &fail)?;
        }
        Instr::PerformValue { argc, reply_ty } => {
            let reply_ty = *reply_ty;
            let argc = *argc as usize;
            if state.stack.len() < argc + 1 {
                return Err(fail("perform through a value on a short stack".to_string()));
            }
            let callee_ty = state.stack[state.stack.len() - 1 - argc];
            let BcType::Op(op, fn_ty) = ctx.ty(callee_ty) else {
                return Err(fail(format!(
                    "perform target type {callee_ty} is not an operation value"
                )));
            };
            let name = lm_abi::op_name(op);
            if !ctx.row_has_name(&func.row, &name) {
                return Err(fail(format!(
                    "the perform of `{name}` is not inside the claimed row"
                )));
            }
            let BcType::Fn(params, _, ret, _) = ctx.ty(fn_ty) else {
                unreachable!("a verified Op type embeds a function type");
            };
            if params.len() != argc {
                return Err(fail("perform argument count mismatch".to_string()));
            }
            pop_args(state, &params)?;
            pop(state)?;
            push(state, ret)?;
            check_reply_ty(ctx, state, reply_ty, &fail)?;
        }
        Instr::OpConst(op) => {
            let sig = ctx.fixed_sig_type(*op).map_err(&fail)?;
            let ty = ctx.intern(BcType::Op(*op, sig));
            push(state, ty)?;
        }
        Instr::TableEdit { action, kind, slot } => {
            if *action == 2 {
                let handler = pop(state)?;
                let want = ctx.fixed_sig_type(*slot).map_err(&fail)?;
                if !ctx.is_subtype(handler, want) {
                    return Err(fail(format!(
                        "a mock handler must have the exact operation signature \
                         with an empty row, found type {handler}"
                    )));
                }
            }
            let table = pop(state)?;
            if ctx.ty(table) != BcType::PolicyTable {
                return Err(fail(format!("table edit on non-table type {table}")));
            }
            if *action == 0 {
                // The dependent grant rule: `pass` is charged to the
                // granter's claimed row.
                let name = if *kind == 0 {
                    lm_abi::op_name(*slot)
                } else {
                    lm_abi::GROUPS[*slot as usize].to_string()
                };
                if !ctx.row_has_name(&func.row, &name) {
                    return Err(fail(format!(
                        "the pass of `{name}` is not inside the claimed row"
                    )));
                }
            }
            push(state, TY_UNIT)?;
        }
        Instr::AsCall(op) => {
            let request = pop(state)?;
            if ctx.ty(request) != BcType::Request {
                return Err(fail(format!("as_call on non-request type {request}")));
            }
            let view = ctx.op_args_view(*op).map_err(&fail)?;
            let def = lm_abi::op(*op);
            let reply = ctx.abi_ty(def.reply).map_err(&fail)?;
            let call = ctx.intern(BcType::PendingCall(view, reply));
            let out = ctx
                .event_inst(ctx.core.option, "Option", call)
                .map_err(&fail)?;
            push(state, out)?;
        }
        Instr::CallArgs => {
            let call = pop(state)?;
            let BcType::PendingCall(view, _) = ctx.ty(call) else {
                return Err(fail(format!("args view on non-call type {call}")));
            };
            push(state, view)?;
        }
        Instr::FaultCode => {
            let fault = pop(state)?;
            if ctx.ty(fault) != BcType::Fault {
                return Err(fail(format!("fault code on non-fault type {fault}")));
            }
            push(state, TY_STR)?;
        }
        Instr::FaultDenied => {
            pop_expect(state, TY_STR)?;
            let fault = ctx.intern(BcType::Fault);
            push(state, fault)?;
        }
        Instr::RequestOp => {
            let request = pop(state)?;
            if ctx.ty(request) != BcType::Request {
                return Err(fail(format!("request op on non-request type {request}")));
            }
            push(state, TY_STR)?;
        }
        Instr::Unreachable => {
            // A diverging terminator: no stack effect, no successor.
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClass, Func, Instr::*, Module, TypeApp, NO_PARENT};

    fn base_types() -> Vec<BcType> {
        vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str]
    }

    fn plain_func(name: &str, params: Vec<u32>, ret: u32, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
            type_params: 0,
            effect_params: 0,
            param_muts: vec![false; params.len()],
            local_types: {
                let mut locals = params.clone();
                locals.resize(2, TY_INT);
                locals
            },
            params,
            ret,
            row: vec![],
            captures: vec![],
            blocks,
        }
    }

    fn module_with(blocks: Vec<Vec<Instr>>) -> Module {
        Module {
            strings: vec!["s".to_string()],
            types: base_types(),
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![plain_func("main", vec![], TY_INT, blocks)],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    /// A module with one class `Counter { value: Int }` and one method
    /// `bump(self): Int` on selector 0, plus an entry function.
    fn class_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Class(0)); // type 4
        Module {
            strings: vec![],
            types,
            selectors: vec!["bump".to_string()],
            apps: vec![],
            classes: vec![BcClass {
                name: "Counter".to_string(),
                parent_args: Vec::new(),
                key: "Counter".to_string(),
                is_final: false,
                parent: NO_PARENT,
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), TY_INT)],
                methods: vec![(0, 1)],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                plain_func(
                    "bump",
                    vec![4],
                    TY_INT,
                    vec![vec![LoadLocal(0), LoadField(0), Return]],
                ),
            ],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    /// A generic module: `Box[T] { value: T }`, its `<new>` function,
    /// and an entry that builds `Box[Int]` and reads the field.
    fn generic_module(entry_blocks: Vec<Vec<Instr>>) -> Module {
        let mut types = base_types();
        types.push(BcType::Var(0)); // 4
        types.push(BcType::Inst(0, vec![4])); // 5 Box[$0]
        types.push(BcType::Inst(0, vec![TY_INT])); // 6 Box[Int]
        Module {
            strings: vec![],
            types,
            selectors: vec![],
            apps: vec![
                TypeApp {
                    types: vec![TY_INT],
                    rows: vec![],
                },
                TypeApp {
                    types: vec![4],
                    rows: vec![],
                },
            ],
            classes: vec![BcClass {
                name: "Box".to_string(),
                parent_args: Vec::new(),
                key: "Box".to_string(),
                is_final: false,
                parent: NO_PARENT,
                type_params: 1,
                kind: BcClassKind::Normal,
                fields: vec![("value".to_string(), 4)],
                methods: vec![],
            }],
            funcs: vec![
                plain_func("main", vec![], TY_INT, entry_blocks),
                Func {
                    name: "<new Box>".to_string(),
                    type_params: 1,
                    effect_params: 0,
                    params: vec![4],
                    param_muts: vec![false],
                    ret: 5,
                    row: vec![],
                    captures: vec![],
                    local_types: vec![4, 5],
                    blocks: vec![vec![
                        NewG { class: 0, app: 1 },
                        StoreLocal(1),
                        LoadLocal(1),
                        LoadLocal(0),
                        StoreField(0),
                        LoadLocal(1),
                        Return,
                    ]],
                },
            ],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    #[test]
    fn accepts_simple_function() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), Add, Return]]);
        assert!(verify_module(&m).is_ok());
    }

    #[test]
    fn accepts_class_construction_and_virtual_call() {
        let mut m = class_module(vec![vec![
            New(0),
            StoreLocal(0),
            LoadLocal(0),
            ConstInt(7),
            StoreField(0),
            LoadLocal(0),
            CallVirtual {
                selector: 0,
                argc: 0,
            },
            Return,
        ]]);
        // The entry stores a Counter into local 0, so the declared
        // slot type must accept it.
        m.funcs[0].local_types = vec![4, TY_INT];
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn accepts_generic_construction_and_field_read() {
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_generic_call_with_wrong_result_use() {
        // Box[Int].value is Int; using it as Bool must fail.
        let m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Not,
            Pop,
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_generic_call_without_application() {
        let m = generic_module(vec![vec![ConstInt(41), Call(1), LoadField(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type application"), "{e}");
    }

    #[test]
    fn rejects_application_arity_mismatch() {
        let mut m = generic_module(vec![vec![
            ConstInt(41),
            CallG { func: 1, app: 0 },
            LoadField(0),
            Return,
        ]]);
        m.apps[0].types.push(TY_INT);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_bad_jump_target() {
        let m = module_with(vec![vec![Jump(7)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("jump target"), "{e}");
    }

    #[test]
    fn rejects_wrong_stack_shape() {
        let m = module_with(vec![vec![ConstInt(1), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("empty stack"), "{e}");
    }

    #[test]
    fn rejects_type_confusion() {
        let m = module_with(vec![vec![ConstBool(true), ConstInt(2), Add, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_missing_terminator() {
        let m = module_with(vec![vec![ConstInt(1)]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("terminator"), "{e}");
    }

    #[test]
    fn rejects_load_before_store() {
        let m = module_with(vec![vec![LoadLocal(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("without a value"), "{e}");
    }

    #[test]
    fn rejects_missing_primitive_prefix() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types = vec![BcType::Int, BcType::Unit, BcType::Bool, BcType::Str];
        m.funcs[0].ret = 0;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("does not start with"), "{e}");
    }

    #[test]
    fn rejects_duplicate_type_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Int);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("duplicates"), "{e}");
    }

    #[test]
    fn rejects_forward_type_reference() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(9));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("earlier entry"), "{e}");
    }

    #[test]
    fn rejects_invalid_map_key_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::List(TY_INT)); // 4
        m.types.push(BcType::Map(4, TY_INT)); // key is a list
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("key type"), "{e}");
    }

    #[test]
    fn rejects_overlong_tuple_type() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.types.push(BcType::Tuple(vec![TY_INT; 17]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_tuple_get_out_of_range() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            ConstInt(2),
            TupleNew { ty: 4, count: 2 },
            TupleGet(5),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("tuple index"), "{e}");
    }

    #[test]
    fn rejects_tuple_new_count_mismatch() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            TupleNew { ty: 4, count: 1 },
            TupleGet(0),
            Return,
        ]]);
        m.types.push(BcType::Tuple(vec![TY_INT, TY_INT]));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("arity"), "{e}");
    }

    #[test]
    fn rejects_new_on_abstract_class() {
        let mut m = class_module(vec![vec![New(1), Pop, ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            parent_args: Vec::new(),
            key: "Opt".to_string(),
            is_final: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("abstract"), "{e}");
    }

    #[test]
    fn rejects_subclass_of_case_class() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.classes.push(BcClass {
            name: "Opt".to_string(),
            parent_args: Vec::new(),
            key: "Opt".to_string(),
            is_final: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Abstract,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Opt.None".to_string(),
            parent_args: Vec::new(),
            key: "Opt.None".to_string(),
            is_final: false,
            parent: 1,
            type_params: 0,
            kind: BcClassKind::Case,
            fields: vec![],
            methods: vec![],
        });
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            parent_args: Vec::new(),
            key: "Bad".to_string(),
            is_final: false,
            parent: 2,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("case"), "{e}");
    }

    #[test]
    fn rejects_type_test_between_unrelated_classes() {
        let mut m = class_module(vec![vec![New(0), IsType(5), Pop, ConstInt(0), Return]]);
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Other".to_string(),
            parent_args: Vec::new(),
            key: "Other".to_string(),
            is_final: false,
            parent: NO_PARENT,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("unrelated"), "{e}");
    }

    #[test]
    fn rejects_row_not_inside_caller() {
        // Callee claims Io.Print; caller declares the empty row.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
    }

    #[test]
    fn accepts_row_inside_caller_with_group() {
        // Caller declares Io; callee claims Io.Print.
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.strings = vec!["Io.Print".to_string(), "Io".to_string()];
        m.funcs[0].row = vec![BcRow::Op(1)];
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_non_canonical_declared_row() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.strings = vec!["Io".to_string(), "Fs".to_string()];
        m.funcs[0].row = vec![BcRow::Op(0), BcRow::Op(1)]; // Io before Fs
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("canonical"), "{e}");
    }

    #[test]
    fn rejects_row_var_outside_arity() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].row = vec![BcRow::Var(0)];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("effect variable"), "{e}");
    }

    #[test]
    fn rejects_field_index_out_of_range() {
        let m = class_module(vec![vec![New(0), LoadField(9), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field index"), "{e}");
    }

    #[test]
    fn rejects_wrong_field_store_type() {
        let m = class_module(vec![vec![
            New(0),
            ConstBool(true),
            StoreField(0),
            ConstInt(0),
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("field store"), "{e}");
    }

    #[test]
    fn rejects_unknown_selector_on_class() {
        let mut m = class_module(vec![vec![
            New(0),
            CallVirtual {
                selector: 1,
                argc: 0,
            },
            Return,
        ]]);
        m.selectors.push("other".to_string());
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a class method"), "{e}");
    }

    #[test]
    fn rejects_virtual_argc_mismatch() {
        let m = class_module(vec![vec![
            New(0),
            ConstInt(1),
            CallVirtual {
                selector: 0,
                argc: 1,
            },
            Return,
        ]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("argument count"), "{e}");
    }

    #[test]
    fn rejects_method_with_wrong_self_type() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.funcs[1].params = vec![TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("`self`"), "{e}");
    }

    #[test]
    fn rejects_override_that_changes_parameters() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        // A subclass whose bump takes an extra Int.
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Fast".to_string(),
            parent_args: Vec::new(),
            key: "Fast".to_string(),
            is_final: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(plain_func(
            "bump2",
            vec![5, TY_INT],
            TY_INT,
            vec![vec![ConstInt(1), Return]],
        ));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parameter types"), "{e}");
    }

    #[test]
    fn rejects_override_that_widens_the_row() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types.push(BcType::Class(1)); // 5
        m.classes.push(BcClass {
            name: "Loud".to_string(),
            parent_args: Vec::new(),
            key: "Loud".to_string(),
            is_final: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("value".to_string(), TY_INT)],
            methods: vec![(0, 2)],
        });
        m.funcs.push(Func {
            name: "bump2".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![5],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![5],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("widens the effect row"), "{e}");
    }

    #[test]
    fn rejects_layout_that_breaks_parent_prefix() {
        let mut m = class_module(vec![vec![ConstInt(0), Return]]);
        m.types.push(BcType::Class(1));
        m.classes.push(BcClass {
            name: "Bad".to_string(),
            parent_args: Vec::new(),
            key: "Bad".to_string(),
            is_final: false,
            parent: 0,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![("other".to_string(), TY_BOOL)],
            methods: vec![],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("parent layout"), "{e}");
    }

    #[test]
    fn rejects_direct_call_to_captured_function() {
        let mut m = module_with(vec![vec![Call(1), Return]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("captures"), "{e}");
    }

    #[test]
    fn rejects_closure_without_fn_type_entry() {
        let mut m = module_with(vec![vec![
            ConstInt(1),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![],
            blocks: vec![vec![LoadCapture(0), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("type table"), "{e}");
    }

    #[test]
    fn accepts_closure_create_and_call() {
        let mut m = module_with(vec![vec![
            ConstInt(41),
            MakeClosure {
                func: 1,
                captures: 1,
            },
            ConstInt(1),
            CallValue { argc: 1 },
            Return,
        ]]);
        m.types
            .push(BcType::Fn(vec![TY_INT], vec![false], TY_INT, vec![]));
        m.funcs.push(Func {
            name: "closure".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![TY_INT],
            param_muts: vec![false],
            ret: TY_INT,
            row: vec![],
            captures: vec![TY_INT],
            local_types: vec![TY_INT],
            blocks: vec![vec![LoadCapture(0), LoadLocal(0), Add, Return]],
        });
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    #[test]
    fn rejects_call_value_row_outside_caller() {
        let mut m = module_with(vec![vec![
            MakeClosure {
                func: 1,
                captures: 0,
            },
            CallValue { argc: 0 },
            Return,
        ]]);
        m.strings = vec!["Io.Print".to_string()];
        m.types
            .push(BcType::Fn(vec![], vec![], TY_INT, vec![BcRow::Op(0)]));
        m.funcs.push(Func {
            name: "printer".to_string(),
            type_params: 0,
            effect_params: 0,
            params: vec![],
            param_muts: vec![],
            ret: TY_INT,
            row: vec![BcRow::Op(0)],
            captures: vec![],
            local_types: vec![],
            blocks: vec![vec![ConstInt(1), Return]],
        });
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("row"), "{e}");
    }

    #[test]
    fn rejects_call_value_on_non_function() {
        let m = module_with(vec![vec![ConstInt(1), CallValue { argc: 0 }, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("not a function type"), "{e}");
    }

    #[test]
    fn rejects_capture_index_out_of_range() {
        let m = module_with(vec![vec![LoadCapture(0), Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("capture index"), "{e}");
    }

    #[test]
    fn rejects_list_element_type_mismatch() {
        let mut m = module_with(vec![vec![
            ConstBool(true),
            ListNew { ty: 4, count: 1 },
            Pop,
            ConstInt(0),
            Return,
        ]]);
        m.types.push(BcType::List(TY_INT));
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("expected type"), "{e}");
    }

    #[test]
    fn rejects_freeze_on_scalar() {
        let m = module_with(vec![vec![ConstInt(1), Freeze, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("freeze"), "{e}");
    }

    /// The digest reads a heap graph. A scalar has no graph, so the
    /// verifier rejects the instruction instead of letting the VM meet
    /// a value that is not an object.
    #[test]
    fn rejects_digest_on_a_scalar() {
        let mut m = module_with(vec![vec![ConstInt(1), Digest, Return]]);
        m.types.push(BcType::Digest);
        m.funcs[0].ret = 4;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("digest on non-object type"), "{e}");
    }

    /// A digest comparison reads two digests. Any other operand type
    /// rejects, so the value comparison in the VM cannot meet a shape
    /// that carries no digest.
    #[test]
    fn rejects_digest_comparison_on_other_types() {
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), EqDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
        let m = module_with(vec![vec![ConstInt(1), ConstInt(2), NeDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
        // A string is a heap value and still not a digest.
        let m = module_with(vec![vec![ConstStr(0), ConstStr(0), EqDigest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(
            e.message.contains("digest comparison on non-digest types"),
            "{e}"
        );
    }

    /// The digest result type must exist in the module type table.
    /// A module that omits it rejects instead of resolving to a
    /// neighbouring type.
    #[test]
    fn rejects_digest_without_the_result_type() {
        let m = module_with(vec![vec![ConstStr(0), Digest, Return]]);
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("Digest is not in the type table"), "{e}");
    }

    #[test]
    fn rejects_entry_with_parameters() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].params = vec![TY_INT];
        m.funcs[0].param_muts = vec![false];
        m.funcs[0].local_types = vec![TY_INT, TY_INT];
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("entry function"), "{e}");
    }

    #[test]
    fn rejects_generic_entry() {
        let mut m = module_with(vec![vec![ConstInt(1), Return]]);
        m.funcs[0].type_params = 1;
        let e = verify_module(&m).unwrap_err();
        assert!(e.message.contains("generic"), "{e}");
    }

    #[test]
    fn joins_subclass_stacks_at_merge_points() {
        // Both branches push a different subclass of Animal. The join
        // must settle at the common ancestor.
        let mut types = base_types();
        types.push(BcType::Class(0)); // 4 Animal
        types.push(BcType::Class(1)); // 5 Dog
        types.push(BcType::Class(2)); // 6 Cat
        let class = |name: &str, parent: u32| BcClass {
            name: name.to_string(),
            parent_args: Vec::new(),
            key: name.to_string(),
            is_final: false,
            parent,
            type_params: 0,
            kind: BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let m = Module {
            strings: vec![],
            types,
            selectors: vec![],
            apps: vec![],
            classes: vec![class("Animal", NO_PARENT), class("Dog", 0), class("Cat", 0)],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: TY_UNIT,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![
                    vec![ConstBool(true), JumpIfFalse(1), New(1), Jump(2)],
                    vec![New(2), Jump(2)],
                    vec![Pop, ConstUnit, Return],
                ],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        };
        assert!(verify_module(&m).is_ok(), "{:?}", verify_module(&m));
    }

    /// A class that inherits an instantiated generic parent fits that
    /// exact application and no other.
    ///
    /// `IntBox` inherits `Box[Int]` and takes no type parameter of its
    /// own. A rule that compared class names alone accepted it at a
    /// `Box[String]` position. The subtype rule walks the arguments,
    /// so the plain class position and the application position both
    /// answer the same relation.
    #[test]
    fn an_inherited_generic_parent_fits_one_application() {
        let class = |name: &str, parent: u32, args: Vec<u32>, params: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            parent,
            parent_args: args,
            type_params: params,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = base_types();
        m.types.push(BcType::Var(0)); // 4
        m.types.push(BcType::Inst(0, vec![TY_INT])); // 5 Box[Int]
        m.types.push(BcType::Inst(0, vec![TY_STR])); // 6 Box[String]
        m.types.push(BcType::Class(1)); // 7 IntBox
        m.classes = vec![
            class("Box", NO_PARENT, vec![], 1),
            class("IntBox", 0, vec![TY_INT], 0),
        ];
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core).expect("the tables verify");
        assert!(ctx.is_subtype(7, 5), "an IntBox fits Box[Int]");
        assert!(!ctx.is_subtype(7, 6), "an IntBox fits no Box[String]");
    }

    /// Sibling classes join through the full application of a generic
    /// parent. A class slot alone would lose the parent's argument.
    #[test]
    fn sibling_classes_join_at_one_generic_parent_application() {
        let class = |name: &str, parent: u32, args: Vec<u32>, params: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            parent,
            parent_args: args,
            type_params: params,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = base_types();
        m.types.push(BcType::Class(1)); // 4 IntLeft
        m.types.push(BcType::Class(2)); // 5 IntRight
        m.types.push(BcType::Class(3)); // 6 StringChild
        m.classes = vec![
            class("Box", NO_PARENT, vec![], 1),
            class("IntLeft", 0, vec![TY_INT], 0),
            class("IntRight", 0, vec![TY_INT], 0),
            class("StringChild", 0, vec![TY_STR], 0),
        ];
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core).expect("the tables verify");

        let joined = ctx.join(4, 5).expect("the siblings join");
        assert_eq!(ctx.ty(joined), BcType::Inst(0, vec![TY_INT]));
        assert_eq!(ctx.join(4, 6), None, "different applications do not join");
    }

    /// Shared type children form a DAG, not a tree. Each verifier walk
    /// must visit one node or pair once.
    #[test]
    fn shared_type_dags_do_not_duplicate_verifier_work() {
        const DEPTH: usize = 40;
        let class = |name: &str, parent: u32| BcClass {
            name: name.to_string(),
            key: name.to_string(),
            is_final: false,
            parent,
            parent_args: vec![],
            type_params: 0,
            kind: lm_bytecode::BcClassKind::Normal,
            fields: vec![],
            methods: vec![],
        };
        let mut types = base_types();
        types.push(BcType::Var(0));
        let mut bounded = (types.len() - 1) as u32;
        for _ in 0..DEPTH {
            types.push(BcType::Tuple(vec![bounded, bounded]));
            bounded = (types.len() - 1) as u32;
        }
        types.push(BcType::Class(1));
        let mut left = (types.len() - 1) as u32;
        types.push(BcType::Class(2));
        let mut right = (types.len() - 1) as u32;
        for _ in 0..DEPTH {
            types.push(BcType::Tuple(vec![left, left]));
            left = (types.len() - 1) as u32;
            types.push(BcType::Tuple(vec![right, right]));
            right = (types.len() - 1) as u32;
        }
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = types;
        m.classes = vec![
            class("Parent", NO_PARENT),
            class("Left", 0),
            class("Right", 0),
        ];
        let core = lm_bytecode::corepin::declared_layout(&m);
        let ctx = verify_tables(&m, core).expect("the tables verify");

        assert!(ctx.vars_bounded(bounded, 1, 0));
        assert!(!ctx.is_subtype(left, right));
        assert!(ctx.join(left, right).is_some());
    }

    /// A type table nests no deeper than `MAX_TYPE_DEPTH`.
    ///
    /// An artifact states its own type table, and a hand-built one can
    /// nest a type as deeply as the table holds entries. Every walk
    /// over a type costs at least its depth, and `join` costs the
    /// square of it. The bound therefore makes a deep type
    /// unrepresentable instead of hardening each walk against one.
    #[test]
    fn a_type_table_past_the_depth_bound_rejects() {
        let mut types = base_types();
        let mut deep = TY_INT;
        for _ in 0..MAX_TYPE_DEPTH {
            types.push(BcType::List(deep));
            deep = (types.len() - 1) as u32;
        }
        let mut m = module_with(vec![vec![ConstInt(0), Return]]);
        m.types = types;
        let error = verify_module(&m).expect_err("a type past the bound rejects");
        assert!(
            format!("{error:?}").contains("nests"),
            "the diagnostic names the depth rule: {error:?}"
        );
    }

    /// Every type walk answers at the bound on a small stack.
    #[test]
    fn a_type_table_at_the_depth_bound_walks_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                // `Var(0)` is depth 1, so this many list levels reach
                // the bound exactly.
                const DEPTH: u32 = MAX_TYPE_DEPTH - 1;
                // `join` tests the subtype relation at each level, so
                // its cost grows with the square of the depth. The
                // bound keeps that cost small.
                const JOIN_DEPTH: u32 = MAX_TYPE_DEPTH - 2;
                let class = |name: &str, parent: u32| BcClass {
                    name: name.to_string(),
                    key: name.to_string(),
                    is_final: false,
                    parent,
                    parent_args: vec![],
                    type_params: 0,
                    kind: lm_bytecode::BcClassKind::Normal,
                    fields: vec![],
                    methods: vec![],
                };
                let mut types = base_types();
                // `[[[ ... [T] ... ]]]`, nested `DEPTH` deep over the
                // type variable of a generic function.
                types.push(BcType::Var(0));
                let mut deep = (types.len() - 1) as u32;
                for _ in 0..DEPTH {
                    types.push(BcType::List(deep));
                    deep = (types.len() - 1) as u32;
                }
                // Two tuple chains of the same depth, one over each
                // child class. Their join walks to the common parent
                // at the innermost position.
                types.push(BcType::Class(1));
                let mut left = (types.len() - 1) as u32;
                types.push(BcType::Class(2));
                let mut right = (types.len() - 1) as u32;
                for _ in 0..JOIN_DEPTH {
                    types.push(BcType::Tuple(vec![left]));
                    left = (types.len() - 1) as u32;
                    types.push(BcType::Tuple(vec![right]));
                    right = (types.len() - 1) as u32;
                }
                let mut callee = plain_func("deep", vec![deep], TY_INT, vec![]);
                callee.type_params = 1;
                callee.local_types = vec![deep];
                callee.blocks = vec![vec![ConstInt(0), Return]];
                let mut m = module_with(vec![vec![ConstInt(0), Return]]);
                m.types = types;
                m.classes = vec![class("P", NO_PARENT), class("A", 0), class("B", 0)];
                m.apps = vec![TypeApp {
                    types: vec![TY_INT],
                    rows: vec![],
                }];
                m.funcs.push(callee);
                // The whole pass runs first: the table rules read every
                // entry, and `vars_bounded` walks the deep parameter.
                verify_module(&m).expect("the module verifies");
                // The three remaining walks run directly, because no
                // small program reaches a type this deep.
                let core = lm_bytecode::corepin::declared_layout(&m);
                let ctx = verify_tables(&m, core).expect("the tables verify");
                assert!(ctx.vars_bounded(deep, 1, 0));
                let closed = ctx.subst(deep, &[TY_INT], &[]);
                assert_ne!(closed, deep);
                assert!(ctx.is_subtype(closed, closed));
                assert!(!ctx.is_subtype(left, right));
                assert!(ctx.join(left, right).is_some());
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }
}
