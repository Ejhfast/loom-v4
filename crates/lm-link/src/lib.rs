//! The pure linker.
//!
//! Linking merges the modules of one program into a single artifact
//! with an empty import table. It installs no global name, performs
//! no host operation, and reads no file: it takes decoded modules and
//! returns a decoded module.
//!
//! Three rules do the work.
//!
//! - **Slot resolution.** An import slot names a providing module and
//!   an export. The linker finds that export and rejects a provider
//!   whose interface hash differs from the pinned one.
//! - **Class merging.** The linker compares two classes on their
//!   qualified key and structural hash. One core provider supplies
//!   every imported core class.
//! - **Function binding resolution.** A named binding maps a qualified
//!   name to one function. One binding key with two structural hashes
//!   is a rejection. Two providers of one name never coexist.
//! - **Relocation.** Every module-global index is renumbered into the
//!   merged tables. Strings, types, selectors, and applications
//!   intern by content, so the merged tables stay canonical.
//!
//! The merged module passes the whole verifier before it runs. A
//! wrong pin or a wrong resolution therefore produces no executable
//! code that the verifier did not admit.

mod collect;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub use lm_bytecode::artifact::LinkUnit;
use lm_bytecode::artifact::{Artifact, ArtifactId, CORE_MODULE_PATH};
use lm_bytecode::identity::ModuleIdentity;
use lm_bytecode::interface::Interface;
use lm_bytecode::{
    BcAssociated, BcCallableContract, BcClass, BcClassKind, BcConformance, BcInterface,
    BcInterfaceMethod, BcInterfaceUse, BcRow, BcType, CodeTables, Export, ExtendedInstr, Func,
    FuncBinding, Import, ImportKind, Instr, Module, SlotContract, SlotSpec, SlotTarget, TypeApp,
    NO_CLASS, NO_PARENT,
};
use std::collections::HashMap;

/// Remove dependency declarations that local code cannot reach.
pub fn collect_compiled_unit(module: &Module) -> Result<Module, String> {
    collect::collect_compiled_unit(module).map(|(module, _)| module)
}

/// A failure to build a link environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkEnvError {
    DuplicateModule(String),
    InvalidUnit(String),
    MissingDependency {
        unit: String,
        dependency: String,
    },
    DependencyIdentityMismatch {
        unit: String,
        dependency: String,
    },
    MissingDependencyBinding {
        unit: String,
        dependency: String,
    },
    ExtraDependencyBinding {
        unit: String,
        dependency: String,
    },
    MissingArtifactDependency {
        unit: String,
        dependency: String,
    },
    RuntimeCoreMismatch {
        required: ArtifactId,
        available: ArtifactId,
    },
    DependencyCycle,
}

impl std::fmt::Display for LinkEnvError {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkEnvError::DuplicateModule(path) => {
                write!(out, "the module `{path}` is bound twice")
            }
            LinkEnvError::InvalidUnit(message) => out.write_str(message),
            LinkEnvError::MissingDependency { unit, dependency } => {
                write!(out, "module `{unit}` needs unbound module `{dependency}`")
            }
            LinkEnvError::DependencyIdentityMismatch { unit, dependency } => write!(
                out,
                "module `{unit}` pins another identity for `{dependency}`"
            ),
            LinkEnvError::MissingDependencyBinding { unit, dependency } => write!(
                out,
                "module `{unit}` imports `{dependency}` without an exact dependency"
            ),
            LinkEnvError::ExtraDependencyBinding { unit, dependency } => write!(
                out,
                "module `{unit}` has an unused exact dependency on `{dependency}`"
            ),
            LinkEnvError::MissingArtifactDependency { unit, dependency } => write!(
                out,
                "artifact module `{unit}` needs missing module `{dependency}`"
            ),
            LinkEnvError::RuntimeCoreMismatch {
                required,
                available,
            } => write!(
                out,
                "the artifact needs core {required}, but the runtime provides {available}"
            ),
            LinkEnvError::DependencyCycle => {
                out.write_str("the artifact module graph contains a dependency cycle")
            }
        }
    }
}

impl std::error::Error for LinkEnvError {}

/// A mutable set of modules for one link step.
#[derive(Debug, Clone, Default)]
pub struct LinkEnv {
    units: BTreeMap<String, Arc<LinkUnit>>,
}

impl LinkEnv {
    /// Create an empty link environment.
    pub fn new() -> LinkEnv {
        LinkEnv::default()
    }

    /// Bind one module to its canonical path.
    pub fn bind_unit<U>(&mut self, unit: U) -> Result<(), LinkEnvError>
    where
        U: Into<Arc<LinkUnit>>,
    {
        let unit = unit.into();
        let path = unit.module_path().to_string();
        if self.units.contains_key(&path) {
            return Err(LinkEnvError::DuplicateModule(path));
        }
        self.validate_dependencies(unit.as_ref())?;
        self.units.insert(path, unit);
        Ok(())
    }

    /// Build one link unit against the exact providers in this environment.
    pub fn prepare_unit(
        &self,
        path: impl Into<String>,
        module: Module,
        interface: Interface,
    ) -> Result<LinkUnit, LinkEnvError> {
        let bundle = lm_abi::standard_bundle();
        self.prepare_unit_with_bundle(path, module, interface, &bundle)
    }

    /// Build one link unit under one immutable ABI bundle.
    pub fn prepare_unit_with_bundle(
        &self,
        path: impl Into<String>,
        module: Module,
        interface: Interface,
        bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    ) -> Result<LinkUnit, LinkEnvError> {
        let path = path.into();
        if self.units.contains_key(&path) {
            return Err(LinkEnvError::DuplicateModule(path));
        }
        let imports = imported_module_paths(&module);
        let mut dependencies = Vec::with_capacity(imports.len());
        for dependency in imports {
            let provider =
                self.units
                    .get(&dependency)
                    .ok_or_else(|| LinkEnvError::MissingDependency {
                        unit: path.clone(),
                        dependency: dependency.clone(),
                    })?;
            dependencies.push(
                lm_bytecode::artifact::ArtifactDependency::new(&dependency, provider.id())
                    .map_err(|error| LinkEnvError::InvalidUnit(error.to_string()))?,
            );
        }
        if path != CORE_MODULE_PATH
            && !dependencies
                .iter()
                .any(|dependency| dependency.module_path() == CORE_MODULE_PATH)
        {
            if let Some(core) = self.units.get(CORE_MODULE_PATH) {
                dependencies.push(
                    lm_bytecode::artifact::ArtifactDependency::new(CORE_MODULE_PATH, core.id())
                        .map_err(|error| LinkEnvError::InvalidUnit(error.to_string()))?,
                );
            }
        }
        LinkUnit::new_with_bundle(path, module, interface, dependencies, bundle)
            .map_err(|error| LinkEnvError::InvalidUnit(error.to_string()))
    }

    fn validate_dependencies(&self, unit: &LinkUnit) -> Result<(), LinkEnvError> {
        let imports = imported_module_paths(unit.module());
        for dependency in &imports {
            let exact = unit
                .dependencies()
                .iter()
                .find(|candidate| candidate.module_path() == dependency)
                .ok_or_else(|| LinkEnvError::MissingDependencyBinding {
                    unit: unit.module_path().to_string(),
                    dependency: dependency.clone(),
                })?;
            let provider =
                self.units
                    .get(dependency)
                    .ok_or_else(|| LinkEnvError::MissingDependency {
                        unit: unit.module_path().to_string(),
                        dependency: dependency.clone(),
                    })?;
            if provider.id() != exact.artifact() {
                return Err(LinkEnvError::DependencyIdentityMismatch {
                    unit: unit.module_path().to_string(),
                    dependency: dependency.clone(),
                });
            }
        }
        for dependency in unit.dependencies() {
            if dependency.module_path() == CORE_MODULE_PATH
                && !imports.iter().any(|path| path == CORE_MODULE_PATH)
            {
                let provider = self.units.get(CORE_MODULE_PATH).ok_or_else(|| {
                    LinkEnvError::MissingDependency {
                        unit: unit.module_path().to_string(),
                        dependency: CORE_MODULE_PATH.to_string(),
                    }
                })?;
                if provider.id() != dependency.artifact() {
                    return Err(LinkEnvError::DependencyIdentityMismatch {
                        unit: unit.module_path().to_string(),
                        dependency: CORE_MODULE_PATH.to_string(),
                    });
                }
                continue;
            }
            if !imports.iter().any(|path| path == dependency.module_path())
                && dependency.module_path() != CORE_MODULE_PATH
            {
                return Err(LinkEnvError::ExtraDependencyBinding {
                    unit: unit.module_path().to_string(),
                    dependency: dependency.module_path().to_string(),
                });
            }
        }
        Ok(())
    }

    /// Freeze this environment for one link step.
    pub fn freeze(self) -> FrozenLinkEnv {
        FrozenLinkEnv { units: self.units }
    }
}

fn imported_module_paths(module: &Module) -> Vec<String> {
    let mut paths: Vec<String> = module
        .imports
        .iter()
        .map(|import| import.module.clone())
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// An immutable set of modules for one link step.
#[derive(Debug, Clone, Default)]
pub struct FrozenLinkEnv {
    units: BTreeMap<String, Arc<LinkUnit>>,
}

/// One definition selected from an artifact root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionSelection {
    Function(u32),
    Class(u32),
}

/// Build one exact portable definition artifact.
pub fn select_definition_artifact(
    artifact: Artifact,
    runtime_core: Option<Arc<LinkUnit>>,
    selection: DefinitionSelection,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Artifact, LinkError> {
    let (root, env) =
        resolve_artifact(artifact, runtime_core).map_err(|error| fail(error.to_string()))?;
    let root_path = env
        .paths()
        .into_iter()
        .find(|path| env.unit(path).is_some_and(|unit| unit.id() == root))
        .ok_or_else(|| fail("the artifact root unit is missing"))?
        .to_string();
    let source = env
        .unit(&root_path)
        .ok_or_else(|| fail("the artifact root unit is missing"))?;
    let (module, definition) = prepare_definition_module(source.module(), selection)?;
    build_definition_artifact(source, module, definition, &env, bundle)
}

impl FrozenLinkEnv {
    /// Return the module at one canonical path.
    pub fn unit(&self, path: &str) -> Option<&LinkUnit> {
        self.units.get(path).map(Arc::as_ref)
    }

    /// Return all bound module paths in canonical order.
    pub fn paths(&self) -> Vec<&str> {
        self.units.keys().map(String::as_str).collect()
    }

    /// Build a thin artifact for one root module.
    ///
    /// The artifact embeds every reachable unit except standard core.
    pub fn artifact(&self, root: &str) -> Result<Artifact, LinkError> {
        let bundle = lm_abi::standard_bundle();
        let selected = collect_environment(root, self, &bundle)?;
        let order = link_order(root, &selected)?;
        artifact_from_order(root, &selected, &order, false)
    }

    /// Build a fat artifact for one root module.
    pub fn fat_artifact(&self, root: &str) -> Result<Artifact, LinkError> {
        let bundle = lm_abi::standard_bundle();
        let selected = collect_environment(root, self, &bundle)?;
        let order = link_order(root, &selected)?;
        artifact_from_order(root, &selected, &order, true)
    }

    /// Build a thin artifact that keeps the root module surface.
    pub fn complete_artifact(&self, root: &str) -> Result<Artifact, LinkError> {
        let order = link_order(root, self)?;
        artifact_from_order(root, self, &order, false)
    }
}

/// Resolve one thin or fat artifact through the shared link environment.
pub fn resolve_artifact(
    artifact: Artifact,
    runtime_core: Option<Arc<LinkUnit>>,
) -> Result<(ArtifactId, FrozenLinkEnv), LinkEnvError> {
    let (root, mut units) = artifact.into_units();
    let mut units: Vec<Arc<LinkUnit>> = units.drain(..).map(Arc::new).collect();
    let mut available: BTreeSet<ArtifactId> = units.iter().map(|unit| unit.id()).collect();
    let mut required_core = None;
    for unit in &units {
        for dependency in unit.dependencies() {
            if available.contains(&dependency.artifact()) {
                continue;
            }
            if dependency.module_path() != CORE_MODULE_PATH {
                return Err(LinkEnvError::MissingArtifactDependency {
                    unit: unit.module_path().to_string(),
                    dependency: dependency.module_path().to_string(),
                });
            }
            match required_core {
                Some(required) if required != dependency.artifact() => {
                    return Err(LinkEnvError::MissingArtifactDependency {
                        unit: unit.module_path().to_string(),
                        dependency: CORE_MODULE_PATH.to_string(),
                    });
                }
                None => required_core = Some(dependency.artifact()),
                _ => {}
            }
        }
    }
    if let Some(required) = required_core {
        let core = runtime_core.ok_or_else(|| LinkEnvError::MissingArtifactDependency {
            unit: units
                .iter()
                .find(|unit| {
                    unit.dependencies()
                        .iter()
                        .any(|dependency| dependency.artifact() == required)
                })
                .map(|unit| unit.module_path().to_string())
                .unwrap_or_else(|| "<root>".to_string()),
            dependency: CORE_MODULE_PATH.to_string(),
        })?;
        if core.id() != required {
            return Err(LinkEnvError::RuntimeCoreMismatch {
                required,
                available: core.id(),
            });
        }
        if core.module_path() != CORE_MODULE_PATH {
            return Err(LinkEnvError::InvalidUnit(format!(
                "the runtime core uses module path `{}`",
                core.module_path()
            )));
        }
        available.insert(core.id());
        units.push(core);
    }
    let mut index = BTreeMap::new();
    for (position, unit) in units.iter().enumerate() {
        index.insert(unit.id(), position);
    }
    let mut indegree = vec![0usize; units.len()];
    let mut successors = vec![Vec::new(); units.len()];
    for (position, unit) in units.iter().enumerate() {
        indegree[position] = unit.dependencies().len();
        for dependency in unit.dependencies() {
            let Some(provider) = index.get(&dependency.artifact()).copied() else {
                return Err(LinkEnvError::MissingArtifactDependency {
                    unit: unit.module_path().to_string(),
                    dependency: dependency.module_path().to_string(),
                });
            };
            successors[provider].push(position);
        }
    }
    let mut ready = BTreeSet::new();
    for (position, unit) in units.iter().enumerate() {
        if indegree[position] == 0 {
            ready.insert((unit.id(), position));
        }
    }
    let mut units: Vec<Option<Arc<LinkUnit>>> = units.into_iter().map(Some).collect();
    let mut env = LinkEnv::new();
    let mut linked = 0usize;
    while let Some(&(id, position)) = ready.iter().next() {
        ready.remove(&(id, position));
        let unit = units[position]
            .take()
            .expect("one artifact unit enters the link environment once");
        env.bind_unit(unit)?;
        linked += 1;
        for successor in &successors[position] {
            indegree[*successor] -= 1;
            if indegree[*successor] == 0 {
                let successor_id = units[*successor]
                    .as_ref()
                    .expect("an unlinked successor keeps its unit")
                    .id();
                ready.insert((successor_id, *successor));
            }
        }
    }
    if linked != units.len() {
        return Err(LinkEnvError::DependencyCycle);
    }
    Ok((root, env.freeze()))
}

/// A link failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError(pub String);

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "link error: {}", self.0)
    }
}

fn fail(message: impl Into<String>) -> LinkError {
    LinkError(message.into())
}

/// Collect one exact artifact graph before relocation.
fn collect_environment(
    root: &str,
    env: &FrozenLinkEnv,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<FrozenLinkEnv, LinkError> {
    collect_environment_with_root(root, env, bundle, None)
}

fn collect_environment_with_root(
    root: &str,
    env: &FrozenLinkEnv,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    definition: Option<collect::DefinitionRoot>,
) -> Result<FrozenLinkEnv, LinkError> {
    let order = link_order(root, env)?;
    let mut requests: BTreeMap<String, Vec<(String, ImportKind)>> = BTreeMap::new();
    let mut selected: BTreeMap<String, Module> = BTreeMap::new();

    for path in order.iter().rev() {
        let unit = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
        if path == CORE_MODULE_PATH {
            selected.insert(path.clone(), unit.module().clone());
            continue;
        }
        let result = if path == root {
            match definition {
                Some(definition) => collect::collect_link_definition(unit.module(), definition),
                None => collect::collect_link_root(unit.module()),
            }
        } else {
            let Some(exports) = requests.remove(path) else {
                continue;
            };
            collect::collect_link_exports(unit.module(), &exports)
        };
        let module = result
            .map_err(|error| fail(format!("the module `{path}` does not collect: {error}")))?
            .0;
        for import in &module.imports {
            requests
                .entry(import.module.clone())
                .or_default()
                .push((import.name.clone(), import.kind));
        }
        selected.insert(path.clone(), module);
    }

    requests.remove(CORE_MODULE_PATH);
    if let Some(path) = requests.keys().next() {
        return Err(fail(format!(
            "the selected artifact needs the unbound module `{path}`"
        )));
    }

    let mut rebuilt = LinkEnv::new();
    for path in &order {
        let Some(module) = selected.remove(path) else {
            continue;
        };
        let source = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
        if path == CORE_MODULE_PATH {
            let source = env
                .units
                .get(path)
                .cloned()
                .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
            rebuilt
                .bind_unit(source)
                .map_err(|error| fail(error.to_string()))?;
            continue;
        }
        let interface = collected_interface(source, &module, bundle)?;
        let unit = rebuilt
            .prepare_unit_with_bundle(path.clone(), module, interface, bundle)
            .map_err(|error| fail(error.to_string()))?;
        rebuilt
            .bind_unit(unit)
            .map_err(|error| fail(error.to_string()))?;
    }
    Ok(rebuilt.freeze())
}

/// Verify decoded artifact units before relocation.
fn validate_untrusted_units(
    env: &FrozenLinkEnv,
    units: &BTreeSet<ArtifactId>,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<(), LinkError> {
    for path in env.paths() {
        let unit = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
        if !units.contains(&unit.id()) {
            continue;
        }
        if unit.interface().bundle_digest != bundle.digest() {
            return Err(fail(format!("the module `{path}` uses another ABI bundle")));
        }
        lm_verify::verify_module_with_bundle(unit.module(), bundle)
            .map_err(|error| fail(format!("the module `{path}` does not verify: {error}")))?;
    }
    Ok(())
}

fn collected_interface(
    source: &LinkUnit,
    module: &Module,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Interface, LinkError> {
    let identity = lm_bytecode::identity::module_identity_with_bundle(module, bundle)
        .map_err(|error| fail(format!("the collected module does not hash: {error}")))?;
    lm_bytecode::interface::derive_interface_with_bundle(
        module,
        &identity,
        source.module_path(),
        bundle,
    )
    .map_err(|error| fail(format!("the collected interface does not build: {error}")))
}

/// One resolved code namespace.
///
/// The namespace owns relocated tables and one exact artifact graph.
/// A VM executes this value. It never executes a `Module` payload.
#[derive(Debug, Clone)]
pub struct CodeNamespace {
    artifact_id: ArtifactId,
    artifacts: Vec<std::sync::Arc<Artifact>>,
    units: BTreeMap<ArtifactId, std::sync::Arc<LinkUnit>>,
    active_units: BTreeMap<String, ArtifactId>,
    relocations: BTreeMap<ArtifactId, UnitRelocation>,
    core_artifact: Option<ArtifactId>,
    tables: std::sync::Arc<CodeTables>,
    dispatch: Arc<[DispatchRow]>,
    entry: u32,
    core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    exports: Vec<Export>,
    bindings: Arc<[FuncBinding]>,
    identity: Arc<ModuleIdentity>,
    closure_bodies: Arc<std::sync::OnceLock<Vec<bool>>>,
    slot_initials: Arc<[Option<SlotTarget>]>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
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

    pub fn relocation(&self, id: ArtifactId) -> Option<&UnitRelocation> {
        self.relocations.get(&id)
    }

    pub fn contains_function(&self, function: u32) -> bool {
        self.relocations
            .values()
            .any(|relocation| relocation.functions().contains(&function))
    }

    pub fn contains_class(&self, class: u32) -> bool {
        self.relocations
            .values()
            .any(|relocation| relocation.classes().contains(&class))
    }

    pub fn contains_slot(&self, slot: u32) -> bool {
        self.relocations
            .values()
            .any(|relocation| relocation.slots().contains(&slot))
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

    pub fn dispatch_store(&self) -> Arc<[DispatchRow]> {
        self.dispatch.clone()
    }

    pub fn entry(&self) -> u32 {
        self.entry
    }

    pub fn core_roles(&self) -> &[u32; lm_bytecode::CORE_ROLE_COUNT] {
        &self.core_roles
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

    /// Build one portable artifact for an arena function.
    pub fn function_artifact(&self, function: u32) -> Result<Artifact, LinkError> {
        let (unit, local) = self.local_function(function)?;
        let (module, definition) =
            prepare_definition_module(unit.module(), DefinitionSelection::Function(local))?;
        self.build_definition_artifact(unit, module, definition)
    }

    /// Build one portable artifact for an arena class.
    pub fn class_artifact(&self, class: u32) -> Result<Artifact, LinkError> {
        let (unit, local) = self.local_class(class)?;
        let (module, definition) =
            prepare_definition_module(unit.module(), DefinitionSelection::Class(local))?;
        self.build_definition_artifact(unit, module, definition)
    }

    fn local_function(&self, function: u32) -> Result<(&LinkUnit, u32), LinkError> {
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
        module: Module,
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
        build_definition_artifact(source, module, definition, &env, &self.bundle)
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
                BcRow::Op(index) => self.tables.strings[*index as usize].as_str(),
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
    #[inline]
    pub fn method(&self, selector: u32) -> Option<u32> {
        let offset = selector.checked_sub(self.base)? as usize;
        match self.table.get(offset).copied() {
            Some(NO_METHOD) | None => None,
            Some(function) => Some(function),
        }
    }

    #[inline]
    pub fn interface_override(&self, interface: u32, method: u32) -> Option<bool> {
        let witnesses = self.interface_witnesses.as_deref()?;
        let witness = witnesses
            .binary_search_by_key(&interface, |witness| witness.interface)
            .ok()
            .map(|index| &witnesses[index])?;
        witness.method_overrides.get(method as usize).copied()
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

fn build_dispatch(tables: &CodeTables) -> Arc<[DispatchRow]> {
    let mut resolved: Vec<Vec<(u32, u32)>> = Vec::with_capacity(tables.classes.len());
    let mut dispatch: Vec<DispatchRow> = Vec::with_capacity(tables.classes.len());
    let mut conformances_by_class = vec![Vec::new(); tables.classes.len()];
    for (index, conformance) in tables.conformances.iter().enumerate() {
        conformances_by_class[conformance.class as usize].push(index);
    }
    let interfaces_with_defaults: Vec<bool> = tables
        .interfaces
        .iter()
        .map(|interface| {
            interface
                .methods
                .iter()
                .any(|method| method.default != lm_bytecode::NO_FUNC)
        })
        .collect();
    for (class_index, class) in tables.classes.iter().enumerate() {
        let mut methods: Vec<(u32, u32)> = match class.parent() {
            Some(parent) => resolved[parent as usize].clone(),
            None => Vec::new(),
        };
        let inherited_witnesses = class
            .parent()
            .and_then(|parent| dispatch[parent as usize].interface_witnesses.clone());
        let mut changed_witnesses: Option<Vec<InterfaceWitness>> = None;
        for conformance in conformances_by_class[class_index]
            .iter()
            .map(|index| &tables.conformances[*index])
        {
            let interface = conformance.application.interface as usize;
            if interfaces_with_defaults[interface] {
                let interface = interface as u32;
                let witnesses = changed_witnesses.get_or_insert_with(|| {
                    inherited_witnesses
                        .as_deref()
                        .map_or_else(Vec::new, <[_]>::to_vec)
                });
                let witness = InterfaceWitness {
                    interface,
                    method_overrides: conformance.method_overrides.clone().into(),
                };
                match witnesses.binary_search_by_key(&interface, |item| item.interface) {
                    Ok(index) => witnesses[index] = witness,
                    Err(index) => witnesses.insert(index, witness),
                }
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
                for (selector, function) in &methods {
                    table[(*selector - base) as usize] = *function;
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
        resolved.push(methods);
        dispatch.push(row);
    }
    dispatch.into()
}

fn prepare_definition_module(
    source: &Module,
    selection: DefinitionSelection,
) -> Result<(Module, collect::DefinitionRoot), LinkError> {
    let mut module = source.clone();
    let (export, definition) = match selection {
        DefinitionSelection::Function(function) => {
            let export = module
                .exports
                .iter()
                .find(|export| {
                    export.kind == lm_bytecode::ExportKind::Function && export.def == function
                })
                .cloned()
                .or_else(|| {
                    let binding = module
                        .bindings
                        .iter()
                        .find(|binding| binding.class == NO_CLASS && binding.func == function)?;
                    Some(Export {
                        kind: lm_bytecode::ExportKind::Function,
                        name: binding
                            .key
                            .rsplit_once('.')
                            .map_or(binding.key.clone(), |(_, name)| name.to_string()),
                        def: function,
                        ctor: lm_bytecode::NO_CTOR,
                    })
                })
                .ok_or_else(|| fail("the function has no portable export"))?;
            (export, collect::DefinitionRoot::Function(function))
        }
        DefinitionSelection::Class(class) => {
            let constructor = module
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
            let source = module
                .classes
                .get(class as usize)
                .ok_or_else(|| fail("the portable class is missing"))?;
            if source.has_init && constructor == lm_bytecode::NO_CTOR {
                return Err(fail("the class has no portable constructor binding"));
            }
            let kind = match source.kind {
                BcClassKind::Normal => lm_bytecode::ExportKind::Class,
                BcClassKind::Abstract => lm_bytecode::ExportKind::Enum,
                BcClassKind::Case => lm_bytecode::ExportKind::EnumCase,
            };
            let export = module
                .exports
                .iter()
                .find(|export| export.kind.is_class() && export.def == class)
                .cloned()
                .unwrap_or_else(|| Export {
                    kind,
                    name: source.name.clone(),
                    def: class,
                    ctor: constructor,
                });
            (export, collect::DefinitionRoot::Class(class))
        }
    };
    module.exports = vec![export];
    Ok((module, definition))
}

fn build_definition_artifact(
    source: &LinkUnit,
    module: Module,
    definition: collect::DefinitionRoot,
    env: &FrozenLinkEnv,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Artifact, LinkError> {
    let replacement = LinkUnit::from_module_with_bundle(
        source.module_path(),
        module,
        source.dependencies().to_vec(),
        bundle,
    )
    .map_err(|error| fail(format!("the portable unit is invalid: {error}")))?;
    let mut units = env.units.clone();
    units.insert(source.module_path().to_string(), Arc::new(replacement));
    let env = FrozenLinkEnv { units };
    let selected =
        collect_environment_with_root(source.module_path(), &env, bundle, Some(definition))?;
    let order = link_order(source.module_path(), &selected)?;
    artifact_from_order(source.module_path(), &selected, &order, false)
}

/// The stable index of one published namespace in a world arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamespaceId(u32);

impl NamespaceId {
    pub const ROOT: NamespaceId = NamespaceId(0);

    pub fn from_index(index: u32) -> NamespaceId {
        NamespaceId(index)
    }

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
        let retained = std::sync::Arc::new(artifact.clone());
        let root_path = artifact.root().module_path().to_string();
        let untrusted: BTreeSet<ArtifactId> = artifact.units().iter().map(LinkUnit::id).collect();
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
                    merged.units.insert(unit.id(), reloc.clone());
                    reloc
                }
            };
            relocations.insert(unit.id(), UnitRelocation(reloc.clone()));
            units.insert(unit.id(), Arc::new(unit.clone()));
            active_units.insert(path.clone(), unit.id());
            if path == &root_path {
                entry = reloc.funcs.get(unit.module().entry as usize).copied();
                root_exports = relocated_exports(unit.module(), &reloc)?;
            }
        }
        let entry = entry.ok_or_else(|| fail("the artifact root has no entry"))?;
        let core_artifact = active_units.get(CORE_MODULE_PATH).copied();
        view.slot_initials.resize(merged.slots.len(), None);
        let tables = Arc::new(tables_of(&merged));
        let dispatch = build_dispatch(&tables);
        let identity = Arc::new(namespace_identity(&merged, root));
        let namespace = CodeNamespace {
            artifact_id: root,
            artifacts: vec![retained],
            units,
            active_units,
            relocations,
            core_artifact,
            tables,
            dispatch,
            entry,
            core_roles: view.core_roles,
            exports: root_exports,
            bindings: view.bindings.into(),
            identity,
            closure_bodies: Arc::new(std::sync::OnceLock::new()),
            slot_initials: view.slot_initials.into(),
            bundle: self.bundle.clone(),
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
        let mut namespace = self.publish_known(root.as_ref().clone(), core)?;
        for artifact in artifacts {
            namespace = self.extend_known(namespace, artifact.as_ref().clone())?;
        }
        Ok(namespace)
    }

    fn publish_known(
        &mut self,
        artifact: Artifact,
        runtime_core: Option<Arc<LinkUnit>>,
    ) -> Result<NamespaceId, LinkError> {
        let units: BTreeSet<ArtifactId> = artifact.units().iter().map(LinkUnit::id).collect();
        Arc::make_mut(&mut self.verified).extend(units);
        self.publish(artifact, runtime_core)
    }

    fn extend_known(
        &mut self,
        base: NamespaceId,
        artifact: Artifact,
    ) -> Result<NamespaceId, LinkError> {
        let units: BTreeSet<ArtifactId> = artifact.units().iter().map(LinkUnit::id).collect();
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
        let untrusted: BTreeSet<ArtifactId> = artifact.units().iter().map(LinkUnit::id).collect();
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
        let mut merged = self.merged.as_ref().clone();
        let mut addition = NamespaceBuild::default();
        let mut root_exports = Vec::new();
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
                    merged.units.insert(unit.id(), reloc.clone());
                    reloc
                }
            };
            relocations.insert(unit.id(), UnitRelocation(reloc.clone()));
            units.insert(unit.id(), Arc::new(unit.clone()));
            active_units.insert(path.clone(), unit.id());
            if path == &root_path {
                root_exports = relocated_exports(unit.module(), &reloc)?;
            }
        }

        let mut slot_initials = base.slot_initials.to_vec();
        slot_initials.resize(merged.slots.len(), None);
        addition.slot_initials.resize(merged.slots.len(), None);
        for (index, initial) in addition.slot_initials.into_iter().enumerate() {
            if initial.is_some() {
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
            binding_by_key.insert(binding.key.clone(), binding);
        }
        let mut artifacts = base.artifacts.clone();
        if !artifacts.iter().any(|item| item.id() == retained.id()) {
            artifacts.push(retained);
        }
        let tables = Arc::new(tables_of(&merged));
        let dispatch = build_dispatch(&tables);
        let identity = Arc::new(namespace_identity(&merged, base.artifact_id));
        let namespace = CodeNamespace {
            artifact_id: base.artifact_id,
            artifacts,
            units,
            active_units,
            relocations,
            core_artifact: base.core_artifact,
            tables,
            dispatch,
            entry: base.entry,
            core_roles: base.core_roles,
            exports: base.exports.clone(),
            bindings: binding_by_key.into_values().collect::<Vec<_>>().into(),
            identity,
            closure_bodies: Arc::new(std::sync::OnceLock::new()),
            slot_initials: slot_initials.into(),
            bundle: self.bundle.clone(),
        };
        let index = u32::try_from(self.namespaces.len())
            .map_err(|_| fail("the world has too many code namespaces"))?;
        let id = NamespaceId(index);
        Arc::make_mut(&mut self.namespaces).push(Arc::new(namespace));
        Arc::make_mut(&mut self.by_chain).insert(chain, id);
        self.merged = Arc::new(merged);
        Arc::make_mut(&mut self.verified).extend(unchecked);
        let _ = root_exports;
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

fn namespace_identity(merged: &Merged, artifact: ArtifactId) -> ModuleIdentity {
    ModuleIdentity {
        class_hashes: merged.class_hashes.clone(),
        func_hashes: merged.func_hashes.clone(),
        interface_hashes: merged.interface_hashes.clone(),
        type_hashes: merged.type_hashes.clone(),
        semantic_hash: artifact.into_bytes(),
        max_refine_rounds: 0,
    }
}

/// The append-only dense tables of one code arena.
#[derive(Debug, Clone, Default)]
struct Merged {
    strings: Vec<String>,
    string_index: HashMap<String, u32>,
    bytes: Vec<Vec<u8>>,
    bytes_index: HashMap<Vec<u8>, u32>,
    types: Vec<BcType>,
    type_hashes: Vec<[u8; 32]>,
    type_index: HashMap<BcType, u32>,
    selectors: Vec<String>,
    selector_index: HashMap<String, u32>,
    apps: Vec<TypeApp>,
    app_index: HashMap<TypeApp, u32>,
    classes: Vec<BcClass>,
    class_hashes: Vec<[u8; 32]>,
    class_bounds: Vec<Vec<Vec<BcInterfaceUse>>>,
    interfaces: Vec<BcInterface>,
    interface_hashes: Vec<[u8; 32]>,
    conformances: Vec<BcConformance>,
    funcs: Vec<Func>,
    func_hashes: Vec<[u8; 32]>,
    func_bounds: Vec<Vec<Vec<BcInterfaceUse>>>,
    /// Late-bound slot contracts, merged by stable key and contract.
    slots: Vec<SlotSpec>,
    slot_by_contract: HashMap<(ArtifactId, [u8; 32], [u8; 32]), u32>,
    /// Optional source data after table relocation.
    debug: lm_bytecode::debug::DebugInfo,
    /// One permanent relocation for each exact unit.
    units: HashMap<ArtifactId, Reloc>,
}

/// One artifact graph's bindings over arena indices.
#[derive(Debug, Clone)]
struct NamespaceBuild {
    core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    class_version: HashMap<String, (u32, [u8; 32], String)>,
    interface_by_key: HashMap<String, (u32, String)>,
    bindings: Vec<lm_bytecode::FuncBinding>,
    binding_version: HashMap<String, ([u8; 32], String)>,
    class_exports: HashMap<(String, String), u32>,
    interface_exports: HashMap<(String, String), u32>,
    func_exports: HashMap<(String, String), u32>,
    ctor_exports: HashMap<(String, String), u32>,
    export_hash: HashMap<(String, String), [u8; 32]>,
    slot_initials: Vec<Option<SlotTarget>>,
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
    fn string(&mut self, text: &str) -> u32 {
        if let Some(idx) = self.string_index.get(text) {
            return *idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(text.to_string());
        self.string_index.insert(text.to_string(), idx);
        idx
    }

    fn selector(&mut self, name: &str) -> u32 {
        if let Some(idx) = self.selector_index.get(name) {
            return *idx;
        }
        let idx = self.selectors.len() as u32;
        self.selectors.push(name.to_string());
        self.selector_index.insert(name.to_string(), idx);
        idx
    }

    fn bytes(&mut self, value: &[u8]) -> u32 {
        if let Some(idx) = self.bytes_index.get(value) {
            return *idx;
        }
        let idx = self.bytes.len() as u32;
        let value = value.to_vec();
        self.bytes.push(value.clone());
        self.bytes_index.insert(value, idx);
        idx
    }

    fn ty(&mut self, ty: BcType, hash: [u8; 32]) -> Result<u32, LinkError> {
        if let Some(idx) = self.type_index.get(&ty) {
            if self.type_hashes[*idx as usize] != hash {
                return Err(fail("two resolved types have different identities"));
            }
            return Ok(*idx);
        }
        let idx = self.types.len() as u32;
        self.types.push(ty.clone());
        self.type_hashes.push(hash);
        self.type_index.insert(ty, idx);
        Ok(idx)
    }

    fn app(&mut self, app: TypeApp) -> u32 {
        if let Some(idx) = self.app_index.get(&app) {
            return *idx;
        }
        let idx = self.apps.len() as u32;
        self.apps.push(app.clone());
        self.app_index.insert(app, idx);
        idx
    }
}

/// One module's relocation maps.
#[derive(Debug, Clone)]
struct Reloc {
    strings: Vec<u32>,
    bytes: Vec<u32>,
    types: Vec<u32>,
    selectors: Vec<u32>,
    apps: Vec<u32>,
    classes: Vec<u32>,
    interfaces: Vec<u32>,
    funcs: Vec<u32>,
    slots: Vec<u32>,
}

/// Stable arena indices for one exact link unit.
#[derive(Debug, Clone)]
pub struct UnitRelocation(Reloc);

impl UnitRelocation {
    pub fn strings(&self) -> &[u32] {
        &self.0.strings
    }

    pub fn bytes(&self) -> &[u32] {
        &self.0.bytes
    }

    pub fn types(&self) -> &[u32] {
        &self.0.types
    }

    pub fn selectors(&self) -> &[u32] {
        &self.0.selectors
    }

    pub fn applications(&self) -> &[u32] {
        &self.0.apps
    }

    pub fn classes(&self) -> &[u32] {
        &self.0.classes
    }

    pub fn interfaces(&self) -> &[u32] {
        &self.0.interfaces
    }

    pub fn functions(&self) -> &[u32] {
        &self.0.funcs
    }

    pub fn slots(&self) -> &[u32] {
        &self.0.slots
    }
}

/// Exact dense-index maps between two publications of one graph.
#[derive(Debug, Clone)]
pub struct CodeRelocation {
    identity: bool,
    strings: Vec<Option<u32>>,
    bytes: Vec<Option<u32>>,
    types: Vec<Option<u32>>,
    selectors: Vec<Option<u32>>,
    applications: Vec<Option<u32>>,
    classes: Vec<Option<u32>>,
    interfaces: Vec<Option<u32>>,
    functions: Vec<Option<u32>>,
    slots: Vec<Option<u32>>,
}

impl CodeRelocation {
    fn with_source(source: &CodeTables) -> CodeRelocation {
        CodeRelocation {
            identity: false,
            strings: vec![None; source.strings.len()],
            bytes: vec![None; source.bytes.len()],
            types: vec![None; source.types.len()],
            selectors: vec![None; source.selectors.len()],
            applications: vec![None; source.apps.len()],
            classes: vec![None; source.classes.len()],
            interfaces: vec![None; source.interfaces.len()],
            functions: vec![None; source.funcs.len()],
            slots: vec![None; source.slots.len()],
        }
    }

    /// Build the identity map for one shared arena.
    pub fn identity() -> CodeRelocation {
        CodeRelocation {
            identity: true,
            strings: Vec::new(),
            bytes: Vec::new(),
            types: Vec::new(),
            selectors: Vec::new(),
            applications: Vec::new(),
            classes: Vec::new(),
            interfaces: Vec::new(),
            functions: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// True when every source index is also its target index.
    pub fn is_identity(&self) -> bool {
        self.identity
    }

    fn merge_unit(
        &mut self,
        source: &UnitRelocation,
        target: &UnitRelocation,
    ) -> Result<(), LinkError> {
        merge_index_map(
            &mut self.strings,
            source.strings(),
            target.strings(),
            "string",
        )?;
        merge_index_map(
            &mut self.bytes,
            source.bytes(),
            target.bytes(),
            "byte literal",
        )?;
        merge_index_map(&mut self.types, source.types(), target.types(), "type")?;
        merge_index_map(
            &mut self.selectors,
            source.selectors(),
            target.selectors(),
            "selector",
        )?;
        merge_index_map(
            &mut self.applications,
            source.applications(),
            target.applications(),
            "type application",
        )?;
        merge_index_map(
            &mut self.classes,
            source.classes(),
            target.classes(),
            "class",
        )?;
        merge_index_map(
            &mut self.interfaces,
            source.interfaces(),
            target.interfaces(),
            "interface",
        )?;
        merge_index_map(
            &mut self.functions,
            source.functions(),
            target.functions(),
            "function",
        )?;
        merge_index_map(&mut self.slots, source.slots(), target.slots(), "slot")?;
        Ok(())
    }

    /// Add every resolved index from another compatible map.
    pub fn merge(&mut self, other: &CodeRelocation) -> Result<(), LinkError> {
        if self.identity || other.identity {
            if self.identity && other.identity {
                return Ok(());
            }
            return Err(fail("an identity relocation cannot merge with another map"));
        }
        merge_optional_map(&mut self.strings, &other.strings, "string")?;
        merge_optional_map(&mut self.bytes, &other.bytes, "byte literal")?;
        merge_optional_map(&mut self.types, &other.types, "type")?;
        merge_optional_map(&mut self.selectors, &other.selectors, "selector")?;
        merge_optional_map(
            &mut self.applications,
            &other.applications,
            "type application",
        )?;
        merge_optional_map(&mut self.classes, &other.classes, "class")?;
        merge_optional_map(&mut self.interfaces, &other.interfaces, "interface")?;
        merge_optional_map(&mut self.functions, &other.functions, "function")?;
        merge_optional_map(&mut self.slots, &other.slots, "slot")?;
        Ok(())
    }

    pub fn string(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.strings, source)
    }

    pub fn bytes(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.bytes, source)
    }

    pub fn ty(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.types, source)
    }

    pub fn selector(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.selectors, source)
    }

    pub fn application(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.applications, source)
    }

    pub fn class(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.classes, source)
    }

    pub fn interface(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.interfaces, source)
    }

    pub fn function(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.functions, source)
    }

    pub fn slot(&self, source: u32) -> Option<u32> {
        if self.identity {
            return Some(source);
        }
        map_index(&self.slots, source)
    }
}

fn merge_optional_map(
    target: &mut Vec<Option<u32>>,
    source: &[Option<u32>],
    what: &str,
) -> Result<(), LinkError> {
    target.resize(target.len().max(source.len()), None);
    for (index, value) in source.iter().copied().enumerate() {
        let Some(value) = value else {
            continue;
        };
        match target[index] {
            Some(current) if current != value => {
                return Err(fail(format!(
                    "the {what} index {index} has conflicting relocation targets"
                )));
            }
            Some(_) => {}
            None => target[index] = Some(value),
        }
    }
    Ok(())
}

fn map_index(map: &[Option<u32>], source: u32) -> Option<u32> {
    map.get(source as usize).copied().flatten()
}

fn merge_index_map(
    map: &mut [Option<u32>],
    source: &[u32],
    target: &[u32],
    kind: &str,
) -> Result<(), LinkError> {
    if source.len() != target.len() {
        return Err(fail(format!("the {kind} relocation has another length")));
    }
    for (source, target) in source.iter().copied().zip(target.iter().copied()) {
        let entry = map
            .get_mut(source as usize)
            .ok_or_else(|| fail(format!("the source {kind} index is outside its tables")))?;
        match *entry {
            Some(existing) if existing != target => {
                return Err(fail(format!("the shared {kind} has two target indices")))
            }
            Some(_) => {}
            None => *entry = Some(target),
        }
    }
    Ok(())
}

fn tables_of(merged: &Merged) -> CodeTables {
    CodeTables {
        strings: merged.strings.clone(),
        bytes: merged.bytes.clone(),
        types: merged.types.clone(),
        selectors: merged.selectors.clone(),
        apps: merged.apps.clone(),
        slots: merged.slots.clone(),
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

fn relocated_exports(module: &Module, reloc: &Reloc) -> Result<Vec<Export>, LinkError> {
    module
        .exports
        .iter()
        .map(|export| {
            let def = if export.kind.is_class() {
                reloc.classes.get(export.def as usize)
            } else if export.kind.is_interface() {
                reloc.interfaces.get(export.def as usize)
            } else {
                reloc.funcs.get(export.def as usize)
            }
            .copied()
            .ok_or_else(|| fail("an export names a missing relocated definition"))?;
            let ctor = if export.ctor == lm_bytecode::NO_CTOR {
                lm_bytecode::NO_CTOR
            } else {
                reloc
                    .funcs
                    .get(export.ctor as usize)
                    .copied()
                    .ok_or_else(|| fail("an export names a missing relocated constructor"))?
            };
            Ok(Export {
                kind: export.kind,
                name: export.name.clone(),
                def,
                ctor,
            })
        })
        .collect()
}

/// Merge one validated unit.
fn merge_unit(
    merged: &mut Merged,
    view: &mut NamespaceBuild,
    unit: &LinkUnit,
    path: &str,
    slot_scope: ArtifactId,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Reloc, LinkError> {
    if unit.interface().bundle_digest != bundle.digest() {
        return Err(fail(format!("the module `{path}` uses another ABI bundle")));
    }
    let identity = unit.identity();
    let reloc = relocate(merged, view, unit.module(), identity, path, slot_scope)?;
    bind_unit(view, merged, unit, path, &reloc)?;
    Ok(reloc)
}

/// Bind one relocated unit into one artifact namespace.
fn bind_unit(
    view: &mut NamespaceBuild,
    merged: &Merged,
    unit: &LinkUnit,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let module = unit.module();
    let identity = unit.identity();
    let extern_classes = module.extern_classes();
    for (index, class) in module.classes.iter().enumerate() {
        if extern_classes[index] {
            continue;
        }
        let target = reloc.classes[index];
        let hash = identity.class_hashes[index];
        match view.class_version.get(&class.key) {
            Some((found, _, provider)) if *found != target => {
                return Err(fail(format!(
                    "the class `{}` is provided by `{provider}` and `{path}`",
                    class.key
                )));
            }
            Some(_) => {}
            None => {
                view.class_version
                    .insert(class.key.clone(), (target, hash, path.to_string()));
            }
        }
    }
    for (index, interface) in module.interfaces.iter().enumerate() {
        let target = reloc.interfaces[index];
        match view.interface_by_key.get(&interface.key) {
            Some((found, provider)) if *found != target => {
                return Err(fail(format!(
                    "the interface `{}` is provided by `{provider}` and `{path}`",
                    interface.key
                )));
            }
            Some(_) => {}
            None => {
                view.interface_by_key
                    .insert(interface.key.clone(), (target, path.to_string()));
            }
        }
    }
    view.slot_initials.resize(merged.slots.len(), None);
    for (index, source) in module.slots.iter().enumerate() {
        let target = reloc.slots[index] as usize;
        let initial = source.initial.map(|value| reloc_slot_target(value, reloc));
        match (view.slot_initials[target], initial) {
            (Some(found), Some(wanted)) if found != wanted => {
                return Err(fail(format!(
                    "the slot {index} of `{path}` has another initial target"
                )));
            }
            (None, Some(wanted)) => view.slot_initials[target] = Some(wanted),
            _ => {}
        }
    }
    for (role, source) in module.core_roles.iter().enumerate() {
        if *source == lm_bytecode::NO_ROLE {
            continue;
        }
        let target = reloc
            .classes
            .get(*source as usize)
            .copied()
            .ok_or_else(|| fail(format!("the core role {role} of `{path}` is invalid")))?;
        match view.core_roles[role] {
            lm_bytecode::NO_ROLE => view.core_roles[role] = target,
            found if found == target => {}
            _ => {
                return Err(fail(format!(
                    "the module `{path}` uses another class for core role {role}"
                )));
            }
        }
    }
    merge_bindings(view, module, identity, path, reloc)?;
    register_exports(view, module, unit.interface(), path, reloc)
}

fn artifact_from_order(
    root: &str,
    env: &FrozenLinkEnv,
    order: &[String],
    embed_core: bool,
) -> Result<Artifact, LinkError> {
    let root_unit = env
        .unit(root)
        .cloned()
        .ok_or_else(|| fail(format!("the module `{root}` is not bound")))?;
    let mut embedded = Vec::new();
    for path in order {
        if path == root || (!embed_core && path == CORE_MODULE_PATH) {
            continue;
        }
        embedded.push(
            env.unit(path)
                .cloned()
                .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?,
        );
    }
    Artifact::new(root_unit, embedded)
        .map_err(|error| fail(format!("the artifact graph is invalid: {error}")))
}

/// The link order: every imported module before its importer. The
/// walk rejects a cycle and an unbound module.
fn link_order(root: &str, env: &FrozenLinkEnv) -> Result<Vec<String>, LinkError> {
    let mut order: Vec<String> = Vec::new();
    let mut done: Vec<String> = Vec::new();
    let mut path: Vec<String> = Vec::new();
    // An explicit stack keeps the walk off the host stack.
    let mut stack: Vec<(String, bool)> = vec![(root.to_string(), false)];
    while let Some((name, expanded)) = stack.pop() {
        if done.contains(&name) {
            continue;
        }
        if expanded {
            path.retain(|p| *p != name);
            done.push(name.clone());
            order.push(name);
            continue;
        }
        let unit = env
            .unit(&name)
            .ok_or_else(|| fail(format!("the module `{name}` is not bound")))?;
        path.push(name.clone());
        stack.push((name.clone(), true));
        let mut needs: Vec<&str> = unit
            .dependencies()
            .iter()
            .map(|dependency| dependency.module_path())
            .collect();
        needs.sort_unstable();
        needs.dedup();
        for need in needs {
            if path.iter().any(|p| p == need) {
                return Err(fail(format!(
                    "the modules `{name}` and `{need}` import each other"
                )));
            }
            if !done.iter().any(|p| p == need) {
                stack.push((need.to_string(), false));
            }
        }
    }
    Ok(order)
}

/// Relocate one module into the merged tables.
fn relocate(
    merged: &mut Merged,
    view: &NamespaceBuild,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
    slot_scope: ArtifactId,
) -> Result<Reloc, LinkError> {
    let extern_classes = module.extern_classes();
    let strings: Vec<u32> = module.strings.iter().map(|s| merged.string(s)).collect();
    let bytes: Vec<u32> = module
        .bytes
        .iter()
        .map(|value| merged.bytes(value))
        .collect();
    let selectors: Vec<u32> = module
        .selectors
        .iter()
        .map(|s| merged.selector(s))
        .collect();
    // The class map first: a type may name a class, and an imported
    // class resolves to a definition another module provides.
    let extern_funcs = module.extern_funcs();
    let mut classes: Vec<u32> = vec![u32::MAX; module.classes.len()];
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind != ImportKind::Class {
            continue;
        }
        let target = resolve_class_import(view, import, path, idx)?;
        classes[import.def as usize] = target;
    }
    // Types reference only earlier types, so one ascending pass is
    // enough. A class reference of a local class resolves below.
    let mut types: Vec<u32> = vec![u32::MAX; module.types.len()];
    let mut apps: Vec<u32> = vec![u32::MAX; module.apps.len()];
    // The local classes take merged indices in ascending order, so a
    // parent keeps a lower index than its child. A class body needs
    // the type map, and a type may name a class. The indices are
    // therefore assigned first, and the bodies fill after the types
    // relocate.
    let mut created_classes: Vec<u32> = Vec::new();
    for idx in 0..module.classes.len() as u32 {
        if classes[idx as usize] != u32::MAX {
            continue;
        }
        let source = &module.classes[idx as usize];
        let hash = identity.class_hashes[idx as usize];
        match view.class_version.get(&source.key) {
            Some((_, seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the class `{}` arrives with two implementations, from `{provider}` and \
                     from `{path}`; rebuild both against one version",
                    source.key
                )));
            }
            _ => {}
        }
        let at = merged.classes.len() as u32;
        merged.classes.push(BcClass {
            name: source.name.clone(),
            key: source.key.clone(),
            is_final: source.is_final,
            is_frozen: source.is_frozen,
            parent: NO_PARENT,
            parent_args: Vec::new(),
            type_params: source.type_params,
            kind: source.kind,
            fields: Vec::new(),
            field_defaults: Vec::new(),
            own_start: 0,
            has_init: false,
            methods: Vec::new(),
        });
        merged.class_hashes.push(hash);
        merged.class_bounds.push(Vec::new());
        classes[idx as usize] = at;
        created_classes.push(idx);
    }
    // Interface keys are nominal. Assign every merged index before
    // type relocation because a projection names an interface.
    let mut interfaces: Vec<u32> = vec![u32::MAX; module.interfaces.len()];
    let mut created_interfaces: Vec<u32> = Vec::new();
    let mut shared_interfaces: Vec<u32> = Vec::new();
    for (idx, source) in module.interfaces.iter().enumerate() {
        if let Some((existing, _)) = view.interface_by_key.get(&source.key) {
            interfaces[idx] = *existing;
            shared_interfaces.push(idx as u32);
            continue;
        }
        if merged.interfaces.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize {
            return Err(fail(format!(
                "the linked program has too many interfaces after `{path}`"
            )));
        }
        let at = merged.interfaces.len() as u32;
        merged.interfaces.push(BcInterface {
            name: source.name.clone(),
            key: source.key.clone(),
            type_params: 0,
            effect_params: 0,
            generic_is_effect: Vec::new(),
            parents: Vec::new(),
            type_bounds: Vec::new(),
            associated: Vec::new(),
            methods: Vec::new(),
        });
        merged.interface_hashes.push(identity.interface_hashes[idx]);
        interfaces[idx] = at;
        created_interfaces.push(idx as u32);
    }
    for (idx, ty) in module.types.iter().enumerate() {
        let relocated = reloc_type(ty, &types, &classes, &interfaces, &strings);
        types[idx] = merged.ty(relocated, identity.type_hashes[idx])?;
    }
    for (idx, app) in module.apps.iter().enumerate() {
        let relocated = TypeApp {
            types: app.types.iter().map(|t| types[*t as usize]).collect(),
            rows: app
                .rows
                .iter()
                .map(|row| reloc_row(row, &strings))
                .collect(),
        };
        apps[idx] = merged.app(relocated);
    }
    let mut reloc = Reloc {
        strings,
        bytes,
        types,
        selectors,
        apps,
        classes,
        interfaces,
        funcs: vec![u32::MAX; module.funcs.len()],
        slots: Vec::with_capacity(module.slots.len()),
    };
    // The function map resolves each imported declaration to one
    // provider definition. Each local function gets one arena entry.
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind == ImportKind::Class {
            continue;
        }
        let target = resolve_func_import(view, merged, module, import, path, idx, &reloc)?;
        reloc.funcs[import.def as usize] = target;
    }
    let mut created_funcs: Vec<u32> = Vec::new();
    for (idx, is_extern) in extern_funcs.iter().copied().enumerate() {
        if is_extern {
            continue;
        }
        let at = merged.funcs.len() as u32;
        // The placeholder keeps the index. The body fills after all
        // function indices are known.
        merged.funcs.push(Func {
            name: module.funcs[idx].name.clone(),
            type_params: 0,
            effect_params: 0,
            params: Vec::new(),
            param_muts: Vec::new(),
            param_names: Vec::new(),
            ret: 0,
            row: Vec::new(),
            captures: Vec::new(),
            local_types: Vec::new(),
            blocks: Vec::new(),
        });
        merged.func_hashes.push(identity.func_hashes[idx]);
        merged.func_bounds.push(Vec::new());
        reloc.funcs[idx] = at;
        created_funcs.push(idx as u32);
    }
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind == ImportKind::Class {
            check_class_import_contract(merged, module, import, path, idx, &reloc)?;
        }
    }
    for (slot, source) in module.slots.iter().enumerate() {
        let contract = reloc_slot_contract(&source.contract, &reloc);
        let slot_key = (slot_scope, source.key, source.contract_hash);
        let merged_slot = match merged.slot_by_contract.get(&slot_key).copied() {
            Some(existing) => {
                let found = &mut merged.slots[existing as usize];
                if found.contract_hash != source.contract_hash {
                    return Err(fail(format!(
                        "the slot {slot} of `{path}` has another contract"
                    )));
                }
                existing
            }
            None => {
                let index = merged.slots.len() as u32;
                merged.slots.push(SlotSpec {
                    binding: source.binding.clone(),
                    late: source.late,
                    key: source.key,
                    contract_hash: source.contract_hash,
                    contract,
                    initial: None,
                });
                merged.slot_by_contract.insert(slot_key, index);
                index
            }
        };
        reloc.slots.push(merged_slot);
    }
    // Fill the created definitions, and prove that every shared one
    // really is the definition its hash claims.
    for idx in &created_classes {
        let source = &module.classes[*idx as usize];
        let at = reloc.classes[*idx as usize] as usize;
        let filled = reloc_class(source, &reloc);
        merged.classes[at] = filled;
        let bounds = module
            .class_bounds
            .get(*idx as usize)
            .map(|items| reloc_bounds(items, &reloc))
            .unwrap_or_default();
        merged.class_bounds[at] = bounds;
    }
    for idx in created_interfaces.iter().chain(shared_interfaces.iter()) {
        let source = &module.interfaces[*idx as usize];
        let at = reloc.interfaces[*idx as usize] as usize;
        let filled = reloc_interface(source, &reloc);
        if created_interfaces.contains(idx) {
            merged.interfaces[at] = filled;
        } else if merged.interfaces[at] != filled {
            let provider = &view.interface_by_key[&source.key].1;
            return Err(fail(format!(
                "the interface `{}` arrives with two contracts, from `{provider}` and from `{path}`",
                source.key
            )));
        }
    }
    for idx in &created_funcs {
        let source = &module.funcs[*idx as usize];
        let at = reloc.funcs[*idx as usize] as usize;
        let filled = reloc_func(source, &reloc);
        let bounds = module
            .func_bounds
            .get(*idx as usize)
            .map(|items| reloc_bounds(items, &reloc))
            .unwrap_or_default();
        merged.funcs[at] = filled;
        merged.func_bounds[at] = bounds;
    }
    for source in &module.conformances {
        // An imported class carries its provider conformance set as
        // part of its declaration. The contract check compares that
        // set. Only the provider publishes those conformances.
        if extern_classes[source.class as usize] {
            continue;
        }
        let filled = reloc_conformance(source, &reloc);
        if !merged.conformances.contains(&filled) {
            merged.conformances.push(filled);
        }
    }
    let debug = lm_bytecode::debug::decode(&module.debug)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    lm_bytecode::debug::validate(&debug, module)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    merged
        .debug
        .append_relocated(&debug, &reloc.funcs, &reloc.classes)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    Ok(reloc)
}

fn reloc_class(source: &BcClass, reloc: &Reloc) -> BcClass {
    BcClass {
        name: source.name.clone(),
        key: source.key.clone(),
        is_final: source.is_final,
        is_frozen: source.is_frozen,
        parent: source
            .parent()
            .map(|parent| reloc.classes[parent as usize])
            .unwrap_or(NO_PARENT),
        parent_args: source
            .parent_args
            .iter()
            .map(|item| reloc.types[*item as usize])
            .collect(),
        type_params: source.type_params,
        kind: source.kind,
        fields: source
            .fields
            .iter()
            .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
            .collect(),
        field_defaults: source.field_defaults.clone(),
        own_start: source.own_start,
        has_init: source.has_init,
        methods: source
            .methods
            .iter()
            .map(|(selector, function)| {
                (
                    reloc.selectors[*selector as usize],
                    reloc.funcs[*function as usize],
                )
            })
            .collect(),
    }
}

/// Merge the named function bindings of one module (specification
/// 3.6). The table is exhaustive:
///
/// | Binding key | StructuralHash | Result |
/// | --- | --- | --- |
/// | same | same | share the binding and the code |
/// | same | different | reject: conflicting providers |
/// | different | same | keep both bindings, share the code |
/// | different | different | keep both bindings and both code objects |
///
/// Row 2 is the rule the generated constructor needs. A class
/// structural hash covers no constructor, so two providers of one
/// class key with two different constructors merge into one class.
/// Their constructors carry one binding key and two structural
/// hashes, and this rule rejects them.
fn merge_bindings(
    view: &mut NamespaceBuild,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_funcs = module.extern_funcs();
    for binding in &module.bindings {
        let local = binding.func as usize;
        if local >= module.funcs.len() {
            return Err(fail(format!(
                "the binding `{}` of `{path}` names a function outside the module",
                binding.key
            )));
        }
        if extern_funcs[local] {
            // An imported declaration carries no body, so the module
            // that declares it is not a provider of that name. A
            // The verifier rejects constructor bindings on imports.
            continue;
        }
        let hash = identity.func_hashes[local];
        match view.binding_version.get(&binding.key) {
            Some((seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the function `{}` arrives with two implementations, from \
                     `{provider}` and from `{path}`; rebuild both against one version",
                    binding.key
                )));
            }
            Some(_) => continue,
            None => {
                view.binding_version
                    .insert(binding.key.clone(), (hash, path.to_string()));
            }
        }
        view.bindings.push(lm_bytecode::FuncBinding {
            key: binding.key.clone(),
            func: reloc.funcs[local],
            class: if binding.class == lm_bytecode::NO_CLASS {
                lm_bytecode::NO_CLASS
            } else {
                reloc.classes[binding.class as usize]
            },
        });
    }
    Ok(())
}

/// Resolve one class import slot against the provided definitions.
fn resolve_class_import(
    view: &NamespaceBuild,
    import: &Import,
    path: &str,
    slot: usize,
) -> Result<u32, LinkError> {
    let key = (import.module.clone(), import.name.clone());
    check_pin(view, import, path, slot)?;
    view.class_exports.get(&key).copied().ok_or_else(|| {
        fail(format!(
            "`{path}` slot {slot} names the type `{}.{}`, which the module does \
             not export",
            import.module, import.name
        ))
    })
}

/// Compare the pinned interface hash with the provider export.
fn check_pin(
    view: &NamespaceBuild,
    import: &Import,
    path: &str,
    slot: usize,
) -> Result<(), LinkError> {
    // A method slot pins the interface hash of its class, so the
    // lookup drops the method name.
    let export_name = match import.kind {
        ImportKind::Method => import
            .name
            .rsplit_once('.')
            .map(|(class, _)| class.to_string())
            .unwrap_or_else(|| import.name.clone()),
        _ => import.name.clone(),
    };
    let key = (import.module.clone(), export_name.clone());
    let Some(found) = view.export_hash.get(&key) else {
        return Err(fail(format!(
            "`{path}` slot {slot} names `{}.{export_name}`, which the module does \
             not export",
            import.module
        )));
    };
    if *found != import.hash {
        return Err(fail(format!(
            "`{path}` slot {slot} pins an interface of `{}.{export_name}` that the \
             module no longer provides; rebuild the importing module",
            import.module
        )));
    }
    Ok(())
}

/// Resolve one function, constructor, or method import slot.
fn resolve_func_import(
    view: &NamespaceBuild,
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    reloc: &Reloc,
) -> Result<u32, LinkError> {
    check_pin(view, import, path, slot)?;
    let target = match import.kind {
        ImportKind::Func => {
            let key = (import.module.clone(), import.name.clone());
            view.func_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the function `{}.{}`, which the \
                     module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Ctor => {
            let key = (import.module.clone(), import.name.clone());
            view.ctor_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the constructor of `{}.{}`, which \
                     the module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Method => {
            let Some((class_name, method)) = import.name.rsplit_once('.') else {
                return Err(fail(format!(
                    "`{path}` slot {slot} names the method `{}` without a class",
                    import.name
                )));
            };
            let key = (import.module.clone(), class_name.to_string());
            let class = view.class_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names a method of `{}.{class_name}`, \
                     which the module does not export",
                    import.module
                ))
            })?;
            if method == "init" {
                let key = lm_bytecode::qualified_key(&import.module, &format!("{class_name}.init"));
                let target = view
                    .bindings
                    .iter()
                    .find(|binding| binding.key == key)
                    .map(|binding| binding.func)
                    .ok_or_else(|| {
                        fail(format!(
                            "`{path}` slot {slot} names the initializer of \
                             `{}.{class_name}`, which the module does not provide",
                            import.module
                        ))
                    })?;
                return check_function_import_contract(
                    tables, module, import, path, slot, target, reloc,
                )
                .map(|()| target);
            }
            // The local selector table holds the method name.
            module
                .selectors
                .iter()
                .position(|s| s == method)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         the module does not call"
                    ))
                })?;
            let selector = tables.selector_index.get(method).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the unknown method `{method}`"
                ))
            })?;
            tables.classes[class as usize]
                .methods
                .iter()
                .find(|(sel, _)| *sel == selector)
                .map(|(_, func)| *func)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         `{}.{class_name}` does not answer",
                        import.module
                    ))
                })
        }
        ImportKind::Class => unreachable!("a class slot never reaches the function map"),
    }?;
    check_function_import_contract(tables, module, import, path, slot, target, reloc)?;
    Ok(target)
}

/// Compare one sparse callable declaration with its provider.
fn check_function_import_contract(
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    target: u32,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let source = module
        .funcs
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no function declaration")))?;
    let found = tables
        .funcs
        .get(target as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no provider function")))?;
    let source_bounds = module
        .func_bounds
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no function bounds")))?;
    let params: Vec<u32> = source
        .params
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let captures: Vec<u32> = source
        .captures
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let matches = source.type_params == found.type_params
        && source.effect_params == found.effect_params
        && reloc_bounds(source_bounds, reloc) == tables.func_bounds[target as usize]
        && params == found.params
        && source.param_muts == found.param_muts
        && source.param_names == found.param_names
        && reloc.types[source.ret as usize] == found.ret
        && reloc_row(&source.row, &reloc.strings) == found.row
        && captures == found.captures;
    if !matches {
        return Err(fail(format!(
            "`{path}` slot {slot} declares another callable contract for `{}.{}`",
            import.module, import.name
        )));
    }
    Ok(())
}

/// Compare one sparse class declaration with its provider.
fn check_class_import_contract(
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let source = module
        .classes
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no class declaration")))?;
    let target_index = reloc.classes[import.def as usize];
    let found = tables
        .classes
        .get(target_index as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no provider class")))?;
    let parent = source
        .parent()
        .map(|parent| reloc.classes[parent as usize])
        .unwrap_or(NO_PARENT);
    let parent_args: Vec<u32> = source
        .parent_args
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let fields: Vec<(String, u32)> = source
        .fields
        .iter()
        .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
        .collect();
    let bounds = module
        .class_bounds
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no class bounds")))?;
    let layout_matches = source.name == found.name
        && source.key == found.key
        && source.is_final == found.is_final
        && source.is_frozen == found.is_frozen
        && parent == found.parent
        && parent_args == found.parent_args
        && source.type_params == found.type_params
        && source.kind == found.kind
        && fields == found.fields
        && source.field_defaults == found.field_defaults
        && source.own_start == found.own_start
        && source.has_init == found.has_init
        && reloc_bounds(bounds, reloc) == tables.class_bounds[target_index as usize];
    let methods_match = source.methods.iter().all(|(selector, function)| {
        let method = (
            reloc.selectors[*selector as usize],
            reloc.funcs[*function as usize],
        );
        found.methods.contains(&method)
    });
    let conformances_match = module
        .conformances
        .iter()
        .filter(|item| item.class == import.def)
        .map(|item| reloc_conformance(item, reloc))
        .all(|item| tables.conformances.contains(&item));
    if !(layout_matches && methods_match && conformances_match) {
        return Err(fail(format!(
            "`{path}` slot {slot} declares another class contract for `{}.{}`",
            import.module, import.name
        )));
    }
    Ok(())
}

/// Record the exports of one module for the modules that follow.
fn register_exports(
    view: &mut NamespaceBuild,
    module: &Module,
    interface: &Interface,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    for export in &module.exports {
        let key = (path.to_string(), export.name.clone());
        if view.export_hash.contains_key(&key) {
            return Err(fail(format!(
                "the module `{path}` exports the name `{}` twice",
                export.name
            )));
        }
        // The decoder bounds these indices, and a hand-built module
        // reaches the linker without a decoder, so the bound is
        // checked here too.
        let limit = if export.kind.is_class() {
            reloc.classes.len()
        } else if export.kind.is_interface() {
            reloc.interfaces.len()
        } else {
            reloc.funcs.len()
        };
        if export.def as usize >= limit
            || (export.ctor != lm_bytecode::NO_CTOR && export.ctor as usize >= reloc.funcs.len())
        {
            return Err(fail(format!(
                "the export `{}` of `{path}` names a definition outside the \
                 module",
                export.name
            )));
        }
        // A module exports what it defines. A re-export of an
        // imported declaration would give one definition two
        // qualified names, and a pin would then name a module that
        // does not hold the definition.
        let imported = if export.kind.is_class() {
            extern_classes[export.def as usize]
        } else if export.kind.is_interface() {
            false
        } else {
            extern_funcs[export.def as usize]
        };
        if imported {
            return Err(fail(format!(
                "the module `{path}` exports `{}`, which it imports",
                export.name
            )));
        }
        let entry = interface.find(&export.name).ok_or_else(|| {
            fail(format!(
                "the interface of `{path}` does not describe the export `{}`",
                export.name
            ))
        })?;
        view.export_hash.insert(key.clone(), entry.iface_hash);
        if export.kind.is_class() {
            view.class_exports
                .insert(key.clone(), reloc.classes[export.def as usize]);
            if export.ctor != lm_bytecode::NO_CTOR {
                view.ctor_exports
                    .insert(key, reloc.funcs[export.ctor as usize]);
            }
        } else if export.kind.is_interface() {
            view.interface_exports
                .insert(key, reloc.interfaces[export.def as usize]);
        } else {
            view.func_exports
                .insert(key, reloc.funcs[export.def as usize]);
        }
    }
    Ok(())
}

fn reloc_row(row: &[BcRow], strings: &[u32]) -> Vec<BcRow> {
    row.iter()
        .map(|elem| match elem {
            BcRow::Op(idx) => BcRow::Op(strings[*idx as usize]),
            BcRow::Var(v) => BcRow::Var(*v),
        })
        .collect()
}

fn reloc_type(
    ty: &BcType,
    types: &[u32],
    classes: &[u32],
    interfaces: &[u32],
    strings: &[u32],
) -> BcType {
    match ty {
        BcType::Class(c) => BcType::Class(classes[*c as usize]),
        BcType::Inst(c, args) => BcType::Inst(
            classes[*c as usize],
            args.iter().map(|a| types[*a as usize]).collect(),
        ),
        BcType::List(e) => BcType::List(types[*e as usize]),
        BcType::Map(k, v) => BcType::Map(types[*k as usize], types[*v as usize]),
        BcType::Tuple(elems) => BcType::Tuple(elems.iter().map(|e| types[*e as usize]).collect()),
        BcType::Fn(params, muts, ret, row) => BcType::Fn(
            params.iter().map(|p| types[*p as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row, strings),
        ),
        BcType::Callback(params, muts, ret, row) => BcType::Callback(
            params.iter().map(|p| types[*p as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row, strings),
        ),
        BcType::Projection {
            base,
            interface,
            assoc,
        } => BcType::Projection {
            base: types[*base as usize],
            interface: interfaces[*interface as usize],
            assoc: *assoc,
        },
        BcType::Run(t) => BcType::Run(types[*t as usize]),
        BcType::Wait(t) => BcType::Wait(types[*t as usize]),
        BcType::RunSnapshot(t) => BcType::RunSnapshot(types[*t as usize]),
        BcType::PendingCall(a, r) => BcType::PendingCall(types[*a as usize], types[*r as usize]),
        BcType::Handle(m, r) => BcType::Handle(types[*m as usize], types[*r as usize]),
        BcType::Op(op, f) => BcType::Op(*op, types[*f as usize]),
        other => other.clone(),
    }
}

fn reloc_interface_use(application: &BcInterfaceUse, reloc: &Reloc) -> BcInterfaceUse {
    BcInterfaceUse {
        interface: reloc.interfaces[application.interface as usize],
        types: application
            .types
            .iter()
            .map(|item| reloc.types[*item as usize])
            .collect(),
        rows: application
            .rows
            .iter()
            .map(|row| reloc_row(row, &reloc.strings))
            .collect(),
    }
}

fn reloc_bounds(bounds: &[Vec<BcInterfaceUse>], reloc: &Reloc) -> Vec<Vec<BcInterfaceUse>> {
    bounds
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|item| reloc_interface_use(item, reloc))
                .collect()
        })
        .collect()
}

fn reloc_callable_contract(source: &BcCallableContract, reloc: &Reloc) -> BcCallableContract {
    BcCallableContract {
        type_params: source.type_params,
        effect_params: source.effect_params,
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        params: source
            .params
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        param_muts: source.param_muts.clone(),
        ret: reloc.types[source.ret as usize],
        row: reloc_row(&source.row, &reloc.strings),
    }
}

fn reloc_slot_contract(source: &SlotContract, reloc: &Reloc) -> SlotContract {
    match source {
        SlotContract::Function(contract) => {
            SlotContract::Function(reloc_callable_contract(contract, reloc))
        }
        SlotContract::Method(contract) => {
            SlotContract::Method(reloc_callable_contract(contract, reloc))
        }
        SlotContract::Class {
            type_params,
            abi,
            ty,
            constructor,
        } => SlotContract::Class {
            type_params: *type_params,
            abi: *abi,
            ty: reloc.types[*ty as usize],
            constructor: reloc_callable_contract(constructor, reloc),
        },
        SlotContract::Value { ty } => SlotContract::Value {
            ty: reloc.types[*ty as usize],
        },
        SlotContract::Process { message, result } => SlotContract::Process {
            message: reloc.types[*message as usize],
            result: reloc.types[*result as usize],
        },
    }
}

fn reloc_slot_target(source: SlotTarget, reloc: &Reloc) -> SlotTarget {
    match source {
        SlotTarget::Function(func) => SlotTarget::Function(reloc.funcs[func as usize]),
        SlotTarget::Class { class, constructor } => SlotTarget::Class {
            class: reloc.classes[class as usize],
            constructor: reloc.funcs[constructor as usize],
        },
    }
}

fn reloc_interface(source: &BcInterface, reloc: &Reloc) -> BcInterface {
    BcInterface {
        name: source.name.clone(),
        key: source.key.clone(),
        type_params: source.type_params,
        effect_params: source.effect_params,
        generic_is_effect: source.generic_is_effect.clone(),
        parents: source
            .parents
            .iter()
            .map(|parent| reloc_interface_use(parent, reloc))
            .collect(),
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        associated: source
            .associated
            .iter()
            .map(|item| BcAssociated {
                name: item.name.clone(),
                bounds: item
                    .bounds
                    .iter()
                    .map(|bound| reloc_interface_use(bound, reloc))
                    .collect(),
            })
            .collect(),
        methods: source
            .methods
            .iter()
            .map(|method| BcInterfaceMethod {
                selector: reloc.selectors[method.selector as usize],
                mut_self: method.mut_self,
                type_params: method.type_params,
                type_bounds: reloc_bounds(&method.type_bounds, reloc),
                effect_params: method.effect_params,
                premises: method
                    .premises
                    .iter()
                    .map(|premise| lm_bytecode::BcTypePremise {
                        subject: reloc.types[premise.subject as usize],
                        bounds: premise
                            .bounds
                            .iter()
                            .map(|bound| reloc_interface_use(bound, reloc))
                            .collect(),
                    })
                    .collect(),
                params: method
                    .params
                    .iter()
                    .map(|item| reloc.types[*item as usize])
                    .collect(),
                param_muts: method.param_muts.clone(),
                param_names: method.param_names.clone(),
                ret: reloc.types[method.ret as usize],
                row: reloc_row(&method.row, &reloc.strings),
                default: if method.default == lm_bytecode::NO_FUNC {
                    lm_bytecode::NO_FUNC
                } else {
                    reloc.funcs[method.default as usize]
                },
            })
            .collect(),
    }
}

fn reloc_conformance(source: &BcConformance, reloc: &Reloc) -> BcConformance {
    BcConformance {
        class: reloc.classes[source.class as usize],
        application: reloc_interface_use(&source.application, reloc),
        premises: source
            .premises
            .iter()
            .map(|premise| lm_bytecode::BcConformancePremise {
                param: premise.param,
                bounds: premise
                    .bounds
                    .iter()
                    .map(|bound| reloc_interface_use(bound, reloc))
                    .collect(),
            })
            .collect(),
        associated: source
            .associated
            .iter()
            .map(|item| reloc.types[*item as usize])
            .collect(),
        method_overrides: source.method_overrides.clone(),
    }
}

fn reloc_func(func: &Func, reloc: &Reloc) -> Func {
    Func {
        name: func.name.clone(),
        type_params: func.type_params,
        effect_params: func.effect_params,
        params: func
            .params
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        param_muts: func.param_muts.clone(),
        param_names: func.param_names.clone(),
        ret: reloc.types[func.ret as usize],
        row: reloc_row(&func.row, &reloc.strings),
        captures: func
            .captures
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        local_types: func
            .local_types
            .iter()
            .map(|t| reloc.types[*t as usize])
            .collect(),
        blocks: func
            .blocks
            .iter()
            .map(|block| block.iter().map(|i| reloc_instr(i, reloc)).collect())
            .collect(),
    }
}

/// Relocate one instruction. The match is exhaustive without a
/// wildcard arm, so a future instruction with a module-global operand
/// fails to compile until its relocation is decided.
fn reloc_instr(instr: &Instr, reloc: &Reloc) -> Instr {
    match instr {
        Instr::ConstStr(idx) => Instr::ConstStr(reloc.strings[*idx as usize]),
        Instr::ConstBytes(idx) => Instr::ConstBytes(reloc.bytes[*idx as usize]),
        Instr::Call(f) => Instr::Call(reloc.funcs[*f as usize]),
        Instr::CallG { func, app } => Instr::CallG {
            func: reloc.funcs[*func as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::CallVirtual { selector, argc } => Instr::CallVirtual {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
        },
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => Instr::CallVirtualG {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
            app: reloc.apps[*app as usize],
        },
        Instr::MakeClosure { func, captures } => Instr::MakeClosure {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        // The reply type index names a module type, so it moves with
        // the type table.
        Instr::Perform { op, argc, reply_ty } => Instr::Perform {
            op: *op,
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        Instr::PerformValue { argc, reply_ty } => Instr::PerformValue {
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        Instr::New(c) => Instr::New(reloc.classes[*c as usize]),
        Instr::NewG { class, app } => Instr::NewG {
            class: reloc.classes[*class as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::TupleNew { ty, count } => Instr::TupleNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::ListNew { ty, count } => Instr::ListNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::MapNew { ty, count } => Instr::MapNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::IsType(ty) => Instr::IsType(reloc.types[*ty as usize]),
        Instr::CastType(ty) => Instr::CastType(reloc.types[*ty as usize]),
        Instr::MapPut { ty, discard } => Instr::MapPut {
            ty: reloc.types[*ty as usize],
            discard: *discard,
        },
        // Every remaining operand is function-local or manifest-dense.
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
        | Instr::Numeric(_)
        | Instr::LoadLocal(_)
        | Instr::StoreLocal(_)
        | Instr::Pop
        | Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
        | Instr::Neg
        | Instr::Not
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::Native(lm_bytecode::NativeInstr::EqStr)
        | Instr::Native(lm_bytecode::NativeInstr::NeStr)
        | Instr::Native(lm_bytecode::NativeInstr::StrByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::StrCharCount)
        | Instr::Native(lm_bytecode::NativeInstr::StrConcat)
        | Instr::Native(lm_bytecode::NativeInstr::StrStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::StrContains)
        | Instr::Native(lm_bytecode::NativeInstr::StrFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex)
        | Instr::Native(lm_bytecode::NativeInstr::TextAtByte)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrim)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd)
        | Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextToUpperAscii)
        | Instr::Native(lm_bytecode::NativeInstr::TextReplace)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
        | Instr::Native(lm_bytecode::NativeInstr::TextParseIntValue)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadStart)
        | Instr::Native(lm_bytecode::NativeInstr::TextPadEnd)
        | Instr::Native(lm_bytecode::NativeInstr::TextHash)
        | Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesContains)
        | Instr::Native(lm_bytecode::NativeInstr::TextSplit)
        | Instr::Native(lm_bytecode::NativeInstr::TextLines)
        | Instr::Native(lm_bytecode::NativeInstr::TextAt)
        | Instr::Native(lm_bytecode::NativeInstr::TextSlice)
        | Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary)
        | Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextBytes)
        | Instr::Native(lm_bytecode::NativeInstr::TextLt)
        | Instr::Native(lm_bytecode::NativeInstr::TextLe)
        | Instr::Native(lm_bytecode::NativeInstr::TextGt)
        | Instr::Native(lm_bytecode::NativeInstr::TextGe)
        | Instr::Native(lm_bytecode::NativeInstr::SubstringToString)
        | Instr::Native(lm_bytecode::NativeInstr::CharCodepoint)
        | Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len)
        | Instr::Native(lm_bytecode::NativeInstr::EqChar)
        | Instr::Native(lm_bytecode::NativeInstr::NeChar)
        | Instr::Native(lm_bytecode::NativeInstr::LtChar)
        | Instr::Native(lm_bytecode::NativeInstr::LeChar)
        | Instr::Native(lm_bytecode::NativeInstr::GtChar)
        | Instr::Native(lm_bytecode::NativeInstr::GeChar)
        | Instr::EqRef
        | Instr::EqValue
        | Instr::NeValue
        | Instr::NeRef
        | Instr::CallValue { .. }
        | Instr::LoadCapture(_)
        | Instr::LoadField(_)
        | Instr::StoreField(_)
        | Instr::TupleGet(_)
        | Instr::ListLen
        | Instr::ListAt
        | Instr::ListPush
        | Instr::MapLen
        | Instr::MapHas
        | Instr::MapAt
        | Instr::Native(lm_bytecode::NativeInstr::SbNew)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendStr)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendInt)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendBool)
        | Instr::Native(lm_bytecode::NativeInstr::SbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::SbLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbClear)
        | Instr::Native(lm_bytecode::NativeInstr::SbAppendChar)
        | Instr::Native(lm_bytecode::NativeInstr::SbByteLen)
        | Instr::Native(lm_bytecode::NativeInstr::SbFinish)
        | Instr::Native(lm_bytecode::NativeInstr::BbNew)
        | Instr::Native(lm_bytecode::NativeInstr::BbAppend)
        | Instr::Native(lm_bytecode::NativeInstr::BbLen)
        | Instr::Native(lm_bytecode::NativeInstr::BbBuild)
        | Instr::Native(lm_bytecode::NativeInstr::BbExtend)
        | Instr::Native(lm_bytecode::NativeInstr::BbReserve)
        | Instr::Native(lm_bytecode::NativeInstr::BbClear)
        | Instr::Native(lm_bytecode::NativeInstr::BbFinish)
        | Instr::Native(lm_bytecode::NativeInstr::BbAt)
        | Instr::Native(lm_bytecode::NativeInstr::BbFindFrom)
        | Instr::Native(lm_bytecode::NativeInstr::BytesNew)
        | Instr::Native(lm_bytecode::NativeInstr::BytesLen)
        | Instr::Native(lm_bytecode::NativeInstr::BytesText)
        | Instr::Native(lm_bytecode::NativeInstr::BytesAt)
        | Instr::Native(lm_bytecode::NativeInstr::BytesGet)
        | Instr::Native(lm_bytecode::NativeInstr::BytesSlice)
        | Instr::Native(lm_bytecode::NativeInstr::BytesConcat)
        | Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith)
        | Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHex)
        | Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8)
        | Instr::Native(lm_bytecode::NativeInstr::EqBytes)
        | Instr::Native(lm_bytecode::NativeInstr::NeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::LtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::LeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GtBytes)
        | Instr::Native(lm_bytecode::NativeInstr::GeBytes)
        | Instr::Native(lm_bytecode::NativeInstr::BytesCompact)
        | Instr::Native(lm_bytecode::NativeInstr::BytesTextView)
        | Instr::Native(lm_bytecode::NativeInstr::BytesHash)
        | Instr::Native(lm_bytecode::NativeInstr::HashCombine)
        | Instr::Native(lm_bytecode::NativeInstr::HashUnorderedCombine)
        | Instr::Freeze
        | Instr::EqDigest
        | Instr::NeDigest
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfTrue(_)
        | Instr::Return
        | Instr::OpConst(_)
        | Instr::TableEdit { .. }
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::FaultDenied
        | Instr::RaiseUserPanic
        | Instr::RaiseAssertionFailed
        | Instr::RaiseFault
        | Instr::RequestOp
        | Instr::Unreachable => *instr,
        Instr::Digest { ty } => Instr::Digest {
            ty: reloc.types[*ty as usize],
        },
        Instr::AsCall { op, ty } => Instr::AsCall {
            op: *op,
            ty: reloc.types[*ty as usize],
        },
        Instr::CallInterface { site, recv_ty, app } => {
            let (interface, method) = lm_bytecode::unpack_interface_call_site(*site);
            let relocated = reloc.interfaces[interface as usize];
            Instr::CallInterface {
                site: lm_bytecode::pack_interface_call_site(relocated, method)
                    .expect("the linked interface count was checked"),
                recv_ty: reloc.types[*recv_ty as usize],
                app: if *app == lm_bytecode::NO_APP {
                    lm_bytecode::NO_APP
                } else {
                    reloc.apps[*app as usize]
                },
            }
        }
        Instr::Extended(instr) => Instr::Extended(reloc_extended(instr, reloc)),
    }
}

fn reloc_extended(instr: &ExtendedInstr, reloc: &Reloc) -> ExtendedInstr {
    match instr {
        ExtendedInstr::MakeCallback { func, captures } => ExtendedInstr::MakeCallback {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        ExtendedInstr::FunctionCode { func } => ExtendedInstr::FunctionCode {
            func: reloc.funcs[*func as usize],
        },
        ExtendedInstr::ClassCode { class } => ExtendedInstr::ClassCode {
            class: reloc.classes[*class as usize],
        },
        ExtendedInstr::CodeSource { ty } => ExtendedInstr::CodeSource {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::CodeDefinition => ExtendedInstr::CodeDefinition,
        ExtendedInstr::FaultSite { ty } => ExtendedInstr::FaultSite {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::FaultTrace { ty } => ExtendedInstr::FaultTrace {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionSome { ty } => ExtendedInstr::OptionSome {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionNone { ty } => ExtendedInstr::OptionNone {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionPayload { ty } => ExtendedInstr::OptionPayload {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::ListGet { ty } => ExtendedInstr::ListGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapGet { ty } => ExtendedInstr::MapGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::ListPop { ty } => ExtendedInstr::ListPop {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapRemove { ty } => ExtendedInstr::MapRemove {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::DynPack { ty } => ExtendedInstr::DynPack {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::PrepareWait { op_argc, reply_ty } => ExtendedInstr::PrepareWait {
            op_argc: *op_argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        ExtendedInstr::CallSlot { slot, app } => ExtendedInstr::CallSlot {
            slot: reloc.slots[*slot as usize],
            app: if *app == lm_bytecode::NO_APP {
                lm_bytecode::NO_APP
            } else {
                reloc.apps[*app as usize]
            },
        },
        ExtendedInstr::NewSlot { slot, app } => ExtendedInstr::NewSlot {
            slot: reloc.slots[*slot as usize],
            app: if *app == lm_bytecode::NO_APP {
                lm_bytecode::NO_APP
            } else {
                reloc.apps[*app as usize]
            },
        },
        ExtendedInstr::LoadSlot { slot } => ExtendedInstr::LoadSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::SendSlot { slot } => ExtendedInstr::SendSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::AsCallback
        | ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::MapEpoch
        | ExtendedInstr::MapIterLen
        | ExtendedInstr::MapNextIndex
        | ExtendedInstr::SealInstance
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::ListCapacity
        | ExtendedInstr::ListSet
        | ExtendedInstr::ListInsert
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListReserve
        | ExtendedInstr::ListTruncate
        | ExtendedInstr::ListContains
        | ExtendedInstr::ListReorder
        | ExtendedInstr::MapClear
        | ExtendedInstr::MapReserve
        | ExtendedInstr::MapProbe
        | ExtendedInstr::MapProbeFound
        | ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeSetValue
        | ExtendedInstr::MapProbeRemove
        | ExtendedInstr::MapInsertHashed
        | ExtendedInstr::MapWriteGuard
        | ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::DynRender
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::SyntaxToTree => *instr,
    }
}
