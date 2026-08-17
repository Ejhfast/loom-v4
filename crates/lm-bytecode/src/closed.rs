//! The canonical closed type table and the type environment table
//! (`docs/specs/snapshot-image-admission.md` section 5.6).
//!
//! The verifier proves one generic body once, with the type variables
//! of that body opaque. One activation of the body needs the type
//! arguments its call site applied. Three positions carry no call
//! site: the bottom frame of a machine, a closure that outlived its
//! creator frame, and a machine past its constructor. The runtime
//! therefore retains the evidence before any capture.
//!
//! One table holds every closed type expression of one world. No entry
//! holds a free type variable, and every entry has a canonical content
//! digest, so one closed type has one identity in every process. A
//! second table holds the environments: one environment is the type
//! and effect arguments of one activation. Environment zero is empty,
//! so a monomorphic state stores zero and performs no type work.
//!
//! The table belongs to one world. An untrusted restore never grows
//! shared module state: it re-interns the records of the image into
//! the table of the target world.

use crate::{BcClass, BcRow, BcType, Module, NO_PARENT};
use lm_value::TypeEnvId;
use std::collections::HashMap;

/// One entry of the closed type table, by index.
pub type ClosedTypeId = u32;

/// One closed effect row: the module string slots of its operations
/// and groups, in canonical order without a duplicate.
///
/// A closed row holds no effect variable, because an environment
/// substitutes every one of them.
pub type ClosedRow = Vec<u32>;

/// One node of the closed type grammar.
///
/// Every child index names an earlier entry of the same table, so a
/// walk over a node terminates. The grammar mirrors `BcType` without
/// `Var`: a closed type holds no free type variable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClosedType {
    Unit,
    Bool,
    Int,
    Str,
    StringBuilder,
    ByteBuffer,
    Fault,
    Request,
    PolicyTable,
    EmptyVm,
    Digest,
    SnapshotImage,
    /// An instance type of a class without generic parameters.
    Class(u32),
    /// An instance type of a generic class applied to arguments.
    Inst(u32, Vec<ClosedTypeId>),
    List(ClosedTypeId),
    Map(ClosedTypeId, ClosedTypeId),
    Tuple(Vec<ClosedTypeId>),
    /// A function value type: parameters, `mut` markers, result, and
    /// one closed effect row.
    Fn(Vec<ClosedTypeId>, Vec<bool>, ClosedTypeId, ClosedRow),
    Vm(ClosedTypeId),
    PendingCall(ClosedTypeId, ClosedTypeId),
    Handle(ClosedTypeId, ClosedTypeId),
    /// An identity-indexed operation value: the manifest slot and the
    /// callable function type.
    Op(u32, ClosedTypeId),
    Snapshot(ClosedTypeId),
}

impl ClosedType {
    /// Every child index this node names, in canonical order.
    pub fn children(&self) -> Vec<ClosedTypeId> {
        match self {
            ClosedType::Class(_) => Vec::new(),
            ClosedType::Inst(_, args) | ClosedType::Tuple(args) => args.clone(),
            ClosedType::List(e) | ClosedType::Vm(e) | ClosedType::Snapshot(e) => vec![*e],
            ClosedType::Op(_, e) => vec![*e],
            ClosedType::Map(a, b) | ClosedType::PendingCall(a, b) | ClosedType::Handle(a, b) => {
                vec![*a, *b]
            }
            ClosedType::Fn(params, _, ret, _) => {
                let mut out = params.clone();
                out.push(*ret);
                out
            }
            _ => Vec::new(),
        }
    }

    /// Rebuild this node with every child index remapped.
    ///
    /// A restore re-interns the records of an image into the table of
    /// the target world, so every stored index moves.
    pub fn remap(&self, map: impl Fn(ClosedTypeId) -> ClosedTypeId) -> ClosedType {
        match self {
            ClosedType::Inst(c, args) => {
                ClosedType::Inst(*c, args.iter().map(|a| map(*a)).collect())
            }
            ClosedType::Tuple(elems) => ClosedType::Tuple(elems.iter().map(|e| map(*e)).collect()),
            ClosedType::List(e) => ClosedType::List(map(*e)),
            ClosedType::Vm(e) => ClosedType::Vm(map(*e)),
            ClosedType::Snapshot(e) => ClosedType::Snapshot(map(*e)),
            ClosedType::Op(op, e) => ClosedType::Op(*op, map(*e)),
            ClosedType::Map(a, b) => ClosedType::Map(map(*a), map(*b)),
            ClosedType::PendingCall(a, b) => ClosedType::PendingCall(map(*a), map(*b)),
            ClosedType::Handle(a, b) => ClosedType::Handle(map(*a), map(*b)),
            ClosedType::Fn(params, muts, ret, row) => ClosedType::Fn(
                params.iter().map(|p| map(*p)).collect(),
                muts.clone(),
                map(*ret),
                row.clone(),
            ),
            other => other.clone(),
        }
    }
}

/// The type and effect arguments of one activation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TypeEnv {
    /// The closed type argument of each type parameter, in order.
    pub types: Vec<ClosedTypeId>,
    /// The closed effect argument of each effect parameter, in order.
    pub rows: Vec<ClosedRow>,
}

impl TypeEnv {
    /// True when the environment binds nothing.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.rows.is_empty()
    }
}

/// The type environment table of one world reached its cap.
///
/// The language permits polymorphic recursion, so a program can ask
/// for environments without bound. The cap turns that into one local
/// resource fault instead of unbounded growth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeEnvFull {
    /// True when the closed type nodes reached their cap, false when
    /// the environment nodes did.
    pub types: bool,
}

/// The default closed type node cap of one world.
pub const DEFAULT_MAX_CLOSED_TYPES: u32 = 1 << 16;

/// The default environment node cap of one world.
pub const DEFAULT_MAX_TYPE_ENVS: u32 = 1 << 16;

/// The domain separator of one closed type digest.
const DIGEST_DOMAIN: &[u8] = b"lm-closed-type-v1\0";

/// The canonical closed type table and environment table of one world.
///
/// The table interns, so one closed type has one index. It also caches
/// each derived environment by its parent environment and its type
/// application, so a repeated generic call reuses one index.
#[derive(Debug)]
pub struct TypeEnvs {
    types: Vec<ClosedType>,
    type_index: HashMap<ClosedType, ClosedTypeId>,
    envs: Vec<TypeEnv>,
    env_index: HashMap<TypeEnv, TypeEnvId>,
    /// One derived environment per `(parent environment, application)`.
    derived: HashMap<(TypeEnvId, u32), TypeEnvId>,
    /// One closed type per `(module type, environment)`.
    closed: HashMap<(u32, TypeEnvId), ClosedTypeId>,
    /// The content digest of each closed type node, filled on demand.
    digests: Vec<Option<[u8; 32]>>,
    max_types: u32,
    max_envs: u32,
}

impl Default for TypeEnvs {
    fn default() -> TypeEnvs {
        TypeEnvs::new(DEFAULT_MAX_CLOSED_TYPES, DEFAULT_MAX_TYPE_ENVS)
    }
}

impl TypeEnvs {
    /// One empty table with an exact node cap.
    ///
    /// Environment zero is the empty environment, so a monomorphic
    /// state stores zero and the table allocates nothing for it.
    pub fn new(max_types: u32, max_envs: u32) -> TypeEnvs {
        let mut table = TypeEnvs {
            types: Vec::new(),
            type_index: HashMap::new(),
            envs: Vec::new(),
            env_index: HashMap::new(),
            derived: HashMap::new(),
            closed: HashMap::new(),
            digests: Vec::new(),
            max_types,
            max_envs: max_envs.max(1),
        };
        let empty = TypeEnv::default();
        table.envs.push(empty.clone());
        table.env_index.insert(empty, TypeEnvId::EMPTY);
        table
    }

    /// The number of closed type nodes the table holds.
    pub fn type_count(&self) -> usize {
        self.types.len()
    }

    /// The number of environment nodes the table holds, including the
    /// empty environment.
    pub fn env_count(&self) -> usize {
        self.envs.len()
    }

    /// The closed type node cap of this table.
    pub fn max_types(&self) -> u32 {
        self.max_types
    }

    /// The environment node cap of this table.
    pub fn max_envs(&self) -> u32 {
        self.max_envs
    }

    /// One closed type node by index.
    pub fn ty(&self, id: ClosedTypeId) -> Option<&ClosedType> {
        self.types.get(id as usize)
    }

    /// One environment by index.
    pub fn env(&self, id: TypeEnvId) -> Option<&TypeEnv> {
        self.envs.get(id.0 as usize)
    }

    /// Intern one closed type node.
    ///
    /// Every child index must already name an entry of this table, so
    /// the caller builds a node from the bottom up.
    pub fn intern(&mut self, node: ClosedType) -> Result<ClosedTypeId, TypeEnvFull> {
        if let Some(id) = self.type_index.get(&node) {
            return Ok(*id);
        }
        if self.types.len() as u32 >= self.max_types {
            return Err(TypeEnvFull { types: true });
        }
        let id = self.types.len() as ClosedTypeId;
        self.types.push(node.clone());
        self.digests.push(None);
        self.type_index.insert(node, id);
        Ok(id)
    }

    /// Intern one environment.
    pub fn intern_env(&mut self, env: TypeEnv) -> Result<TypeEnvId, TypeEnvFull> {
        if env.is_empty() {
            return Ok(TypeEnvId::EMPTY);
        }
        if let Some(id) = self.env_index.get(&env) {
            return Ok(*id);
        }
        if self.envs.len() as u32 >= self.max_envs {
            return Err(TypeEnvFull { types: false });
        }
        let id = TypeEnvId(self.envs.len() as u32);
        self.envs.push(env.clone());
        self.env_index.insert(env, id);
        Ok(id)
    }

    /// Close one module type under one environment.
    ///
    /// The call substitutes every type variable and every effect
    /// variable, so the answer holds neither. A monomorphic type under
    /// the empty environment still interns, because the closed table is
    /// the one identity the witness records name.
    /// The walk is iterative. A module type table can nest a type as
    /// deeply as it holds entries, and a hand-built artifact chooses
    /// that depth, so a walk on the Rust stack would abort the host.
    pub fn close(
        &mut self,
        module: &Module,
        ty: u32,
        env: TypeEnvId,
    ) -> Result<ClosedTypeId, TypeEnvFull> {
        if let Some(id) = self.closed.get(&(ty, env)) {
            return Ok(*id);
        }
        // Each entry pairs one module type with the flag that says
        // whether its children already sit on the stack.
        let mut stack: Vec<(u32, bool)> = vec![(ty, false)];
        let mut children: Vec<u32> = Vec::new();
        while let Some((cur, expanded)) = stack.pop() {
            if self.closed.contains_key(&(cur, env)) {
                continue;
            }
            // A module type index the caller cannot resolve closes to
            // the unit type. Every caller inside this workspace reads
            // a verified module, so the branch is unreachable there; a
            // hand-built module must not panic here.
            let node = match module.types.get(cur as usize) {
                Some(node) => node.clone(),
                None => BcType::Unit,
            };
            if !expanded {
                stack.push((cur, true));
                children.clear();
                bc_children(&node, &mut children);
                for child in &children {
                    stack.push((*child, false));
                }
                continue;
            }
            let closed = self.close_flat(module, &node, env)?;
            self.closed.insert((cur, env), closed);
        }
        match self.closed.get(&(ty, env)) {
            Some(id) => Ok(*id),
            None => self.intern(ClosedType::Unit),
        }
    }

    /// Close one module type node whose children already closed.
    fn close_flat(
        &mut self,
        module: &Module,
        node: &BcType,
        env: TypeEnvId,
    ) -> Result<ClosedTypeId, TypeEnvFull> {
        let child = |table: &TypeEnvs, ty: u32| -> ClosedTypeId {
            table.closed.get(&(ty, env)).copied().unwrap_or(0)
        };
        let built = match node {
            BcType::Unit => ClosedType::Unit,
            BcType::Bool => ClosedType::Bool,
            BcType::Int => ClosedType::Int,
            BcType::Str => ClosedType::Str,
            BcType::StringBuilder => ClosedType::StringBuilder,
            BcType::ByteBuffer => ClosedType::ByteBuffer,
            BcType::Fault => ClosedType::Fault,
            BcType::Request => ClosedType::Request,
            BcType::PolicyTable => ClosedType::PolicyTable,
            BcType::EmptyVm => ClosedType::EmptyVm,
            BcType::Digest => ClosedType::Digest,
            BcType::SnapshotImage => ClosedType::SnapshotImage,
            BcType::Class(c) => ClosedType::Class(*c),
            BcType::Var(i) => {
                // A variable the environment does not bind has no
                // closed form. The unit type stands in, and the
                // boundary check rejects the value that names it,
                // because the derivation from the code names another
                // type.
                let bound = self
                    .env(env)
                    .and_then(|e| e.types.get(*i as usize))
                    .copied();
                return match bound {
                    Some(id) => Ok(id),
                    None => self.intern(ClosedType::Unit),
                };
            }
            BcType::Inst(c, args) => {
                ClosedType::Inst(*c, args.iter().map(|a| child(self, *a)).collect())
            }
            BcType::List(e) => ClosedType::List(child(self, *e)),
            BcType::Map(k, v) => ClosedType::Map(child(self, *k), child(self, *v)),
            BcType::Tuple(elems) => {
                ClosedType::Tuple(elems.iter().map(|e| child(self, *e)).collect())
            }
            BcType::Fn(params, muts, ret, row) => ClosedType::Fn(
                params.iter().map(|p| child(self, *p)).collect(),
                muts.clone(),
                child(self, *ret),
                self.close_row(module, row, env),
            ),
            BcType::Vm(t) => ClosedType::Vm(child(self, *t)),
            BcType::Snapshot(t) => ClosedType::Snapshot(child(self, *t)),
            BcType::PendingCall(a, r) => ClosedType::PendingCall(child(self, *a), child(self, *r)),
            BcType::Handle(m, r) => ClosedType::Handle(child(self, *m), child(self, *r)),
            BcType::Op(op, f) => ClosedType::Op(*op, child(self, *f)),
        };
        self.intern(built)
    }

    /// Close one module effect row under one environment.
    ///
    /// The answer is canonical: the operation names sort by their text
    /// and hold no duplicate, so one closed row has one identity.
    pub fn close_row(&self, module: &Module, row: &[BcRow], env: TypeEnvId) -> ClosedRow {
        let mut out: ClosedRow = Vec::with_capacity(row.len());
        for elem in row {
            match elem {
                BcRow::Op(slot) => out.push(*slot),
                BcRow::Var(i) => {
                    if let Some(bound) = self.env(env).and_then(|e| e.rows.get(*i as usize)) {
                        out.extend_from_slice(bound);
                    }
                }
            }
        }
        canonical_row(module, out)
    }

    /// Derive one environment from a parent environment and one type
    /// application.
    ///
    /// The application is expressed in the type arguments of the
    /// caller, so it closes through the parent environment first. The
    /// answer is cached by `(parent, application)`, so a repeated
    /// generic call reuses one index.
    pub fn derive(
        &mut self,
        module: &Module,
        parent: TypeEnvId,
        app: u32,
    ) -> Result<TypeEnvId, TypeEnvFull> {
        if let Some(id) = self.derived.get(&(parent, app)) {
            return Ok(*id);
        }
        let entry = match module.apps.get(app as usize) {
            Some(entry) => entry.clone(),
            None => return Ok(TypeEnvId::EMPTY),
        };
        let mut types = Vec::with_capacity(entry.types.len());
        for ty in &entry.types {
            types.push(self.close(module, *ty, parent)?);
        }
        let rows: Vec<ClosedRow> = entry
            .rows
            .iter()
            .map(|row| self.close_row(module, row, parent))
            .collect();
        let id = self.intern_env(TypeEnv { types, rows })?;
        self.derived.insert((parent, app), id);
        Ok(id)
    }

    /// Build one environment from an explicit closed argument list.
    pub fn env_of(
        &mut self,
        types: Vec<ClosedTypeId>,
        rows: Vec<ClosedRow>,
    ) -> Result<TypeEnvId, TypeEnvFull> {
        self.intern_env(TypeEnv { types, rows })
    }

    /// The nominal class and closed arguments of one instance type.
    pub fn as_instance(&self, ty: ClosedTypeId) -> Option<(u32, Vec<ClosedTypeId>)> {
        match self.ty(ty)? {
            ClosedType::Class(c) => Some((*c, Vec::new())),
            ClosedType::Inst(c, args) => Some((*c, args.clone())),
            _ => None,
        }
    }

    /// The arguments of `ancestor` seen from an instance of `class`
    /// applied to `args`.
    ///
    /// Only a class without type parameters may declare a parent, so a
    /// declared parent records closed arguments and an enum case passes
    /// the arguments of its family through. The walk therefore never
    /// substitutes.
    pub fn ancestor_args(
        &mut self,
        module: &Module,
        class: u32,
        args: &[ClosedTypeId],
        ancestor: u32,
    ) -> Option<Vec<ClosedTypeId>> {
        let mut cur = class;
        let mut cur_args = args.to_vec();
        let mut steps = 0usize;
        loop {
            if cur == ancestor {
                return Some(cur_args);
            }
            steps += 1;
            if steps > module.classes.len() {
                return None;
            }
            let entry: &BcClass = module.classes.get(cur as usize)?;
            if entry.parent == NO_PARENT {
                return None;
            }
            let parent = entry.parent;
            if !entry.parent_args.is_empty() {
                // A declared generic parent records closed arguments,
                // so the walk closes them under the empty environment.
                let mut out = Vec::with_capacity(entry.parent_args.len());
                for arg in entry.parent_args.clone() {
                    out.push(self.close(module, arg, TypeEnvId::EMPTY).ok()?);
                }
                cur_args = out;
            } else if module.classes.get(parent as usize)?.type_params == 0 {
                cur_args = Vec::new();
            }
            cur = parent;
        }
    }

    /// The mailbox message type of one closed proc instance type.
    ///
    /// `None` marks a type that is not an instance of a subclass of the
    /// core class `Proc`.
    pub fn proc_mailbox(
        &mut self,
        module: &Module,
        proc_class: Option<u32>,
        ty: ClosedTypeId,
    ) -> Option<ClosedTypeId> {
        let proc = proc_class?;
        let (class, args) = self.as_instance(ty)?;
        let found = self.ancestor_args(module, class, &args, proc)?;
        found.first().copied()
    }

    /// The canonical content digest of one closed type node.
    ///
    /// The digest names a class by its verified definition hash, an
    /// operation by its manifest identity, and an effect name by its
    /// text, so one closed type has one identity in every process. A
    /// child enters through its own digest, so the answer is a content
    /// address of the whole expression.
    /// The walk is iterative. A closed type table can nest a type as
    /// deeply as it holds entries, and polymorphic recursion is legal,
    /// so a walk on the Rust stack would abort the host.
    pub fn digest(
        &mut self,
        module: &Module,
        class_hashes: &[[u8; 32]],
        id: ClosedTypeId,
    ) -> [u8; 32] {
        if let Some(Some(hit)) = self.digests.get(id as usize) {
            return *hit;
        }
        // Each entry pairs one node with the flag that says whether
        // its children already sit on the stack. Every child names an
        // earlier entry, so the walk terminates.
        let mut stack: Vec<(ClosedTypeId, bool)> = vec![(id, false)];
        while let Some((cur, expanded)) = stack.pop() {
            if matches!(self.digests.get(cur as usize), Some(Some(_))) {
                continue;
            }
            let Some(node) = self.ty(cur).cloned() else {
                continue;
            };
            if !expanded {
                stack.push((cur, true));
                for child in node.children() {
                    stack.push((child, false));
                }
                continue;
            }
            let digest = self.digest_flat(module, class_hashes, &node);
            if let Some(slot) = self.digests.get_mut(cur as usize) {
                *slot = Some(digest);
            }
        }
        self.digests
            .get(id as usize)
            .copied()
            .flatten()
            .unwrap_or([0u8; 32])
    }

    /// The digest of one node whose children already answered.
    fn digest_flat(
        &self,
        module: &Module,
        class_hashes: &[[u8; 32]],
        node: &ClosedType,
    ) -> [u8; 32] {
        let mut out: Vec<u8> = Vec::with_capacity(64);
        out.extend_from_slice(DIGEST_DOMAIN);
        out.push(tag_of(node));
        let child = |table: &TypeEnvs, out: &mut Vec<u8>, c: ClosedTypeId| {
            let d = table
                .digests
                .get(c as usize)
                .copied()
                .flatten()
                .unwrap_or([0u8; 32]);
            out.extend_from_slice(&d);
        };
        match node {
            ClosedType::Class(c) => {
                out.extend_from_slice(&class_hash(class_hashes, *c));
            }
            ClosedType::Inst(c, args) => {
                out.extend_from_slice(&class_hash(class_hashes, *c));
                out.extend_from_slice(&(args.len() as u32).to_le_bytes());
                for arg in args {
                    child(self, &mut out, *arg);
                }
            }
            ClosedType::List(e) | ClosedType::Vm(e) | ClosedType::Snapshot(e) => {
                child(self, &mut out, *e);
            }
            ClosedType::Map(a, b) | ClosedType::PendingCall(a, b) | ClosedType::Handle(a, b) => {
                child(self, &mut out, *a);
                child(self, &mut out, *b);
            }
            ClosedType::Tuple(elems) => {
                out.extend_from_slice(&(elems.len() as u32).to_le_bytes());
                for elem in elems {
                    child(self, &mut out, *elem);
                }
            }
            ClosedType::Fn(params, muts, ret, row) => {
                out.extend_from_slice(&(params.len() as u32).to_le_bytes());
                for (param, mutable) in params.iter().zip(muts.iter()) {
                    out.push(u8::from(*mutable));
                    child(self, &mut out, *param);
                }
                child(self, &mut out, *ret);
                out.extend_from_slice(&(row.len() as u32).to_le_bytes());
                for slot in row {
                    let name = module
                        .strings
                        .get(*slot as usize)
                        .map(String::as_str)
                        .unwrap_or("");
                    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    out.extend_from_slice(name.as_bytes());
                }
            }
            ClosedType::Op(op, f) => {
                out.extend_from_slice(&lm_abi::op_identity(*op));
                child(self, &mut out, *f);
            }
            _ => {}
        }
        crate::hash::sha256(&out)
    }
}

/// The child type indices of one module type, in declaration order.
fn bc_children(node: &BcType, out: &mut Vec<u32>) {
    match node {
        BcType::Inst(_, args) | BcType::Tuple(args) => out.extend(args),
        BcType::List(e) | BcType::Vm(e) | BcType::Snapshot(e) | BcType::Op(_, e) => out.push(*e),
        BcType::Map(a, b) | BcType::PendingCall(a, b) | BcType::Handle(a, b) => {
            out.push(*a);
            out.push(*b);
        }
        BcType::Fn(params, _, ret, _) => {
            out.extend(params);
            out.push(*ret);
        }
        _ => {}
    }
}

/// The class definition hash of one class slot.
fn class_hash(class_hashes: &[[u8; 32]], class: u32) -> [u8; 32] {
    class_hashes.get(class as usize).copied().unwrap_or([0; 32])
}

/// The wire and digest tag of one closed type node.
///
/// The tag is part of the digest contract and part of the container
/// encoding, so the order never changes.
pub fn tag_of(node: &ClosedType) -> u8 {
    match node {
        ClosedType::Unit => 0,
        ClosedType::Bool => 1,
        ClosedType::Int => 2,
        ClosedType::Str => 3,
        ClosedType::StringBuilder => 4,
        ClosedType::ByteBuffer => 5,
        ClosedType::Fault => 6,
        ClosedType::Request => 7,
        ClosedType::PolicyTable => 8,
        ClosedType::EmptyVm => 9,
        ClosedType::Digest => 10,
        ClosedType::SnapshotImage => 11,
        ClosedType::Class(_) => 12,
        ClosedType::Inst(_, _) => 13,
        ClosedType::List(_) => 14,
        ClosedType::Map(_, _) => 15,
        ClosedType::Tuple(_) => 16,
        ClosedType::Fn(_, _, _, _) => 17,
        ClosedType::Vm(_) => 18,
        ClosedType::PendingCall(_, _) => 19,
        ClosedType::Handle(_, _) => 20,
        ClosedType::Op(_, _) => 21,
        ClosedType::Snapshot(_) => 22,
    }
}

/// Sort one closed row into canonical order and remove duplicates.
///
/// The order follows the operation text, so it never depends on the
/// string-pool order of one linked program.
pub fn canonical_row(module: &Module, mut row: ClosedRow) -> ClosedRow {
    let name = |slot: &u32| -> &str {
        module
            .strings
            .get(*slot as usize)
            .map(String::as_str)
            .unwrap_or("")
    };
    row.sort_by(|a, b| name(a).cmp(name(b)).then(a.cmp(b)));
    row.dedup_by(|a, b| name(a) == name(b));
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BcClass, BcClassKind, Func, TypeApp};

    /// One module with `Int`, `Var(0)`, `List[Var(0)]`, and one
    /// application of `Int`.
    fn module() -> Module {
        Module {
            strings: vec!["Io".to_string(), "Fs".to_string()],
            types: vec![
                BcType::Unit,
                BcType::Int,
                BcType::Var(0),
                BcType::List(2),
                BcType::Class(0),
            ],
            selectors: vec![],
            apps: vec![TypeApp {
                types: vec![1],
                rows: vec![],
            }],
            imports: vec![],
            core_roles: [crate::NO_ROLE; crate::CORE_ROLE_COUNT],
            classes: vec![BcClass {
                name: "C".to_string(),
                key: "C".to_string(),
                parent: NO_PARENT,
                parent_args: vec![],
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![],
                methods: vec![],
            }],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 1,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        }
    }

    #[test]
    fn the_empty_environment_is_index_zero_and_allocates_nothing() {
        let table = TypeEnvs::default();
        assert_eq!(table.env_count(), 1);
        assert_eq!(table.type_count(), 0);
        assert!(table.env(TypeEnvId::EMPTY).expect("empty").is_empty());
    }

    #[test]
    fn one_application_derives_one_environment_and_caches_it() {
        let m = module();
        let mut table = TypeEnvs::default();
        let a = table.derive(&m, TypeEnvId::EMPTY, 0).expect("derived");
        let b = table.derive(&m, TypeEnvId::EMPTY, 0).expect("derived");
        assert_eq!(a, b);
        assert_ne!(a, TypeEnvId::EMPTY);
        assert_eq!(table.env_count(), 2);
    }

    #[test]
    fn closing_substitutes_every_variable() {
        let m = module();
        let mut table = TypeEnvs::default();
        let env = table.derive(&m, TypeEnvId::EMPTY, 0).expect("derived");
        // `List[Var(0)]` under `[Int]` closes to `List[Int]`.
        let closed = table.close(&m, 3, env).expect("closed");
        let int = table.close(&m, 1, TypeEnvId::EMPTY).expect("closed");
        assert_eq!(table.ty(closed), Some(&ClosedType::List(int)));
    }

    #[test]
    fn one_closed_type_has_one_index() {
        let m = module();
        let mut table = TypeEnvs::default();
        let a = table.close(&m, 1, TypeEnvId::EMPTY).expect("closed");
        let b = table.close(&m, 1, TypeEnvId::EMPTY).expect("closed");
        assert_eq!(a, b);
    }

    #[test]
    fn the_node_cap_refuses_a_new_type() {
        let m = module();
        let mut table = TypeEnvs::new(1, 8);
        table
            .close(&m, 1, TypeEnvId::EMPTY)
            .expect("the first fits");
        assert_eq!(
            table.close(&m, 3, TypeEnvId::EMPTY).err(),
            Some(TypeEnvFull { types: true })
        );
    }

    #[test]
    fn the_environment_cap_refuses_a_new_environment() {
        let m = module();
        let mut table = TypeEnvs::new(64, 1);
        assert_eq!(
            table.derive(&m, TypeEnvId::EMPTY, 0).err(),
            Some(TypeEnvFull { types: false })
        );
    }

    #[test]
    fn a_digest_reads_content_and_not_a_class_slot() {
        let m = module();
        let mut table = TypeEnvs::default();
        let class = table.close(&m, 4, TypeEnvId::EMPTY).expect("closed");
        let one = table.digest(&m, &[[7u8; 32]], class);
        let mut other = TypeEnvs::default();
        // The same class under another slot number with the same
        // definition hash answers the same digest.
        let same = other.intern(ClosedType::Class(0)).expect("interned");
        assert_eq!(other.digest(&m, &[[7u8; 32]], same), one);
        let mut third = TypeEnvs::default();
        let differs = third.intern(ClosedType::Class(0)).expect("interned");
        assert_ne!(third.digest(&m, &[[9u8; 32]], differs), one);
    }

    /// A deeply nested type never grows the Rust stack.
    ///
    /// A hand-built artifact states its module type table, and a
    /// snapshot container states its closed type table, so both depths
    /// are attacker data. `close` and `digest` are iterative, so they
    /// answer on a small stack instead of aborting the host.
    ///
    /// `digest` is reachable from a legal program as well: polymorphic
    /// recursion is legal, and capture digests the closed result type
    /// of every machine.
    #[test]
    fn a_deeply_nested_closed_type_never_recurses() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                const DEPTH: u32 = 50_000;
                let mut m = module();
                // `[[[ ... [Int] ... ]]]`, nested `DEPTH` deep.
                m.types = vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
                let mut deep = 2u32;
                for _ in 0..DEPTH {
                    m.types.push(BcType::List(deep));
                    deep = (m.types.len() - 1) as u32;
                }
                let mut table = TypeEnvs::new(u32::MAX, u32::MAX);
                let closed = table.close(&m, deep, TypeEnvId::EMPTY).expect("closed");
                let digest = table.digest(&m, &[], closed);
                assert_ne!(digest, [0u8; 32]);
                // The cached answer is the same answer.
                assert_eq!(table.digest(&m, &[], closed), digest);
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }

    #[test]
    fn a_canonical_row_sorts_by_text_and_dedups() {
        let m = module();
        // Slot 1 is `Fs` and slot 0 is `Io`, so the text order swaps
        // the slot order.
        assert_eq!(canonical_row(&m, vec![0, 1, 0]), vec![1, 0]);
    }
}
