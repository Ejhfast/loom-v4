//! Artifact environments and exact dependency resolution.

use crate::arena::{build_definition_artifact, prepare_definition_export};
use crate::collect;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lm_bytecode::artifact::{Artifact, ArtifactId, LinkUnit, CORE_MODULE_PATH};
use lm_bytecode::interface::Interface;
use lm_bytecode::{Export, ImportKind, Module};

pub fn collect_compiled_unit(module: &Module) -> Result<Module, String> {
    collect::collect_compiled_unit(module)
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
        .chain(
            module
                .reflections
                .iter()
                .map(|reflection| reflection.name.clone()),
        )
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// An immutable set of modules for one link step.
#[derive(Debug, Clone, Default)]
pub struct FrozenLinkEnv {
    pub(crate) units: BTreeMap<String, Arc<LinkUnit>>,
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
    let (export, definition) = prepare_definition_export(source.module(), selection)?;
    build_definition_artifact(source, export, definition, &env, bundle)
}

impl FrozenLinkEnv {
    /// Return the module at one canonical path.
    pub fn unit(&self, path: &str) -> Option<&LinkUnit> {
        self.units.get(path).map(Arc::as_ref)
    }

    /// Return the shared unit at one canonical path.
    pub fn unit_store(&self, path: &str) -> Option<Arc<LinkUnit>> {
        self.units.get(path).cloned()
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

pub(crate) fn fail(message: impl Into<String>) -> LinkError {
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

pub(crate) fn collect_environment_with_root(
    root: &str,
    env: &FrozenLinkEnv,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
    definition: Option<(collect::DefinitionRoot, Export)>,
) -> Result<FrozenLinkEnv, LinkError> {
    let order = link_order(root, env)?;
    let mut requests: BTreeMap<String, Vec<(String, ImportKind)>> = BTreeMap::new();
    let mut complete = BTreeSet::new();
    let mut selected: BTreeMap<String, Module> = BTreeMap::new();

    for path in order.iter().rev() {
        let unit = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
        if path == CORE_MODULE_PATH {
            continue;
        }
        let result = if path == root {
            match definition {
                Some((definition, ref export)) => {
                    collect::collect_link_definition(unit.module(), definition, export.clone())
                }
                None => collect::collect_link_root(unit.module()),
            }
        } else if complete.remove(path) {
            requests.remove(path);
            collect::collect_compiled_unit(unit.module())
        } else {
            let Some(exports) = requests.remove(path) else {
                continue;
            };
            collect::collect_link_exports(unit.module(), &exports)
        };
        let module = result
            .map_err(|error| fail(format!("the module `{path}` does not collect: {error}")))?;
        for import in &module.imports {
            requests
                .entry(import.module.clone())
                .or_default()
                .push((import.name.clone(), import.kind));
        }
        complete.extend(
            module
                .reflections
                .iter()
                .map(|reflection| reflection.name.clone()),
        );
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
        let Some(module) = selected.remove(path) else {
            continue;
        };
        let source = env
            .unit(path)
            .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?;
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
pub(crate) fn validate_untrusted_units(
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

pub(crate) fn artifact_from_order(
    root: &str,
    env: &FrozenLinkEnv,
    order: &[String],
    embed_core: bool,
) -> Result<Artifact, LinkError> {
    let root_unit = env
        .unit_store(root)
        .ok_or_else(|| fail(format!("the module `{root}` is not bound")))?;
    let mut embedded = Vec::new();
    for path in order {
        if path == root || (!embed_core && path == CORE_MODULE_PATH) {
            continue;
        }
        embedded.push(
            env.unit_store(path)
                .ok_or_else(|| fail(format!("the module `{path}` is not bound")))?,
        );
    }
    Artifact::new_shared(root_unit, embedded)
        .map_err(|error| fail(format!("the artifact graph is invalid: {error}")))
}

/// The link order: every imported module before its importer. The
/// walk rejects a cycle and an unbound module.
pub(crate) fn link_order(root: &str, env: &FrozenLinkEnv) -> Result<Vec<String>, LinkError> {
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
