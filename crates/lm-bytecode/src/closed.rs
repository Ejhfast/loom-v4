//! The canonical closed type table and the type environment table
//! (`docs/specs/sidecar/snapshot-image-admission.md` section 5.6).
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

use crate::{BcClass, BcClassKind, BcRow, BcType, Module, NO_PARENT};
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
    Fault,
    Request,
    PolicyTable,
    Vm,
    Digest,
    VmSnapshot,
    Bytes,
    FileHandle,
    ResourceHandle,
    HostResource,
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
    /// A function value that cannot escape the active call chain.
    Callback(Vec<ClosedTypeId>, Vec<bool>, ClosedTypeId, ClosedRow),
    Run(ClosedTypeId),
    Wait(ClosedTypeId),
    PendingCall(ClosedTypeId, ClosedTypeId),
    Handle(ClosedTypeId, ClosedTypeId),
    /// An identity-indexed operation value: the manifest slot and the
    /// callable function type.
    Op(u32, ClosedTypeId),
    RunSnapshot(ClosedTypeId),
}

impl ClosedType {
    /// The number of child type indices in this node.
    pub fn child_count(&self) -> usize {
        match self {
            ClosedType::Inst(_, args) | ClosedType::Tuple(args) => args.len(),
            ClosedType::List(_)
            | ClosedType::Run(_)
            | ClosedType::Wait(_)
            | ClosedType::RunSnapshot(_)
            | ClosedType::Op(_, _) => 1,
            ClosedType::Map(_, _) | ClosedType::PendingCall(_, _) | ClosedType::Handle(_, _) => 2,
            ClosedType::Fn(params, _, _, _) | ClosedType::Callback(params, _, _, _) => {
                params.len() + 1
            }
            _ => 0,
        }
    }

    /// Every child index this node names, in canonical order.
    pub fn children(&self) -> Vec<ClosedTypeId> {
        match self {
            ClosedType::Class(_) => Vec::new(),
            ClosedType::Inst(_, args) | ClosedType::Tuple(args) => args.clone(),
            ClosedType::List(e)
            | ClosedType::Run(e)
            | ClosedType::Wait(e)
            | ClosedType::RunSnapshot(e) => vec![*e],
            ClosedType::Op(_, e) => vec![*e],
            ClosedType::Map(a, b) | ClosedType::PendingCall(a, b) | ClosedType::Handle(a, b) => {
                vec![*a, *b]
            }
            ClosedType::Fn(params, _, ret, _) | ClosedType::Callback(params, _, ret, _) => {
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
            ClosedType::Run(e) => ClosedType::Run(map(*e)),
            ClosedType::Wait(e) => ClosedType::Wait(map(*e)),
            ClosedType::RunSnapshot(e) => ClosedType::RunSnapshot(map(*e)),
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
            ClosedType::Callback(params, muts, ret, row) => ClosedType::Callback(
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

/// One prepared import into a world type table.
///
/// Preparation resolves every source ordinal and reserves destination
/// storage. Commit then appends the missing records without a limit
/// check or an allocation.
#[derive(Debug)]
pub struct TypeImportPlan {
    types: Vec<(ClosedType, u32)>,
    type_index: HashMap<ClosedType, ClosedTypeId>,
    envs: Vec<TypeEnv>,
    env_index: HashMap<TypeEnv, TypeEnvId>,
    env_map: Vec<TypeEnvId>,
    type_map: Vec<ClosedTypeId>,
}

/// Runtime derivations rooted at one dense type environment.
#[derive(Debug, Default)]
struct TypeEnvCache {
    /// Generic applications, sorted by module application index.
    derived: Vec<(u32, TypeEnvId)>,
    /// Method compositions, sorted by class, function, and own environment.
    methods: Vec<((u32, u32, TypeEnvId), TypeEnvId)>,
}

impl TypeImportPlan {
    /// The destination identifier of each source environment.
    pub fn env_map(&self) -> &[TypeEnvId] {
        &self.env_map
    }

    /// The destination identifier of each source closed type.
    pub fn type_map(&self) -> &[ClosedTypeId] {
        &self.type_map
    }
}

/// The default closed type node cap of one world.
pub const DEFAULT_MAX_CLOSED_TYPES: u32 = 1 << 16;

/// The default environment node cap of one world.
pub const DEFAULT_MAX_TYPE_ENVS: u32 = 1 << 16;

/// The deepest a closed type may nest.
///
/// A walk over a closed type costs at least its depth. The bound keeps
/// every walk cheap, and it keeps a recursive walk inside the Rust
/// stack. Polymorphic recursion deepens a type as a program runs, so
/// the bound also states where such a program takes a local fault.
pub const MAX_CLOSED_DEPTH: u32 = 128;

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
    /// Runtime derivations for each dense environment.
    ///
    /// Each cache vector stays sorted by integer indices. This layout
    /// avoids hashing integer keys on every generic call.
    env_cache: Vec<TypeEnvCache>,
    /// The retained entries in all runtime environment caches.
    cache_entries: u32,
    /// One closed type per `(module type, environment)`.
    closed: HashMap<(u32, TypeEnvId), ClosedTypeId>,
    /// The last closed pair and its result.
    last_closed: Option<(u32, TypeEnvId, ClosedTypeId)>,
    /// The content digest of each closed type node, filled on demand.
    digests: Vec<Option<[u8; 32]>>,
    /// The nesting depth of each closed type node.
    ///
    /// A child always holds a lower identifier, so `intern` reads the
    /// depth of each child and stores one more than the largest.
    depths: Vec<u32>,
    max_types: u32,
    max_envs: u32,
}

impl Default for TypeEnvs {
    fn default() -> TypeEnvs {
        TypeEnvs::new(DEFAULT_MAX_CLOSED_TYPES, DEFAULT_MAX_TYPE_ENVS)
    }
}

impl TypeEnvs {
    /// Read one cached closed-type digest.
    pub fn cached_digest(&self, id: ClosedTypeId) -> Option<[u8; 32]> {
        self.digests.get(id as usize).copied().flatten()
    }

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
            env_cache: Vec::new(),
            cache_entries: 0,
            closed: HashMap::new(),
            last_closed: None,
            digests: Vec::new(),
            depths: Vec::new(),
            max_types,
            max_envs: max_envs.max(1),
        };
        let empty = TypeEnv::default();
        table.envs.push(empty.clone());
        table.env_index.insert(empty, TypeEnvId::EMPTY);
        table.env_cache.push(TypeEnvCache::default());
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

    /// Reserve table storage without adding a record.
    pub fn reserve_capacity(&mut self, types: usize, envs: usize) -> Result<(), TypeEnvFull> {
        self.types
            .try_reserve_exact(types)
            .map_err(|_| TypeEnvFull { types: true })?;
        self.digests
            .try_reserve_exact(types)
            .map_err(|_| TypeEnvFull { types: true })?;
        self.depths
            .try_reserve_exact(types)
            .map_err(|_| TypeEnvFull { types: true })?;
        self.type_index
            .try_reserve(types)
            .map_err(|_| TypeEnvFull { types: true })?;
        self.envs
            .try_reserve_exact(envs)
            .map_err(|_| TypeEnvFull { types: false })?;
        self.env_cache
            .try_reserve_exact(envs)
            .map_err(|_| TypeEnvFull { types: false })?;
        self.env_index
            .try_reserve(envs)
            .map_err(|_| TypeEnvFull { types: false })?;
        Ok(())
    }

    /// Prepare one bottom-up import without changing table contents.
    ///
    /// Each source type child names an earlier source entry. Each
    /// source environment names the source type table.
    pub fn prepare_import(
        &mut self,
        source_types: &[ClosedType],
        source_envs: &[TypeEnv],
    ) -> Result<TypeImportPlan, TypeEnvFull> {
        let mut type_map: Vec<ClosedTypeId> = Vec::new();
        type_map
            .try_reserve_exact(source_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;
        let mut new_types: Vec<(ClosedType, u32)> = Vec::new();
        let mut planned_types: HashMap<ClosedType, ClosedTypeId> = HashMap::new();
        planned_types
            .try_reserve(source_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;

        for source in source_types {
            let mapped = source.remap(|child| type_map[child as usize]);
            if let Some(id) = self.type_index.get(&mapped) {
                type_map.push(*id);
                continue;
            }
            if let Some(id) = planned_types.get(&mapped) {
                type_map.push(*id);
                continue;
            }
            let next = self
                .types
                .len()
                .checked_add(new_types.len())
                .and_then(|len| u32::try_from(len).ok())
                .ok_or(TypeEnvFull { types: true })?;
            if next >= self.max_types {
                return Err(TypeEnvFull { types: true });
            }
            let mut depth = 1u32;
            for child in mapped.children() {
                let child_depth = if (child as usize) < self.depths.len() {
                    self.depths[child as usize]
                } else {
                    let at = child as usize - self.depths.len();
                    new_types
                        .get(at)
                        .map(|entry| entry.1)
                        .ok_or(TypeEnvFull { types: true })?
                };
                depth = depth.max(
                    child_depth
                        .checked_add(1)
                        .ok_or(TypeEnvFull { types: true })?,
                );
            }
            if depth > MAX_CLOSED_DEPTH {
                return Err(TypeEnvFull { types: true });
            }
            planned_types.insert(mapped.clone(), next);
            new_types.push((mapped, depth));
            type_map.push(next);
        }

        let mut env_map: Vec<TypeEnvId> = Vec::new();
        env_map
            .try_reserve_exact(source_envs.len())
            .map_err(|_| TypeEnvFull { types: false })?;
        let mut new_envs: Vec<TypeEnv> = Vec::new();
        let mut planned_envs: HashMap<TypeEnv, TypeEnvId> = HashMap::new();
        planned_envs
            .try_reserve(source_envs.len())
            .map_err(|_| TypeEnvFull { types: false })?;

        for source in source_envs {
            let mut types = Vec::new();
            types
                .try_reserve_exact(source.types.len())
                .map_err(|_| TypeEnvFull { types: false })?;
            types.extend(source.types.iter().map(|ty| type_map[*ty as usize]));
            let mut rows = Vec::new();
            rows.try_reserve_exact(source.rows.len())
                .map_err(|_| TypeEnvFull { types: false })?;
            for source_row in &source.rows {
                let mut row = Vec::new();
                row.try_reserve_exact(source_row.len())
                    .map_err(|_| TypeEnvFull { types: false })?;
                row.extend_from_slice(source_row);
                rows.push(row);
            }
            let env = TypeEnv { types, rows };
            if env.is_empty() {
                env_map.push(TypeEnvId::EMPTY);
                continue;
            }
            if let Some(id) = self.env_index.get(&env) {
                env_map.push(*id);
                continue;
            }
            if let Some(id) = planned_envs.get(&env) {
                env_map.push(*id);
                continue;
            }
            let next = self
                .envs
                .len()
                .checked_add(new_envs.len())
                .and_then(|len| u32::try_from(len).ok())
                .ok_or(TypeEnvFull { types: false })?;
            if next >= self.max_envs {
                return Err(TypeEnvFull { types: false });
            }
            let id = TypeEnvId(next);
            planned_envs.insert(env.clone(), id);
            new_envs.push(env);
            env_map.push(id);
        }

        self.types
            .try_reserve_exact(new_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;
        self.digests
            .try_reserve_exact(new_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;
        self.depths
            .try_reserve_exact(new_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;
        self.type_index
            .try_reserve(new_types.len())
            .map_err(|_| TypeEnvFull { types: true })?;
        self.envs
            .try_reserve_exact(new_envs.len())
            .map_err(|_| TypeEnvFull { types: false })?;
        self.env_cache
            .try_reserve_exact(new_envs.len())
            .map_err(|_| TypeEnvFull { types: false })?;
        self.env_index
            .try_reserve(new_envs.len())
            .map_err(|_| TypeEnvFull { types: false })?;

        Ok(TypeImportPlan {
            types: new_types,
            envs: new_envs,
            type_index: planned_types,
            env_index: planned_envs,
            env_map,
            type_map,
        })
    }

    /// Commit one prepared import.
    pub fn commit_import(&mut self, plan: TypeImportPlan) {
        for (node, depth) in plan.types {
            self.types.push(node);
            self.digests.push(None);
            self.depths.push(depth);
        }
        for (node, id) in plan.type_index {
            self.type_index.insert(node, id);
        }
        for env in plan.envs {
            self.envs.push(env);
            self.env_cache.push(TypeEnvCache::default());
        }
        for (env, id) in plan.env_index {
            self.env_index.insert(env, id);
        }
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
        // The depth of this node is one past its deepest child. A walk
        // over a closed type costs at least its depth, so the bound
        // keeps every walk cheap and keeps a recursive walk inside the
        // Rust stack.
        let mut depth = 1u32;
        for child in node.children() {
            let Some(child_depth) = self.depths.get(child as usize) else {
                return Err(TypeEnvFull { types: true });
            };
            depth = depth.max(child_depth + 1);
        }
        if depth > MAX_CLOSED_DEPTH {
            return Err(TypeEnvFull { types: true });
        }
        let id = self.types.len() as ClosedTypeId;
        self.types.push(node.clone());
        self.digests.push(None);
        self.depths.push(depth);
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
        self.env_cache.push(TypeEnvCache::default());
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
        if let Some((cached_ty, cached_env, id)) = self.last_closed {
            if cached_ty == ty && cached_env == env {
                return Ok(id);
            }
        }
        if let Some(id) = self.closed.get(&(ty, env)).copied() {
            self.last_closed = Some((ty, env, id));
            return Ok(id);
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
        let id = match self.closed.get(&(ty, env)).copied() {
            Some(id) => id,
            None => self.intern(ClosedType::Unit)?,
        };
        self.last_closed = Some((ty, env, id));
        Ok(id)
    }

    /// Close one module type node whose children already closed.
    fn close_flat(
        &mut self,
        module: &Module,
        node: &BcType,
        env: TypeEnvId,
    ) -> Result<ClosedTypeId, TypeEnvFull> {
        let child = |table: &TypeEnvs, ty: u32| -> ClosedTypeId {
            table
                .closed
                .get(&(ty, env))
                .copied()
                .expect("the type walk closes every child first")
        };
        let built = match node {
            BcType::Unit => ClosedType::Unit,
            BcType::Bool => ClosedType::Bool,
            BcType::Int => ClosedType::Int,
            BcType::Str => ClosedType::Str,
            BcType::Fault => ClosedType::Fault,
            BcType::Request => ClosedType::Request,
            BcType::PolicyTable => ClosedType::PolicyTable,
            BcType::Vm => ClosedType::Vm,
            BcType::Digest => ClosedType::Digest,
            BcType::VmSnapshot => ClosedType::VmSnapshot,
            BcType::Bytes => ClosedType::Bytes,
            BcType::FileHandle => ClosedType::FileHandle,
            BcType::ResourceHandle => ClosedType::ResourceHandle,
            BcType::HostResource => ClosedType::HostResource,
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
            BcType::Projection {
                base,
                interface,
                assoc,
            } => {
                let base = child(self, *base);
                let Some((mut class, mut args)) = self.closed_instance(module, base) else {
                    return self.intern(ClosedType::Unit);
                };
                let mut steps = 0usize;
                loop {
                    if let Some(conformance) = module.conformances.iter().find(|item| {
                        item.class == class && item.application.interface == *interface
                    }) {
                        let Some(template) = conformance.associated.get(*assoc as usize) else {
                            return self.intern(ClosedType::Unit);
                        };
                        let owner = self.env_of(args, Vec::new())?;
                        return self.close(module, *template, owner);
                    }
                    steps += 1;
                    if steps > module.classes.len() {
                        return self.intern(ClosedType::Unit);
                    }
                    let Some(entry) = module.classes.get(class as usize) else {
                        return self.intern(ClosedType::Unit);
                    };
                    if entry.parent == NO_PARENT {
                        return self.intern(ClosedType::Unit);
                    }
                    let parent = entry.parent;
                    if !entry.parent_args.is_empty() {
                        let owner = self.env_of(args, Vec::new())?;
                        let mut parent_args = Vec::with_capacity(entry.parent_args.len());
                        for template in &entry.parent_args {
                            parent_args.push(self.close(module, *template, owner)?);
                        }
                        args = parent_args;
                    } else if module.classes[parent as usize].type_params == 0 {
                        args.clear();
                    }
                    class = parent;
                }
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
            BcType::Callback(params, muts, ret, row) => ClosedType::Callback(
                params.iter().map(|p| child(self, *p)).collect(),
                muts.clone(),
                child(self, *ret),
                self.close_row(module, row, env),
            ),
            BcType::Run(t) => ClosedType::Run(child(self, *t)),
            BcType::Wait(t) => ClosedType::Wait(child(self, *t)),
            BcType::RunSnapshot(t) => ClosedType::RunSnapshot(child(self, *t)),
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
        if let Some(cache) = self.env_cache.get(parent.0 as usize) {
            if let Ok(at) = cache
                .derived
                .binary_search_by_key(&app, |(entry, _)| *entry)
            {
                return Ok(cache.derived[at].1);
            }
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
        if self.cache_entries < self.max_envs {
            if let Some(cache) = self.env_cache.get_mut(parent.0 as usize) {
                if cache.derived.try_reserve(1).is_ok() {
                    let at = cache
                        .derived
                        .binary_search_by_key(&app, |(entry, _)| *entry)
                        .unwrap_or_else(|at| at);
                    cache.derived.insert(at, (app, id));
                    self.cache_entries += 1;
                }
            }
        }
        Ok(id)
    }

    /// Compose one generic method environment.
    ///
    /// The receiver supplies class arguments. `own` supplies method
    /// arguments. One canonical composition serves every activation.
    pub fn method_env(
        &mut self,
        module: &Module,
        callee: u32,
        class: u32,
        class_env: TypeEnvId,
        own: TypeEnvId,
    ) -> Result<TypeEnvId, TypeEnvFull> {
        let body = match module.funcs.get(callee as usize) {
            Some(body) => body,
            None => return Ok(own),
        };
        if body.type_params as usize == self.env(own).map(|env| env.types.len()).unwrap_or(0) {
            return Ok(own);
        }

        let key = (class, callee, own);
        if let Some(cache) = self.env_cache.get(class_env.0 as usize) {
            if let Ok(at) = cache
                .methods
                .binary_search_by_key(&key, |(entry, _)| *entry)
            {
                return Ok(cache.methods[at].1);
            }
        }

        let owner = match body
            .params
            .first()
            .and_then(|param| module.types.get(*param as usize))
        {
            Some(BcType::Class(owner)) | Some(BcType::Inst(owner, _)) => *owner,
            _ => return Ok(own),
        };
        let args = self
            .env(class_env)
            .map(|env| env.types.clone())
            .unwrap_or_default();
        let mut types = self
            .ancestor_args(module, class, &args, owner)
            .unwrap_or_default();
        let (own_types, own_rows) = self
            .env(own)
            .map(|env| (env.types.clone(), env.rows.clone()))
            .unwrap_or_default();
        types.extend(own_types);
        let composed = self.env_of(types, own_rows)?;

        if self.cache_entries < self.max_envs {
            if let Some(cache) = self.env_cache.get_mut(class_env.0 as usize) {
                if cache.methods.try_reserve(1).is_ok() {
                    let at = cache
                        .methods
                        .binary_search_by_key(&key, |(entry, _)| *entry)
                        .unwrap_or_else(|at| at);
                    cache.methods.insert(at, (key, composed));
                    self.cache_entries += 1;
                }
            }
        }
        Ok(composed)
    }

    /// Build the environment of one interface-selected class method.
    ///
    /// The instruction supplies the static receiver type. This type
    /// carries generic arguments even when the value representation does not.
    pub fn interface_method_env(
        &mut self,
        module: &Module,
        callee: u32,
        runtime_class: u32,
        receiver: ClosedTypeId,
    ) -> Result<Option<TypeEnvId>, TypeEnvFull> {
        let Some(body) = module.funcs.get(callee as usize) else {
            return Ok(None);
        };
        if body.type_params == 0 {
            return Ok(Some(TypeEnvId::EMPTY));
        }
        let role = |index: usize| {
            module
                .core_roles
                .get(index)
                .copied()
                .filter(|class| *class != crate::NO_ROLE)
        };
        let (class, args) = match self.ty(receiver).cloned() {
            Some(ClosedType::Class(class)) => (class, Vec::new()),
            Some(ClosedType::Inst(class, args)) => (class, args),
            Some(ClosedType::Unit) => match role(crate::corepin::ROLE_UNIT) {
                Some(class) => (class, Vec::new()),
                None => return Ok(None),
            },
            Some(ClosedType::Int) => match role(crate::corepin::ROLE_INT) {
                Some(class) => (class, Vec::new()),
                None => return Ok(None),
            },
            Some(ClosedType::Bool) => match role(crate::corepin::ROLE_BOOL) {
                Some(class) => (class, Vec::new()),
                None => return Ok(None),
            },
            Some(ClosedType::Str) => match role(crate::corepin::ROLE_STRING) {
                Some(class) => (class, Vec::new()),
                None => return Ok(None),
            },
            Some(ClosedType::Bytes) => match role(crate::corepin::ROLE_BYTES) {
                Some(class) => (class, Vec::new()),
                None => return Ok(None),
            },
            Some(ClosedType::List(element)) => match role(crate::corepin::ROLE_LIST) {
                Some(class) => (class, vec![element]),
                None => return Ok(None),
            },
            Some(ClosedType::Map(key, value)) => match role(crate::corepin::ROLE_MAP) {
                Some(class) => (class, vec![key, value]),
                None => return Ok(None),
            },
            Some(ClosedType::Tuple(args)) => {
                let Some(role_index) = crate::corepin::tuple_role(args.len()) else {
                    return Ok(None);
                };
                match role(role_index) {
                    Some(class) => (class, args),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        let runtime_args = if class == runtime_class {
            args.clone()
        } else {
            let Some(runtime) = module.classes.get(runtime_class as usize) else {
                return Ok(None);
            };
            if runtime.kind == BcClassKind::Case && runtime.type_params as usize == args.len() {
                args.clone()
            } else if runtime.type_params == 0 {
                Vec::new()
            } else {
                return Ok(None);
            }
        };
        if self.ancestor_args(module, runtime_class, &runtime_args, class) != Some(args) {
            return Ok(None);
        }
        let owner = match body
            .params
            .first()
            .and_then(|item| module.types.get(*item as usize))
        {
            Some(BcType::Class(owner) | BcType::Inst(owner, _)) => *owner,
            Some(BcType::Unit) => match role(crate::corepin::ROLE_UNIT) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Int) => match role(crate::corepin::ROLE_INT) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Bool) => match role(crate::corepin::ROLE_BOOL) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Str) => match role(crate::corepin::ROLE_STRING) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Bytes) => match role(crate::corepin::ROLE_BYTES) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::List(_)) => match role(crate::corepin::ROLE_LIST) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Map(_, _)) => match role(crate::corepin::ROLE_MAP) {
                Some(owner) => owner,
                None => return Ok(None),
            },
            Some(BcType::Tuple(items)) => {
                let Some(role_index) = crate::corepin::tuple_role(items.len()) else {
                    return Ok(None);
                };
                match role(role_index) {
                    Some(owner) => owner,
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };
        let types = match self.ancestor_args(module, runtime_class, &runtime_args, owner) {
            Some(types) => types,
            None => return Ok(None),
        };
        if types.len() != body.type_params as usize {
            return Ok(None);
        }
        self.env_of(types, Vec::new()).map(Some)
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

    /// Return the nominal class and arguments of one closed value type.
    fn closed_instance(
        &self,
        module: &Module,
        ty: ClosedTypeId,
    ) -> Option<(u32, Vec<ClosedTypeId>)> {
        let role = |index: usize| {
            module
                .core_roles
                .get(index)
                .copied()
                .filter(|class| *class != crate::NO_ROLE)
        };
        match self.ty(ty)? {
            ClosedType::Class(class) => Some((*class, Vec::new())),
            ClosedType::Inst(class, args) => Some((*class, args.clone())),
            ClosedType::Unit => Some((role(crate::corepin::ROLE_UNIT)?, Vec::new())),
            ClosedType::Int => Some((role(crate::corepin::ROLE_INT)?, Vec::new())),
            ClosedType::Bool => Some((role(crate::corepin::ROLE_BOOL)?, Vec::new())),
            ClosedType::Str => Some((role(crate::corepin::ROLE_STRING)?, Vec::new())),
            ClosedType::Bytes => Some((role(crate::corepin::ROLE_BYTES)?, Vec::new())),
            ClosedType::List(element) => Some((role(crate::corepin::ROLE_LIST)?, vec![*element])),
            ClosedType::Map(key, value) => {
                Some((role(crate::corepin::ROLE_MAP)?, vec![*key, *value]))
            }
            ClosedType::Tuple(items) => Some((
                role(crate::corepin::tuple_role(items.len())?)?,
                items.clone(),
            )),
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
                let env = self.env_of(cur_args.clone(), Vec::new()).ok()?;
                let mut out = Vec::with_capacity(entry.parent_args.len());
                for arg in entry.parent_args.clone() {
                    out.push(self.close(module, arg, env).ok()?);
                }
                cur_args = out;
            } else if module.classes.get(parent as usize)?.type_params == 0 {
                cur_args = Vec::new();
            }
            cur = parent;
        }
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
            ClosedType::List(e) | ClosedType::Run(e) | ClosedType::RunSnapshot(e) => {
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
            ClosedType::Callback(params, muts, ret, row) => {
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
pub fn bc_children(node: &BcType, out: &mut Vec<u32>) {
    match node {
        BcType::Inst(_, args) | BcType::Tuple(args) => out.extend(args),
        BcType::List(e)
        | BcType::Projection { base: e, .. }
        | BcType::Run(e)
        | BcType::Wait(e)
        | BcType::RunSnapshot(e)
        | BcType::Op(_, e) => out.push(*e),
        BcType::Map(a, b) | BcType::PendingCall(a, b) | BcType::Handle(a, b) => {
            out.push(*a);
            out.push(*b);
        }
        BcType::Fn(params, _, ret, _) | BcType::Callback(params, _, ret, _) => {
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
        ClosedType::Fault => 6,
        ClosedType::Request => 7,
        ClosedType::PolicyTable => 8,
        ClosedType::Vm => 9,
        ClosedType::Digest => 10,
        ClosedType::VmSnapshot => 11,
        ClosedType::Class(_) => 12,
        ClosedType::Inst(_, _) => 13,
        ClosedType::List(_) => 14,
        ClosedType::Map(_, _) => 15,
        ClosedType::Tuple(_) => 16,
        ClosedType::Fn(_, _, _, _) => 17,
        ClosedType::Run(_) => 18,
        ClosedType::PendingCall(_, _) => 19,
        ClosedType::Handle(_, _) => 20,
        ClosedType::Op(_, _) => 21,
        ClosedType::RunSnapshot(_) => 22,
        ClosedType::Bytes => 23,
        ClosedType::FileHandle => 24,
        ClosedType::ResourceHandle => 25,
        ClosedType::Wait(_) => 26,
        ClosedType::Callback(_, _, _, _) => 27,
        ClosedType::HostResource => 28,
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
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![]],
            imports: vec![],
            slots: vec![],
            core_roles: [crate::NO_ROLE; crate::CORE_ROLE_COUNT],
            classes: vec![BcClass {
                name: "C".to_string(),
                key: "C".to_string(),
                is_final: false,
                is_frozen: false,
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
            debug: Vec::new(),
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
    fn the_environment_cache_stops_retaining_entries_at_its_cap() {
        let mut m = module();
        m.apps.extend_from_within(..);
        m.apps.extend_from_within(..1);
        let mut table = TypeEnvs::new(64, 2);

        for app in 0..3 {
            table
                .derive(&m, TypeEnvId::EMPTY, app)
                .expect("the application derives");
        }
        assert_eq!(table.env_count(), 2);
        assert_eq!(table.cache_entries, 2);
        assert_eq!(table.env_cache[0].derived.len(), 2);

        table
            .derive(&m, TypeEnvId::EMPTY, 2)
            .expect("an uncached application still derives");
        assert_eq!(table.cache_entries, 2);
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
    fn a_closed_type_past_the_depth_bound_rejects() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut m = module();
                // `[[[ ... [Int] ... ]]]`, one level past the bound.
                m.types = vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
                let mut deep = 2u32;
                for _ in 0..MAX_CLOSED_DEPTH {
                    m.types.push(BcType::List(deep));
                    deep = (m.types.len() - 1) as u32;
                }
                let mut table = TypeEnvs::new(u32::MAX, u32::MAX);
                assert_eq!(
                    table.close(&m, deep, TypeEnvId::EMPTY),
                    Err(TypeEnvFull { types: true })
                );
            })
            .expect("thread starts")
            .join()
            .expect("no Rust stack overflow");
    }

    #[test]
    fn a_closed_type_at_the_depth_bound_digests() {
        let mut m = module();
        // `Int` is depth 1, so `MAX_CLOSED_DEPTH - 1` list levels
        // reach the bound exactly.
        m.types = vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
        let mut deep = 2u32;
        for _ in 0..(MAX_CLOSED_DEPTH - 1) {
            m.types.push(BcType::List(deep));
            deep = (m.types.len() - 1) as u32;
        }
        let mut table = TypeEnvs::new(u32::MAX, u32::MAX);
        let closed = table.close(&m, deep, TypeEnvId::EMPTY).expect("closed");
        let digest = table.digest(&m, &[], closed);
        assert_ne!(digest, [0u8; 32]);
        // The cached answer is the same answer.
        assert_eq!(table.digest(&m, &[], closed), digest);
    }

    #[test]
    fn a_canonical_row_sorts_by_text_and_dedups() {
        let m = module();
        // Slot 1 is `Fs` and slot 0 is `Io`, so the text order swaps
        // the slot order.
        assert_eq!(canonical_row(&m, vec![0, 1, 0]), vec![1, 0]);
    }
}
