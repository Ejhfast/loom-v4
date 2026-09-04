//! Arena publication and namespace views.

use crate::collect;
use crate::env::{
    artifact_from_order, collect_environment_with_root, fail, link_order, resolve_artifact,
    validate_untrusted_units, DefinitionSelection, FrozenLinkEnv, LinkError,
};
use crate::reloc_tables::{CodeRelocation, Reloc, UnitRelocation};
use crate::relocate::{bind_unit, merge_unit, relocated_exports, seed_extension_providers};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use lm_bytecode::artifact::{Artifact, ArtifactId, LinkUnit, CORE_MODULE_PATH};
use lm_bytecode::identity::ModuleIdentity;
use lm_bytecode::{
    BcClass, BcClassKind, BcConformance, BcInterface, BcInterfaceUse, BcRow, BcType, CodeTable,
    CodeTables, Export, Func, FuncBinding, Module, ReflectionModule, SlotSpec, SlotTarget, TypeApp,
    NO_CLASS,
};

/// One resolved code namespace.
///
/// The namespace owns relocated tables and one exact artifact graph.
/// A VM executes this value. It never executes a `Module` payload.
#[derive(Debug, Clone)]
pub struct CodeNamespace {
    artifact_id: ArtifactId,
    artifacts: Vec<std::sync::Arc<Artifact>>,
    pub(crate) units: BTreeMap<ArtifactId, std::sync::Arc<LinkUnit>>,
    pub(crate) active_units: BTreeMap<String, ArtifactId>,
    pub(crate) relocations: BTreeMap<ArtifactId, UnitRelocation>,
    functions: Arc<[u64]>,
    classes: Arc<[u64]>,
    slots: Arc<[u64]>,
    core_artifact: Option<ArtifactId>,
    tables: std::sync::Arc<CodeTables>,
    dispatch: Arc<CodeTable<DispatchRow>>,
    entry: u32,
    core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    core: lm_bytecode::corepin::CoreLayout,
    exports: Vec<Export>,
    bindings: Arc<[FuncBinding]>,
    identity: Arc<ModuleIdentity>,
    closure_bodies: Arc<std::sync::OnceLock<Vec<bool>>>,
    slot_initials: Arc<[Option<SlotTarget>]>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
    /// True when these indices match a clean replay of this chain.
    canonical_layout: bool,
}

impl CodeNamespace {
    pub fn artifact_id(&self) -> ArtifactId {
        self.artifact_id
    }

    pub fn artifact(&self) -> &Artifact {
        &self.artifacts[0]
    }

    pub fn artifacts(&self) -> &[std::sync::Arc<Artifact>] {
        &self.artifacts
    }

    pub fn unit(&self, id: ArtifactId) -> Option<&LinkUnit> {
        self.units.get(&id).map(std::sync::Arc::as_ref)
    }

    pub fn active_unit(&self, path: &str) -> Option<&LinkUnit> {
        let id = self.active_units.get(path)?;
        self.unit(*id)
    }

    pub fn active_unit_store(&self, path: &str) -> Option<Arc<LinkUnit>> {
        let id = self.active_units.get(path)?;
        self.units.get(id).cloned()
    }

    pub fn relocation(&self, id: ArtifactId) -> Option<&UnitRelocation> {
        self.relocations.get(&id)
    }

    pub fn contains_function(&self, function: u32) -> bool {
        contains_index(&self.functions, function)
    }

    pub fn contains_class(&self, class: u32) -> bool {
        contains_index(&self.classes, class)
    }

    pub fn contains_slot(&self, slot: u32) -> bool {
        contains_index(&self.slots, slot)
    }

    pub fn core_artifact(&self) -> Option<ArtifactId> {
        self.core_artifact
    }

    pub fn tables(&self) -> &CodeTables {
        &self.tables
    }

    pub fn table_store(&self) -> std::sync::Arc<CodeTables> {
        self.tables.clone()
    }

    pub fn dispatch_store(&self) -> Arc<CodeTable<DispatchRow>> {
        self.dispatch.clone()
    }

    pub fn entry(&self) -> u32 {
        self.entry
    }

    pub fn core_roles(&self) -> &[u32; lm_bytecode::CORE_ROLE_COUNT] {
        &self.core_roles
    }

    pub fn core_layout(&self) -> &lm_bytecode::corepin::CoreLayout {
        &self.core
    }

    pub fn exports(&self) -> &[Export] {
        &self.exports
    }

    pub fn bindings(&self) -> &[FuncBinding] {
        &self.bindings
    }

    pub fn binding_store(&self) -> Arc<[FuncBinding]> {
        self.bindings.clone()
    }

    pub fn class_hashes(&self) -> &[[u8; 32]] {
        &self.identity.class_hashes
    }

    pub fn interface_hashes(&self) -> &[[u8; 32]] {
        &self.identity.interface_hashes
    }

    pub fn func_hashes(&self) -> &[[u8; 32]] {
        &self.identity.func_hashes
    }

    pub fn type_hashes(&self) -> &[[u8; 32]] {
        &self.identity.type_hashes
    }

    pub fn identity_store(&self) -> Arc<ModuleIdentity> {
        self.identity.clone()
    }

    pub fn closure_body_store(&self) -> Arc<std::sync::OnceLock<Vec<bool>>> {
        self.closure_bodies.clone()
    }

    pub fn slot_initials(&self) -> &[Option<SlotTarget>] {
        &self.slot_initials
    }

    pub fn slot_initial_store(&self) -> Arc<[Option<SlotTarget>]> {
        self.slot_initials.clone()
    }

    /// Build exact table maps into another publication of this graph.
    pub fn relocation_to(&self, target: &CodeNamespace) -> Result<CodeRelocation, LinkError> {
        let mut maps = CodeRelocation::with_source(self.tables.as_ref());
        for (id, source) in &self.relocations {
            let target = target
                .relocations
                .get(id)
                .ok_or_else(|| fail(format!("the target namespace lacks unit {id}")))?;
            maps.merge_unit(source, target)?;
        }
        Ok(maps)
    }

    pub fn bundle(&self) -> &std::sync::Arc<lm_abi::AbiBundle> {
        &self.bundle
    }

    /// True when this namespace already uses its portable table layout.
    pub fn has_canonical_layout(&self) -> bool {
        self.canonical_layout
    }

    /// Build one portable artifact for an arena function.
    pub fn function_artifact(&self, function: u32) -> Result<Artifact, LinkError> {
        let (unit, local) = self.function_unit(function)?;
        let (export, definition) =
            prepare_definition_export(unit.module(), DefinitionSelection::Function(local))?;
        self.build_definition_artifact(unit, export, definition)
    }

    /// Build one portable artifact for an arena class.
    pub fn class_artifact(&self, class: u32) -> Result<Artifact, LinkError> {
        let (unit, local) = self.local_class(class)?;
        let (export, definition) =
            prepare_definition_export(unit.module(), DefinitionSelection::Class(local))?;
        self.build_definition_artifact(unit, export, definition)
    }

    /// Return the source unit and local index of one arena function.
    pub fn function_unit(&self, function: u32) -> Result<(&LinkUnit, u32), LinkError> {
        for (id, unit) in &self.units {
            let Some(reloc) = self.relocations.get(id) else {
                continue;
            };
            let externs = unit.module().extern_funcs();
            for (local, target) in reloc.functions().iter().copied().enumerate() {
                if target == function && !externs[local] {
                    return Ok((unit, local as u32));
                }
            }
        }
        Err(fail("the function has no artifact unit"))
    }

    fn local_class(&self, class: u32) -> Result<(&LinkUnit, u32), LinkError> {
        for (id, unit) in &self.units {
            let Some(reloc) = self.relocations.get(id) else {
                continue;
            };
            let externs = unit.module().extern_classes();
            for (local, target) in reloc.classes().iter().copied().enumerate() {
                if target == class && !externs[local] {
                    return Ok((unit, local as u32));
                }
            }
        }
        Err(fail("the class has no artifact unit"))
    }

    fn build_definition_artifact(
        &self,
        source: &LinkUnit,
        export: Export,
        definition: collect::DefinitionRoot,
    ) -> Result<Artifact, LinkError> {
        let mut units = BTreeMap::new();
        for (path, id) in &self.active_units {
            let unit = self
                .units
                .get(id)
                .cloned()
                .ok_or_else(|| fail("the portable unit dependency is missing"))?;
            units.insert(path.clone(), unit);
        }
        let env = FrozenLinkEnv { units };
        build_definition_artifact(source, export, definition, &env, &self.bundle)
    }

    /// Check the root entry against one expected result and effect row.
    pub fn expect_entry(&self, result: &BcType, row: &[&str]) -> Result<(), LinkError> {
        let func = &self.tables.funcs[self.entry as usize];
        let found = &self.tables.types[func.ret as usize];
        if found != result {
            return Err(fail(format!(
                "the entry returns {found:?}, and the caller expects {result:?}"
            )));
        }
        let mut names: Vec<&str> = func
            .row
            .iter()
            .map(|element| match element {
                BcRow::Op(index) => self.bundle.op_name(*index).unwrap_or("?"),
                BcRow::Group(index) => self.bundle.group_name(*index).unwrap_or("?"),
                BcRow::Var(_) => "?",
            })
            .collect();
        names.sort_unstable();
        let mut wanted = row.to_vec();
        wanted.sort_unstable();
        if names != wanted {
            return Err(fail(format!(
                "the entry charges [{}], and the caller expects [{}]",
                names.join(", "),
                wanted.join(", ")
            )));
        }
        Ok(())
    }
}

const NO_METHOD: u32 = u32::MAX;

/// One sparse default-method witness.
#[derive(Debug, Clone)]
struct InterfaceWitness {
    interface: u32,
    conformance: u32,
    method_overrides: Arc<[bool]>,
}

/// The sealed dispatch row of one class.
#[derive(Debug, Clone, Default)]
pub struct DispatchRow {
    base: u32,
    table: Vec<u32>,
    interface_witnesses: Option<Arc<[InterfaceWitness]>>,
}

impl DispatchRow {
    /// Return the first selector stored in this row.
    #[inline]
    pub fn base(&self) -> u32 {
        self.base
    }

    /// Return the dense method cells in this row.
    #[inline]
    pub fn cells(&self) -> &[u32] {
        &self.table
    }

    #[inline]
    pub fn method(&self, selector: u32) -> Option<u32> {
        let offset = selector.checked_sub(self.base)? as usize;
        match self.table.get(offset).copied() {
            Some(NO_METHOD) | None => None,
            Some(function) => Some(function),
        }
    }

    #[inline]
    pub fn interface_witness(&self, interface: u32, method: u32) -> Option<(bool, u32)> {
        let witnesses = self.interface_witnesses.as_deref()?;
        let witness = witnesses
            .binary_search_by_key(&interface, |witness| witness.interface)
            .ok()
            .map(|index| &witnesses[index])?;
        witness
            .method_overrides
            .get(method as usize)
            .copied()
            .map(|overrides| (overrides, witness.conformance))
    }

    #[inline]
    pub fn cell_count(&self) -> usize {
        self.table.len()
    }

    #[inline]
    pub fn witness_count(&self) -> usize {
        self.interface_witnesses.as_deref().map_or(0, <[_]>::len)
    }
}

pub(crate) fn prepare_definition_export(
    source: &Module,
    selection: DefinitionSelection,
) -> Result<(Export, collect::DefinitionRoot), LinkError> {
    let (export, definition) = match selection {
        DefinitionSelection::Function(function) => {
            let export = source
                .exports
                .iter()
                .find(|export| {
                    export.kind == lm_bytecode::ExportKind::Function && export.def == function
                })
                .cloned()
                .or_else(|| {
                    let binding = source
                        .bindings
                        .iter()
                        .find(|binding| binding.class == NO_CLASS && binding.func == function)?;
                    Some(Export {
                        kind: lm_bytecode::ExportKind::Function,
                        name: binding
                            .key
                            .rsplit_once('.')
                            .map_or(binding.key.clone(), |(_, name)| name.to_string()),
                        source: false,
                        def: function,
                        ctor: lm_bytecode::NO_CTOR,
                        constant: None,
                    })
                })
                .ok_or_else(|| fail("the function has no portable export"))?;
            (export, collect::DefinitionRoot::Function(function))
        }
        DefinitionSelection::Class(class) => {
            let constructor = source
                .slots
                .iter()
                .find_map(|slot| match slot.initial {
                    Some(SlotTarget::Class {
                        class: candidate,
                        constructor,
                    }) if candidate == class => Some(constructor),
                    _ => None,
                })
                .unwrap_or(lm_bytecode::NO_CTOR);
            let class_def = source
                .classes
                .get(class as usize)
                .ok_or_else(|| fail("the portable class is missing"))?;
            if class_def.has_init && constructor == lm_bytecode::NO_CTOR {
                return Err(fail("the class has no portable constructor binding"));
            }
            let kind = match class_def.kind {
                BcClassKind::Normal => lm_bytecode::ExportKind::Class,
                BcClassKind::Abstract => lm_bytecode::ExportKind::Enum,
                BcClassKind::Case => lm_bytecode::ExportKind::EnumCase,
            };
            let export = source
                .exports
                .iter()
                .find(|export| export.kind.is_class() && export.def == class)
                .cloned()
                .unwrap_or_else(|| Export {
                    kind,
                    name: class_def.name.clone(),
                    source: false,
                    def: class,
                    ctor: constructor,
                    constant: None,
                });
            (export, collect::DefinitionRoot::Class(class))
        }
    };
    Ok((export, definition))
}

pub(crate) fn build_definition_artifact(
    source: &LinkUnit,
    export: Export,
    definition: collect::DefinitionRoot,
    env: &FrozenLinkEnv,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Artifact, LinkError> {
    let selected = collect_environment_with_root(
        source.module_path(),
        env,
        bundle,
        Some((definition, export)),
    )?;
    let order = link_order(source.module_path(), &selected)?;
    artifact_from_order(source.module_path(), &selected, &order, false)
}

/// The stable index of one published namespace in a world arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespaceId(u32);

impl NamespaceId {
    pub const ROOT: NamespaceId = NamespaceId(0);

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// The published code namespaces of one world.
#[derive(Debug, Clone)]
pub struct CodeArena {
    merged: Arc<Merged>,
    namespaces: Arc<Vec<Arc<CodeNamespace>>>,
    by_artifact: Arc<HashMap<ArtifactId, NamespaceId>>,
    by_chain: Arc<HashMap<Vec<ArtifactId>, NamespaceId>>,
    verified: Arc<BTreeSet<ArtifactId>>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
}

impl Default for CodeArena {
    fn default() -> CodeArena {
        CodeArena::new()
    }
}

impl CodeArena {
    pub fn new() -> CodeArena {
        CodeArena::with_bundle(lm_abi::standard_bundle())
    }

    pub fn with_bundle(bundle: std::sync::Arc<lm_abi::AbiBundle>) -> CodeArena {
        CodeArena {
            merged: Arc::new(Merged::default()),
            namespaces: Arc::new(Vec::new()),
            by_artifact: Arc::new(HashMap::new()),
            by_chain: Arc::new(HashMap::new()),
            verified: Arc::new(BTreeSet::new()),
            bundle,
        }
    }

    /// Resolve, verify, relocate, and publish one artifact.
    pub fn publish(
        &mut self,
        artifact: Artifact,
        runtime_core: Option<Arc<LinkUnit>>,
    ) -> Result<NamespaceId, LinkError> {
        let id = artifact.id();
        if let Some(namespace) = self.by_artifact.get(&id) {
            return Ok(*namespace);
        }
        let canonical_layout = self.namespaces.is_empty();
        let retained = std::sync::Arc::new(artifact.clone());
        let root_path = artifact.root().module_path().to_string();
        let untrusted: BTreeSet<ArtifactId> =
            artifact.units().iter().map(|unit| unit.id()).collect();
        let (root, env) = resolve_artifact(artifact, runtime_core)
            .map_err(|error| fail(format!("the artifact does not resolve: {error}")))?;
        let root_unit = env
            .unit(&root_path)
            .ok_or_else(|| fail(format!("the artifact root `{root_path}` is not bound")))?;
        if root_unit.id() != root {
            return Err(fail(format!(
                "the artifact root `{root_path}` has another identity"
            )));
        }
        let unchecked: BTreeSet<ArtifactId> =
            untrusted.difference(&self.verified).copied().collect();
        validate_untrusted_units(&env, &unchecked, &self.bundle)?;

        let order = link_order(&root_path, &env)?;
        let slot_scope = env
            .unit(CORE_MODULE_PATH)
            .map(LinkUnit::id)
            .ok_or_else(|| fail("the artifact has no core dependency"))?;
        let mut merged = self.merged.as_ref().clone();
        let mut view = NamespaceBuild::default();
        let mut entry = None;
        let mut root_exports = Vec::new();
        let mut relocations = BTreeMap::new();
        let mut units = BTreeMap::new();
        let mut active_units = BTreeMap::new();
        for path in &order {
            let unit = env
                .unit(path)
                .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
            let reloc = match merged.units.get(&unit.id()).cloned() {
                Some(reloc) => {
                    bind_unit(&mut view, &merged, unit, path, &reloc)?;
                    reloc
                }
                None => {
                    let reloc =
                        merge_unit(&mut merged, &mut view, unit, path, slot_scope, &self.bundle)?;
                    Arc::make_mut(&mut merged.units).insert(unit.id(), reloc.clone());
                    reloc
                }
            };
            relocations.insert(unit.id(), UnitRelocation(reloc.clone()));
            units.insert(
                unit.id(),
                env.unit_store(path)
                    .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?,
            );
            active_units.insert(path.clone(), unit.id());
            if path == &root_path {
                entry = reloc.funcs.get(unit.module().entry as usize).copied();
                root_exports = relocated_exports(unit.module(), &reloc)?;
            }
        }
        let entry = entry.ok_or_else(|| fail("the artifact root has no entry"))?;
        let core_artifact = active_units.get(CORE_MODULE_PATH).copied();
        view.slot_initials.resize(merged.slots.len(), None);
        extend_dispatch(&mut merged);
        let tables = Arc::new(tables_of(&merged));
        let dispatch = Arc::new(merged.dispatch.clone());
        let identity = Arc::new(namespace_identity(&merged, root));
        let [functions, classes, slots] = namespace_membership(&relocations);
        let namespace = CodeNamespace {
            artifact_id: root,
            artifacts: vec![retained],
            units,
            active_units,
            relocations,
            functions,
            classes,
            slots,
            core_artifact,
            tables,
            dispatch,
            entry,
            core_roles: view.core_roles,
            core: lm_bytecode::corepin::layout_from_roles(&view.core_roles),
            exports: root_exports,
            bindings: view.bindings.into(),
            identity,
            closure_bodies: Arc::new(std::sync::OnceLock::new()),
            slot_initials: view.slot_initials.into(),
            bundle: self.bundle.clone(),
            canonical_layout,
        };
        let index = u32::try_from(self.namespaces.len())
            .map_err(|_| fail("the world has too many code namespaces"))?;
        let id = NamespaceId(index);
        Arc::make_mut(&mut self.namespaces).push(std::sync::Arc::new(namespace));
        let published = id_of(&self.namespaces[id.index()]);
        Arc::make_mut(&mut self.by_artifact).insert(published, id);
        Arc::make_mut(&mut self.by_chain).insert(vec![published], id);
        self.merged = Arc::new(merged);
        Arc::make_mut(&mut self.verified).extend(unchecked);
        Ok(id)
    }

    /// Replay one verified namespace into this arena.
    ///
    /// The source namespace proves each artifact unit. This operation
    /// repeats linking, but it does not repeat bytecode verification.
    pub fn replay_namespace(&mut self, source: &CodeNamespace) -> Result<NamespaceId, LinkError> {
        let chain: Vec<ArtifactId> = source
            .artifacts
            .iter()
            .map(|artifact| artifact.id())
            .collect();
        if let Some(namespace) = self.by_chain.get(&chain) {
            return Ok(*namespace);
        }
        let mut artifacts = source.artifacts.iter();
        let root = artifacts
            .next()
            .ok_or_else(|| fail("the source namespace has no root artifact"))?;
        let core = source
            .core_artifact
            .and_then(|id| source.units.get(&id).cloned());
        let mut namespace = self.publish_verified(root.as_ref().clone(), core)?;
        for artifact in artifacts {
            namespace = self.extend_verified(namespace, artifact.as_ref().clone())?;
        }
        Ok(namespace)
    }

    /// Publish units that already passed the bytecode verifier.
    ///
    /// Use this path only for exact compiler output or a verified
    /// namespace replay.
    pub fn publish_verified(
        &mut self,
        artifact: Artifact,
        runtime_core: Option<Arc<LinkUnit>>,
    ) -> Result<NamespaceId, LinkError> {
        let units: BTreeSet<ArtifactId> = artifact.units().iter().map(|unit| unit.id()).collect();
        Arc::make_mut(&mut self.verified).extend(units);
        self.publish(artifact, runtime_core)
    }

    /// Extend a namespace with units that already passed verification.
    fn extend_verified(
        &mut self,
        base: NamespaceId,
        artifact: Artifact,
    ) -> Result<NamespaceId, LinkError> {
        let units: BTreeSet<ArtifactId> = artifact.units().iter().map(|unit| unit.id()).collect();
        Arc::make_mut(&mut self.verified).extend(units);
        self.extend(base, artifact)
    }

    /// Extend one namespace with one exact artifact graph.
    ///
    /// The new namespace keeps the base entry and core. Existing
    /// namespaces remain immutable.
    pub fn extend(
        &mut self,
        base: NamespaceId,
        artifact: Artifact,
    ) -> Result<NamespaceId, LinkError> {
        let base = self
            .namespace(base)
            .cloned()
            .ok_or_else(|| fail("the base code namespace is missing"))?;
        let mut chain: Vec<ArtifactId> = base.artifacts.iter().map(|item| item.id()).collect();
        if !chain.contains(&artifact.id()) {
            chain.push(artifact.id());
        }
        if let Some(namespace) = self.by_chain.get(&chain) {
            return Ok(*namespace);
        }
        let runtime_core = base
            .core_artifact
            .and_then(|id| base.units.get(&id).cloned());
        let retained = Arc::new(artifact.clone());
        let untrusted: BTreeSet<ArtifactId> =
            artifact.units().iter().map(|unit| unit.id()).collect();
        let root_path = artifact.root().module_path().to_string();
        let (root, env) = resolve_artifact(artifact, runtime_core)
            .map_err(|error| fail(format!("the artifact does not resolve: {error}")))?;
        let root_unit = env
            .unit(&root_path)
            .ok_or_else(|| fail(format!("the artifact root `{root_path}` is not bound")))?;
        if root_unit.id() != root {
            return Err(fail(format!(
                "the artifact root `{root_path}` has another identity"
            )));
        }
        let graph_core = env.unit(CORE_MODULE_PATH).map(LinkUnit::id);
        if graph_core != base.core_artifact {
            return Err(fail("installed code needs another core artifact"));
        }
        let unchecked: BTreeSet<ArtifactId> =
            untrusted.difference(&self.verified).copied().collect();
        validate_untrusted_units(&env, &unchecked, &self.bundle)?;

        let order = link_order(&root_path, &env)?;
        let slot_scope = graph_core.ok_or_else(|| fail("the artifact has no core dependency"))?;
        let canonical_layout =
            base.canonical_layout && merged_matches_namespace(self.merged.as_ref(), base.as_ref());
        let mut merged = self.merged.as_ref().clone();
        let mut addition = NamespaceBuild::default();
        let replaced_paths: BTreeSet<String> = order.iter().cloned().collect();
        seed_extension_providers(&mut addition, base.as_ref(), &replaced_paths)?;
        let mut relocations = base.relocations.clone();
        let mut units = base.units.clone();
        let mut active_units = base.active_units.clone();
        for path in &order {
            let unit = env
                .unit(path)
                .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
            let reloc = match merged.units.get(&unit.id()).cloned() {
                Some(reloc) => {
                    bind_unit(&mut addition, &merged, unit, path, &reloc)?;
                    reloc
                }
                None => {
                    let reloc = merge_unit(
                        &mut merged,
                        &mut addition,
                        unit,
                        path,
                        slot_scope,
                        &self.bundle,
                    )?;
                    Arc::make_mut(&mut merged.units).insert(unit.id(), reloc.clone());
                    reloc
                }
            };
            relocations.insert(unit.id(), UnitRelocation(reloc.clone()));
            units.insert(
                unit.id(),
                env.unit_store(path)
                    .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?,
            );
            active_units.insert(path.clone(), unit.id());
        }

        let mut slot_initials = base.slot_initials.to_vec();
        slot_initials.resize(merged.slots.len(), None);
        addition.slot_initials.resize(merged.slots.len(), None);
        for (index, initial) in addition.slot_initials.into_iter().enumerate() {
            if slot_initials[index].is_none() {
                slot_initials[index] = initial;
            }
        }
        let mut binding_by_key: BTreeMap<String, FuncBinding> = base
            .bindings
            .iter()
            .cloned()
            .map(|binding| (binding.key.clone(), binding))
            .collect();
        for binding in addition.bindings {
            binding_by_key.entry(binding.key.clone()).or_insert(binding);
        }
        let mut artifacts = base.artifacts.clone();
        if !artifacts.iter().any(|item| item.id() == retained.id()) {
            artifacts.push(retained);
        }
        extend_dispatch(&mut merged);
        let tables = Arc::new(tables_of(&merged));
        let dispatch = Arc::new(merged.dispatch.clone());
        let identity = Arc::new(namespace_identity(&merged, base.artifact_id));
        let [functions, classes, slots] = namespace_membership(&relocations);
        let namespace = CodeNamespace {
            artifact_id: base.artifact_id,
            artifacts,
            units,
            active_units,
            relocations,
            functions,
            classes,
            slots,
            core_artifact: base.core_artifact,
            tables,
            dispatch,
            entry: base.entry,
            core_roles: base.core_roles,
            core: base.core,
            exports: base.exports.clone(),
            bindings: binding_by_key.into_values().collect::<Vec<_>>().into(),
            identity,
            closure_bodies: Arc::new(std::sync::OnceLock::new()),
            slot_initials: slot_initials.into(),
            bundle: self.bundle.clone(),
            canonical_layout,
        };
        let index = u32::try_from(self.namespaces.len())
            .map_err(|_| fail("the world has too many code namespaces"))?;
        let id = NamespaceId(index);
        Arc::make_mut(&mut self.namespaces).push(Arc::new(namespace));
        Arc::make_mut(&mut self.by_chain).insert(chain, id);
        self.merged = Arc::new(merged);
        Arc::make_mut(&mut self.verified).extend(unchecked);
        Ok(id)
    }

    pub fn namespace(&self, id: NamespaceId) -> Option<&std::sync::Arc<CodeNamespace>> {
        self.namespaces.get(id.index())
    }

    pub fn namespace_count(&self) -> usize {
        self.namespaces.len()
    }
}

fn id_of(namespace: &CodeNamespace) -> ArtifactId {
    namespace.artifact_id()
}

fn namespace_membership(relocations: &BTreeMap<ArtifactId, UnitRelocation>) -> [Arc<[u64]>; 3] {
    let mut functions = Vec::new();
    let mut classes = Vec::new();
    let mut slots = Vec::new();
    for relocation in relocations.values() {
        mark_indices(&mut functions, relocation.functions());
        mark_indices(&mut classes, relocation.classes());
        mark_indices(&mut slots, relocation.slots());
    }
    [functions.into(), classes.into(), slots.into()]
}

pub(crate) fn mark_indices(bits: &mut Vec<u64>, indices: &[u32]) {
    for &index in indices {
        let word = index as usize / u64::BITS as usize;
        if bits.len() <= word {
            bits.resize(word + 1, 0);
        }
        bits[word] |= 1 << (index % u64::BITS);
    }
}

pub(crate) fn contains_index(bits: &[u64], index: u32) -> bool {
    let word = index as usize / u64::BITS as usize;
    bits.get(word)
        .is_some_and(|bits| bits & (1 << (index % u64::BITS)) != 0)
}

/// Test whether an extension starts at the exact namespace prefix.
fn merged_matches_namespace(merged: &Merged, namespace: &CodeNamespace) -> bool {
    let tables = namespace.tables();
    merged.strings.len() == tables.strings.len()
        && merged.bytes.len() == tables.bytes.len()
        && merged.types.len() == tables.types.len()
        && merged.selectors.len() == tables.selectors.len()
        && merged.apps.len() == tables.apps.len()
        && merged.classes.len() == tables.classes.len()
        && merged.class_bounds.len() == tables.class_bounds.len()
        && merged.interfaces.len() == tables.interfaces.len()
        && merged.conformances.len() == tables.conformances.len()
        && merged.funcs.len() == tables.funcs.len()
        && merged.func_bounds.len() == tables.func_bounds.len()
        && merged.slots.len() == tables.slots.len()
        && merged.reflections.len() == tables.reflections.len()
        && merged.dispatch.len() == namespace.dispatch.len()
}

fn namespace_identity(merged: &Merged, artifact: ArtifactId) -> ModuleIdentity {
    ModuleIdentity {
        class_hashes: merged.class_hashes.to_vec(),
        func_hashes: merged.func_hashes.to_vec(),
        interface_hashes: merged.interface_hashes.to_vec(),
        type_hashes: merged.type_hashes.to_vec(),
        semantic_hash: artifact.into_bytes(),
        max_refine_rounds: 0,
    }
}

/// The append-only dense tables of one code arena.
type SlotContractKey = (ArtifactId, [u8; 32], [u8; 32]);

#[derive(Debug, Clone, Default)]
pub(crate) struct Merged {
    pub(crate) strings: CodeTable<String>,
    pub(crate) string_index: Arc<HashMap<String, u32>>,
    pub(crate) bytes: CodeTable<Vec<u8>>,
    pub(crate) bytes_index: Arc<HashMap<Vec<u8>, u32>>,
    pub(crate) types: CodeTable<BcType>,
    pub(crate) type_hashes: CodeTable<[u8; 32]>,
    pub(crate) type_index: Arc<HashMap<BcType, u32>>,
    pub(crate) selectors: CodeTable<String>,
    pub(crate) selector_index: Arc<HashMap<String, u32>>,
    pub(crate) apps: CodeTable<TypeApp>,
    pub(crate) app_index: Arc<HashMap<TypeApp, u32>>,
    pub(crate) classes: CodeTable<BcClass>,
    pub(crate) class_hashes: CodeTable<[u8; 32]>,
    pub(crate) class_bounds: CodeTable<Vec<Vec<BcInterfaceUse>>>,
    pub(crate) interfaces: CodeTable<BcInterface>,
    pub(crate) interface_hashes: CodeTable<[u8; 32]>,
    pub(crate) conformances: CodeTable<BcConformance>,
    pub(crate) funcs: CodeTable<Func>,
    pub(crate) func_hashes: CodeTable<[u8; 32]>,
    pub(crate) func_bounds: CodeTable<Vec<Vec<BcInterfaceUse>>>,
    /// One sealed dispatch row for each arena class.
    pub(crate) dispatch: CodeTable<DispatchRow>,
    /// Late-bound slot contracts, merged by stable key and contract.
    pub(crate) slots: CodeTable<SlotSpec>,
    pub(crate) slot_by_contract: Arc<HashMap<SlotContractKey, u32>>,
    /// Exact source module surfaces used by reflection.
    pub(crate) reflections: CodeTable<ReflectionModule>,
    /// Optional source data after table relocation.
    pub(crate) debug: Arc<lm_bytecode::debug::DebugInfo>,
    /// One permanent relocation for each exact unit.
    pub(crate) units: Arc<HashMap<ArtifactId, Reloc>>,
}

/// One artifact graph's bindings over arena indices.
#[derive(Debug, Clone)]
pub(crate) struct NamespaceBuild {
    pub(crate) core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    pub(crate) class_version: HashMap<String, (u32, [u8; 32], String)>,
    pub(crate) interface_by_key: HashMap<String, (u32, String)>,
    pub(crate) bindings: Vec<lm_bytecode::FuncBinding>,
    pub(crate) binding_version: HashMap<String, ([u8; 32], String)>,
    pub(crate) class_exports: HashMap<(String, String), u32>,
    pub(crate) interface_exports: HashMap<(String, String), u32>,
    pub(crate) func_exports: HashMap<(String, String), u32>,
    pub(crate) ctor_exports: HashMap<(String, String), u32>,
    pub(crate) export_hash: HashMap<(String, String), [u8; 32]>,
    pub(crate) slot_initials: Vec<Option<SlotTarget>>,
}

impl Default for NamespaceBuild {
    fn default() -> NamespaceBuild {
        NamespaceBuild {
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            class_version: HashMap::new(),
            interface_by_key: HashMap::new(),
            bindings: Vec::new(),
            binding_version: HashMap::new(),
            class_exports: HashMap::new(),
            interface_exports: HashMap::new(),
            func_exports: HashMap::new(),
            ctor_exports: HashMap::new(),
            export_hash: HashMap::new(),
            slot_initials: Vec::new(),
        }
    }
}

impl Merged {
    pub(crate) fn string(&mut self, text: &str) -> u32 {
        if let Some(idx) = self.string_index.get(text) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(text.to_string());
        Arc::make_mut(&mut self.string_index).insert(text.to_string(), idx);
        idx
    }

    pub(crate) fn selector(&mut self, name: &str) -> u32 {
        if let Some(idx) = self.selector_index.get(name) {
            return *idx;
        }
        let idx = self.selectors.len() as u32;
        self.selectors.push(name.to_string());
        Arc::make_mut(&mut self.selector_index).insert(name.to_string(), idx);
        idx
    }

    pub(crate) fn bytes(&mut self, value: &[u8]) -> u32 {
        if let Some(idx) = self.bytes_index.get(value) {
            return *idx;
        }
        let idx = self.bytes.len() as u32;
        let value = value.to_vec();
        self.bytes.push(value.clone());
        Arc::make_mut(&mut self.bytes_index).insert(value, idx);
        idx
    }

    pub(crate) fn ty(&mut self, ty: BcType, hash: [u8; 32]) -> Result<u32, LinkError> {
        if let Some(idx) = self.type_index.get(&ty) {
            if self.type_hashes[*idx as usize] != hash {
                return Err(fail("two resolved types have different identities"));
            }
            return Ok(*idx);
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_hashes.push(hash);
        Arc::make_mut(&mut self.type_index).insert(ty, idx);
        Ok(idx)
    }

    pub(crate) fn app(&mut self, app: TypeApp) -> u32 {
        if let Some(idx) = self.app_index.get(&app) {
            return *idx;
        }
        let idx = self.apps.len() as u32;
        self.apps.push(app.clone());
        Arc::make_mut(&mut self.app_index).insert(app, idx);
        idx
    }
}

/// Build dispatch rows only for classes in the new publication chunk.
fn extend_dispatch(merged: &mut Merged) {
    let first = merged.dispatch.len();
    let mut conformances_by_class = vec![Vec::new(); merged.classes.len().saturating_sub(first)];
    for (conformance_index, conformance) in merged.conformances.iter().enumerate() {
        let class = conformance.class as usize;
        if class >= first {
            conformances_by_class[class - first].push((conformance_index as u32, conformance));
        }
    }
    for class_index in first..merged.classes.len() {
        let class = &merged.classes[class_index];
        let inherited = class
            .parent()
            .map(|parent| &merged.dispatch[parent as usize]);
        let mut methods = Vec::new();
        if let Some(parent) = inherited {
            methods.extend(
                parent
                    .table
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, function)| *function != NO_METHOD)
                    .map(|(offset, function)| (parent.base + offset as u32, function)),
            );
        }
        let inherited_witnesses = inherited.and_then(|row| row.interface_witnesses.clone());
        let mut changed_witnesses: Option<Vec<InterfaceWitness>> = None;
        for (conformance_index, conformance) in &conformances_by_class[class_index - first] {
            let interface = conformance.application.interface;
            let has_default = merged.interfaces[interface as usize]
                .methods
                .iter()
                .any(|method| method.default != lm_bytecode::NO_FUNC);
            if !has_default {
                continue;
            }
            let witnesses = changed_witnesses.get_or_insert_with(|| {
                inherited_witnesses
                    .as_deref()
                    .map_or_else(Vec::new, <[_]>::to_vec)
            });
            let witness = InterfaceWitness {
                interface,
                conformance: *conformance_index,
                method_overrides: conformance.method_overrides.clone().into(),
            };
            match witnesses.binary_search_by_key(&interface, |item| item.interface) {
                Ok(index) => witnesses[index] = witness,
                Err(index) => witnesses.insert(index, witness),
            }
        }
        let interface_witnesses = changed_witnesses.map(Arc::from).or(inherited_witnesses);
        for (selector, function) in &class.methods {
            match methods.iter_mut().find(|(found, _)| found == selector) {
                Some(entry) => entry.1 = *function,
                None => methods.push((*selector, *function)),
            }
        }
        let row = match methods.iter().map(|(selector, _)| *selector).min() {
            Some(base) => {
                let top = methods
                    .iter()
                    .map(|(selector, _)| *selector)
                    .max()
                    .expect("the method table is not empty");
                let mut table = vec![NO_METHOD; (top - base + 1) as usize];
                for (selector, function) in methods {
                    table[(selector - base) as usize] = function;
                }
                DispatchRow {
                    base,
                    table,
                    interface_witnesses,
                }
            }
            None => DispatchRow {
                interface_witnesses,
                ..DispatchRow::default()
            },
        };
        merged.dispatch.push(row);
    }
}

pub(crate) fn tables_of(merged: &Merged) -> CodeTables {
    CodeTables {
        strings: merged.strings.clone(),
        bytes: merged.bytes.clone(),
        types: merged.types.clone(),
        selectors: merged.selectors.clone(),
        apps: merged.apps.clone(),
        slots: merged.slots.clone(),
        reflections: merged.reflections.clone(),
        classes: merged.classes.clone(),
        class_bounds: merged.class_bounds.clone(),
        interfaces: merged.interfaces.clone(),
        conformances: merged.conformances.clone(),
        funcs: merged.funcs.clone(),
        func_bounds: merged.func_bounds.clone(),
        debug: if merged.debug.sources.is_empty() {
            Vec::new()
        } else {
            lm_bytecode::debug::encode(&merged.debug)
        },
    }
}
