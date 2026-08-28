//! Semantic artifact identities and exact module dependencies.
//!
//! A `LinkUnit` contains one module and its exact dependencies.
//! An `Artifact` contains one root unit and its embedded dependency units.

use crate::identity::{module_identity_with_bundle, IdentityError, ModuleIdentity};
use crate::interface::{interface_identity, Interface};
use crate::{hash, Module};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

mod codec;

pub use codec::{
    decode, decode_with_bundle, encode, encode_with_bundle, encoded_id_with_bundle,
    ArtifactDecodeError, ArtifactEncodeError, ArtifactLimits, FORMAT_VERSION,
};

const ARTIFACT_ID_TAG: &[u8] = b"lm-artifact-id-v2\0";

/// The canonical module path of the standard core.
pub const CORE_MODULE_PATH: &str = "core";

/// The semantic identity of one link-unit closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactId([u8; 32]);

impl ArtifactId {
    /// Construct an identity from its canonical bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> ArtifactId {
        ArtifactId(bytes)
    }

    /// Return the canonical identity bytes.
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Borrow the canonical identity bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_digest(out, &self.0)
    }
}

/// One exact dependency on a canonical module path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDependency {
    module_path: String,
    artifact: ArtifactId,
}

impl ArtifactDependency {
    /// Create one dependency binding.
    pub fn new(
        module_path: impl Into<String>,
        artifact: ArtifactId,
    ) -> Result<ArtifactDependency, ArtifactError> {
        let module_path = module_path.into();
        validate_module_path(&module_path)?;
        Ok(ArtifactDependency {
            module_path,
            artifact,
        })
    }

    /// Return the canonical dependency module path.
    pub fn module_path(&self) -> &str {
        &self.module_path
    }

    /// Return the exact dependency identity.
    pub fn artifact(&self) -> ArtifactId {
        self.artifact
    }
}

/// One compiled module, its interface, and exact direct dependencies.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkUnit {
    id: ArtifactId,
    bundle_digest: [u8; 32],
    identity: ModuleIdentity,
    path: String,
    module: Module,
    interface: Interface,
    dependencies: Vec<ArtifactDependency>,
}

impl LinkUnit {
    /// Create one unit from its canonical module.
    pub fn from_module(
        module_path: impl Into<String>,
        module: Module,
        dependencies: Vec<ArtifactDependency>,
    ) -> Result<LinkUnit, ArtifactError> {
        let bundle = lm_abi::standard_bundle();
        LinkUnit::from_module_with_bundle(module_path, module, dependencies, &bundle)
    }

    /// Create one unit from its canonical module under one ABI bundle.
    pub fn from_module_with_bundle(
        module_path: impl Into<String>,
        module: Module,
        mut dependencies: Vec<ArtifactDependency>,
        bundle: &lm_abi::AbiBundle,
    ) -> Result<LinkUnit, ArtifactError> {
        let module_path = module_path.into();
        validate_unit_path(&module_path)?;
        canonicalize_dependencies(&mut dependencies)?;
        let identity = module_identity_with_bundle(&module, bundle)?;
        let interface = crate::interface::derive_interface_with_bundle(
            &module,
            &identity,
            &module_path,
            bundle,
        )
        .map_err(ArtifactError::InvalidModuleSurface)?;
        let id = compute_artifact_id(&module_path, &module, &identity, &interface, &dependencies)?;
        Ok(LinkUnit {
            id,
            bundle_digest: bundle.digest(),
            identity,
            path: module_path,
            module,
            interface,
            dependencies,
        })
    }

    /// Create one unit under the standard ABI bundle.
    pub fn new(
        module_path: impl Into<String>,
        module: Module,
        interface: Interface,
        dependencies: Vec<ArtifactDependency>,
    ) -> Result<LinkUnit, ArtifactError> {
        let bundle = lm_abi::standard_bundle();
        LinkUnit::new_with_bundle(module_path, module, interface, dependencies, &bundle)
    }

    /// Create one unit under an explicit ABI bundle.
    pub fn new_with_bundle(
        module_path: impl Into<String>,
        module: Module,
        interface: Interface,
        mut dependencies: Vec<ArtifactDependency>,
        bundle: &lm_abi::AbiBundle,
    ) -> Result<LinkUnit, ArtifactError> {
        let module_path = module_path.into();
        validate_unit_path(&module_path)?;
        if interface.module_path != module_path {
            return Err(ArtifactError::InterfacePathMismatch {
                unit: module_path,
                interface: interface.module_path,
            });
        }
        canonicalize_dependencies(&mut dependencies)?;
        let identity = module_identity_with_bundle(&module, bundle)?;
        let id = compute_artifact_id(&module_path, &module, &identity, &interface, &dependencies)?;
        Ok(LinkUnit {
            id,
            bundle_digest: bundle.digest(),
            identity,
            path: module_path,
            module,
            interface,
            dependencies,
        })
    }

    /// Return the semantic artifact identity.
    pub fn id(&self) -> ArtifactId {
        self.id
    }

    /// Return the canonical module path.
    pub fn module_path(&self) -> &str {
        &self.path
    }

    /// Return the bytecode payload.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Return the recomputed module identity.
    pub fn identity(&self) -> &ModuleIdentity {
        &self.identity
    }

    /// Return the module interface.
    pub fn interface(&self) -> &Interface {
        &self.interface
    }

    pub(crate) fn bundle_digest(&self) -> [u8; 32] {
        self.bundle_digest
    }

    /// Return the canonical dependency bindings.
    pub fn dependencies(&self) -> &[ArtifactDependency] {
        &self.dependencies
    }

    /// Consume the unit and return its semantic parts.
    pub fn into_parts(self) -> (String, Module, Interface, Vec<ArtifactDependency>) {
        (self.path, self.module, self.interface, self.dependencies)
    }
}

/// One root artifact and its embedded dependency units.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    root: ArtifactId,
    units: Arc<[Arc<LinkUnit>]>,
}

impl Artifact {
    /// Create one artifact from a root and embedded dependencies.
    pub fn new(root: LinkUnit, embedded: Vec<LinkUnit>) -> Result<Artifact, ArtifactGraphError> {
        Artifact::new_shared(Arc::new(root), embedded.into_iter().map(Arc::new).collect())
    }

    /// Create one artifact from shared link units.
    pub fn new_shared(
        root: Arc<LinkUnit>,
        embedded: Vec<Arc<LinkUnit>>,
    ) -> Result<Artifact, ArtifactGraphError> {
        let root_id = root.id();
        let mut units = Vec::with_capacity(embedded.len().saturating_add(1));
        units.push(root);
        units.extend(embedded);
        Artifact::from_shared_units(root_id, units)
    }

    pub(crate) fn from_units(
        root: ArtifactId,
        units: Vec<LinkUnit>,
    ) -> Result<Artifact, ArtifactGraphError> {
        Artifact::from_shared_units(root, units.into_iter().map(Arc::new).collect())
    }

    fn from_shared_units(
        root: ArtifactId,
        mut units: Vec<Arc<LinkUnit>>,
    ) -> Result<Artifact, ArtifactGraphError> {
        units.sort_by_key(|unit| unit.id());
        for pair in units.windows(2) {
            if pair[0].id() == pair[1].id() {
                return Err(ArtifactGraphError::DuplicateUnit(pair[0].id()));
            }
        }
        let mut module_paths = BTreeMap::new();
        for unit in &units {
            if module_paths
                .insert(unit.module_path().to_string(), unit.id())
                .is_some()
            {
                return Err(ArtifactGraphError::DuplicateModulePath(
                    unit.module_path().to_string(),
                ));
            }
        }
        let artifact = Artifact {
            root,
            units: units.into(),
        };
        artifact.validate_graph()?;
        Ok(artifact)
    }

    /// Return the root identity.
    pub fn id(&self) -> ArtifactId {
        self.root
    }

    /// Return the root unit.
    pub fn root(&self) -> &LinkUnit {
        self.unit(self.root)
            .expect("artifact graph validation keeps the root unit")
    }

    /// Return all embedded units, including the root.
    pub fn units(&self) -> &[Arc<LinkUnit>] {
        &self.units
    }

    /// Find one embedded unit by semantic identity.
    pub fn unit(&self, id: ArtifactId) -> Option<&LinkUnit> {
        self.units
            .binary_search_by_key(&id, |unit| unit.id())
            .ok()
            .map(|index| self.units[index].as_ref())
    }

    /// Consume the artifact and return shared unit stores.
    pub fn into_units(self) -> (ArtifactId, Vec<Arc<LinkUnit>>) {
        (self.root, self.units.iter().cloned().collect())
    }

    fn validate_graph(&self) -> Result<(), ArtifactGraphError> {
        let index: BTreeMap<ArtifactId, u32> = self
            .units
            .iter()
            .enumerate()
            .map(|(index, unit)| (unit.id(), index as u32))
            .collect();
        let Some(root) = index.get(&self.root).copied() else {
            return Err(ArtifactGraphError::MissingRoot(self.root));
        };
        let mut successors = vec![Vec::new(); self.units.len()];
        for (unit_index, unit) in self.units.iter().enumerate() {
            for dependency in &unit.dependencies {
                if let Some(target) = index.get(&dependency.artifact) {
                    let target_unit = &self.units[*target as usize];
                    if target_unit.module_path() != dependency.module_path() {
                        return Err(ArtifactGraphError::DependencyPathMismatch {
                            module_path: dependency.module_path().to_string(),
                            artifact: dependency.artifact(),
                        });
                    }
                    successors[unit_index].push(*target);
                }
            }
        }
        let mut reached = vec![false; self.units.len()];
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if reached[node as usize] {
                continue;
            }
            reached[node as usize] = true;
            work.extend(successors[node as usize].iter().copied());
        }
        if let Some((index, _)) = reached.iter().enumerate().find(|(_, item)| !**item) {
            return Err(ArtifactGraphError::UnreachableUnit(self.units[index].id()));
        }
        reject_cycles(&self.units, &successors)
    }
}

/// An invalid artifact package graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactGraphError {
    DuplicateUnit(ArtifactId),
    DuplicateModulePath(String),
    DependencyPathMismatch {
        module_path: String,
        artifact: ArtifactId,
    },
    MissingRoot(ArtifactId),
    UnreachableUnit(ArtifactId),
    DependencyCycle(ArtifactId),
}

impl fmt::Display for ArtifactGraphError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactGraphError::DuplicateUnit(id) => {
                write!(out, "artifact unit {id} occurs twice")
            }
            ArtifactGraphError::DuplicateModulePath(path) => {
                write!(out, "artifact module path `{path}` occurs twice")
            }
            ArtifactGraphError::DependencyPathMismatch {
                module_path,
                artifact,
            } => write!(
                out,
                "dependency `{module_path}` names artifact {artifact} for another module"
            ),
            ArtifactGraphError::MissingRoot(id) => {
                write!(out, "root artifact unit {id} is missing")
            }
            ArtifactGraphError::UnreachableUnit(id) => {
                write!(out, "artifact unit {id} is unreachable from the root")
            }
            ArtifactGraphError::DependencyCycle(id) => {
                write!(out, "artifact unit {id} belongs to a dependency cycle")
            }
        }
    }
}

impl std::error::Error for ArtifactGraphError {}

fn reject_cycles(
    units: &[Arc<LinkUnit>],
    successors: &[Vec<u32>],
) -> Result<(), ArtifactGraphError> {
    let (components, _) = lm_scc::components(units.len(), successors);
    for component in components {
        let first = component[0];
        if component.len() != 1 || successors[first as usize].contains(&first) {
            return Err(ArtifactGraphError::DependencyCycle(
                units[first as usize].id(),
            ));
        }
    }
    Ok(())
}

/// An artifact identity or dependency failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    InvalidModulePath(String),
    DuplicateModulePath(String),
    InterfacePathMismatch { unit: String, interface: String },
    TooManyDependencies,
    InvalidModuleSurface(String),
    Identity(IdentityError),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactError::InvalidModulePath(path) => {
                write!(out, "the artifact module path `{path}` is invalid")
            }
            ArtifactError::DuplicateModulePath(path) => {
                write!(out, "the artifact module path `{path}` is bound twice")
            }
            ArtifactError::InterfacePathMismatch { unit, interface } => {
                write!(
                    out,
                    "module `{unit}` carries the interface for `{interface}`"
                )
            }
            ArtifactError::TooManyDependencies => {
                out.write_str("the artifact has too many direct dependencies")
            }
            ArtifactError::InvalidModuleSurface(error) => {
                write!(out, "the module surface is invalid: {error}")
            }
            ArtifactError::Identity(error) => error.fmt(out),
        }
    }
}

impl std::error::Error for ArtifactError {}

impl From<IdentityError> for ArtifactError {
    fn from(error: IdentityError) -> ArtifactError {
        ArtifactError::Identity(error)
    }
}

fn write_digest(out: &mut fmt::Formatter<'_>, digest: &[u8; 32]) -> fmt::Result {
    for byte in digest {
        write!(out, "{byte:02x}")?;
    }
    Ok(())
}

fn compute_artifact_id(
    module_path: &str,
    module: &Module,
    identity: &ModuleIdentity,
    interface: &Interface,
    dependencies: &[ArtifactDependency],
) -> Result<ArtifactId, ArtifactError> {
    let count =
        u32::try_from(dependencies.len()).map_err(|_| ArtifactError::TooManyDependencies)?;
    let mut bytes = Vec::with_capacity(
        ARTIFACT_ID_TAG.len() + module_path.len() + 136 + dependencies.len() * 40,
    );
    bytes.extend_from_slice(ARTIFACT_ID_TAG);
    write_identity_string(&mut bytes, module_path)?;
    bytes.extend_from_slice(&identity.semantic_hash);
    bytes.extend_from_slice(&interface_identity(interface));
    bytes.extend_from_slice(&hash::hash256(&crate::semantic_section(module)));
    bytes.extend_from_slice(&hash::hash256(&crate::linkage_section(module)));
    bytes.extend_from_slice(&count.to_le_bytes());
    for dependency in dependencies {
        write_identity_string(&mut bytes, &dependency.module_path)?;
        bytes.extend_from_slice(dependency.artifact.as_bytes());
    }
    Ok(ArtifactId(hash::hash256(&bytes)))
}

fn write_identity_string(out: &mut Vec<u8>, text: &str) -> Result<(), ArtifactError> {
    let length = u32::try_from(text.len())
        .map_err(|_| ArtifactError::InvalidModulePath(text.to_string()))?;
    out.extend_from_slice(&length.to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

fn canonicalize_dependencies(dependencies: &mut [ArtifactDependency]) -> Result<(), ArtifactError> {
    for dependency in dependencies.iter() {
        validate_module_path(&dependency.module_path)?;
    }
    dependencies.sort_by(|left, right| {
        left.module_path
            .cmp(&right.module_path)
            .then(left.artifact.cmp(&right.artifact))
    });
    for pair in dependencies.windows(2) {
        if pair[0].module_path == pair[1].module_path {
            return Err(ArtifactError::DuplicateModulePath(
                pair[0].module_path.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_module_path(module_path: &str) -> Result<(), ArtifactError> {
    let valid = !module_path.is_empty()
        && module_path.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|first| {
                (first.is_ascii_alphabetic() || first == '_')
                    && chars.all(|item| item.is_ascii_alphanumeric() || item == '_' || item == '-')
            })
        });
    if valid {
        Ok(())
    } else {
        Err(ArtifactError::InvalidModulePath(module_path.to_string()))
    }
}

fn validate_unit_path(module_path: &str) -> Result<(), ArtifactError> {
    if module_path.is_empty() {
        Ok(())
    } else {
        validate_module_path(module_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BcType, Func, Instr, NO_ROLE};

    fn module(value: i64, debug: &[u8]) -> Module {
        Module {
            strings: Vec::new(),
            bytes: Vec::new(),
            types: vec![BcType::Int],
            selectors: Vec::new(),
            apps: Vec::new(),
            interfaces: Vec::new(),
            conformances: Vec::new(),
            class_bounds: Vec::new(),
            func_bounds: vec![Vec::new()],
            imports: Vec::new(),
            slots: Vec::new(),
            core_roles: [NO_ROLE; crate::CORE_ROLE_COUNT],
            classes: Vec::new(),
            funcs: vec![Func {
                name: "entry".to_string(),
                param_names: Vec::new(),
                type_params: 0,
                effect_params: 0,
                params: Vec::new(),
                param_muts: Vec::new(),
                ret: 0,
                row: Vec::new(),
                captures: Vec::new(),
                local_types: Vec::new(),
                blocks: vec![vec![Instr::ConstInt(value), Instr::Return]],
            }],
            entry: 0,
            exports: Vec::new(),
            bindings: Vec::new(),
            debug: debug.to_vec(),
        }
    }

    fn id(byte: u8) -> ArtifactId {
        ArtifactId::from_bytes([byte; 32])
    }

    fn interface(module_path: &str, module: &Module) -> Interface {
        let identity =
            crate::identity::module_identity(module).expect("the module has an identity");
        Interface {
            abi_version: lm_abi::ABI_VERSION,
            compiler_abi_version: crate::identity::COMPILER_ABI_VERSION,
            bundle_digest: lm_abi::standard_bundle().digest(),
            module_path: module_path.to_string(),
            semantic_hash: identity.semantic_hash,
            exports: Vec::new(),
            slots: Vec::new(),
        }
    }

    fn unit_at(
        module_path: &str,
        value: i64,
        debug: &[u8],
        dependencies: Vec<ArtifactDependency>,
    ) -> LinkUnit {
        let module = module(value, debug);
        let interface = interface(module_path, &module);
        LinkUnit::new(module_path, module, interface, dependencies)
            .expect("the artifact unit is valid")
    }

    fn unit(value: i64, debug: &[u8]) -> LinkUnit {
        unit_at("test.main", value, debug, Vec::new())
    }

    #[test]
    fn dependency_order_does_not_move_identity() {
        let first = unit_at(
            "test.main",
            7,
            &[],
            vec![
                ArtifactDependency::new("z", id(2)).unwrap(),
                ArtifactDependency::new("a", id(1)).unwrap(),
            ],
        );
        let second = unit_at(
            "test.main",
            7,
            &[],
            vec![
                ArtifactDependency::new("a", id(1)).unwrap(),
                ArtifactDependency::new("z", id(2)).unwrap(),
            ],
        );
        assert_eq!(first.id(), second.id());
        assert_eq!(first.dependencies()[0].module_path(), "a");
    }

    #[test]
    fn dependency_identity_moves_artifact_identity() {
        let first = unit_at(
            "test.main",
            7,
            &[],
            vec![ArtifactDependency::new("core", id(1)).unwrap()],
        );
        let second = unit_at(
            "test.main",
            7,
            &[],
            vec![ArtifactDependency::new("core", id(2)).unwrap()],
        );
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn dependency_module_path_moves_artifact_identity() {
        let first = unit_at(
            "test.main",
            7,
            &[],
            vec![ArtifactDependency::new("left", id(1)).unwrap()],
        );
        let second = unit_at(
            "test.main",
            7,
            &[],
            vec![ArtifactDependency::new("right", id(1)).unwrap()],
        );
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn own_module_path_moves_artifact_identity() {
        let first = unit_at("left", 7, &[], Vec::new());
        let second = unit_at("right", 7, &[], Vec::new());
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn debug_data_does_not_move_artifact_identity() {
        let first = unit(7, b"first");
        let second = unit(7, b"second");
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn module_semantics_move_artifact_identity() {
        let first = unit(7, &[]);
        let second = unit(8, &[]);
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn exported_target_moves_artifact_identity() {
        let mut first = module(7, &[]);
        let mut other = first.funcs[0].clone();
        other.name = "other".to_string();
        other.blocks[0][0] = Instr::ConstInt(8);
        first.funcs.push(other);
        first.func_bounds.push(Vec::new());
        first.exports.push(crate::Export {
            kind: crate::ExportKind::Function,
            name: "value".to_string(),
            def: 0,
            ctor: crate::NO_CTOR,
        });
        let mut second = first.clone();
        second.exports[0].def = 1;

        let first = LinkUnit::from_module("test.main", first, Vec::new()).unwrap();
        let second = LinkUnit::from_module("test.main", second, Vec::new()).unwrap();
        assert_eq!(
            first.identity().semantic_hash,
            second.identity().semantic_hash
        );
        assert_eq!(
            interface_identity(first.interface()),
            interface_identity(second.interface())
        );
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn duplicate_module_path_rejects() {
        let module = module(7, &[]);
        let interface = interface("test.main", &module);
        let error = LinkUnit::new(
            "test.main",
            module,
            interface,
            vec![
                ArtifactDependency::new("core", id(1)).unwrap(),
                ArtifactDependency::new("core", id(2)).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(
            error,
            ArtifactError::DuplicateModulePath("core".to_string())
        );
    }

    #[test]
    fn filesystem_module_path_rejects() {
        let error = ArtifactDependency::new("../core", id(1)).unwrap_err();
        assert_eq!(
            error,
            ArtifactError::InvalidModulePath("../core".to_string())
        );
    }

    #[test]
    fn interface_for_another_module_rejects() {
        let module = module(7, &[]);
        let interface = interface("other.main", &module);
        assert!(matches!(
            LinkUnit::new("test.main", module, interface, Vec::new()),
            Err(ArtifactError::InterfacePathMismatch { .. })
        ));
    }

    #[test]
    fn thin_and_fat_artifacts_have_one_root_identity() {
        let core = unit_at(CORE_MODULE_PATH, 1, &[], Vec::new());
        let root = unit_at(
            "app.main",
            42,
            &[],
            vec![ArtifactDependency::new(CORE_MODULE_PATH, core.id()).unwrap()],
        );
        let thin_bytes = encode(&Artifact::new(root.clone(), Vec::new()).unwrap()).unwrap();
        let fat_bytes = encode(&Artifact::new(root, vec![core.clone()]).unwrap()).unwrap();
        let thin = decode(&thin_bytes).unwrap();
        let fat = decode(&fat_bytes).unwrap();
        assert_eq!(thin.id(), fat.id());
        assert_eq!(thin.units().len(), 1);
        assert_eq!(fat.units().len(), 2);
        assert_ne!(
            crate::identity::container_hash(&thin_bytes),
            crate::identity::container_hash(&fat_bytes)
        );
    }

    #[test]
    fn artifact_encoding_is_canonical() {
        let left = unit_at("lib.left", 1, &[], Vec::new());
        let right = unit_at("lib.right", 2, &[], Vec::new());
        let root = unit_at(
            "app.main",
            42,
            &[],
            vec![
                ArtifactDependency::new("lib.right", right.id()).unwrap(),
                ArtifactDependency::new("lib.left", left.id()).unwrap(),
            ],
        );
        let first = Artifact::new(root.clone(), vec![left.clone(), right.clone()]).unwrap();
        let second = Artifact::new(root, vec![right, left]).unwrap();
        assert_eq!(encode(&first).unwrap(), encode(&second).unwrap());
    }

    #[test]
    fn artifact_round_trip_preserves_every_unit() {
        let core = unit_at(CORE_MODULE_PATH, 1, &[], Vec::new());
        let root = unit_at(
            "app.main",
            42,
            &[],
            vec![ArtifactDependency::new(CORE_MODULE_PATH, core.id()).unwrap()],
        );
        let artifact = Artifact::new(root, vec![core]).unwrap();
        let bytes = encode(&artifact).unwrap();
        assert_eq!(decode(&bytes).unwrap(), artifact);
    }

    #[test]
    fn artifact_bytes_store_the_module_surface_once() {
        let mut module = module(42, &[]);
        module.funcs[0].params = vec![0];
        module.funcs[0].param_muts = vec![false];
        module.funcs[0].param_names = vec!["value".to_string()];
        module.funcs[0].local_types = vec![0];
        module.exports.push(crate::Export {
            kind: crate::ExportKind::Function,
            name: "entry".to_string(),
            def: 0,
            ctor: crate::NO_CTOR,
        });
        let unit = LinkUnit::from_module("test.main", module, Vec::new()).unwrap();
        let module_bytes = crate::encode(unit.module());
        let artifact = Artifact::new(unit.clone(), Vec::new()).unwrap();
        let bytes = encode(&artifact).unwrap();
        let expected =
            codec::HEADER_LEN + 32 + 4 + unit.module_path().len() + 4 + 4 + module_bytes.len();
        assert_eq!(bytes.len(), expected);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(decoded.root().interface(), unit.interface());
    }

    #[test]
    fn decoder_recomputes_stored_unit_identity() {
        let root = unit(42, &[]);
        let artifact = Artifact::new(root, Vec::new()).unwrap();
        let mut bytes = encode(&artifact).unwrap();
        bytes[codec::HEADER_LEN] ^= 1;
        assert!(matches!(
            decode(&bytes),
            Err(ArtifactDecodeError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn decoder_rejects_a_missing_root() {
        let root = unit(42, &[]);
        let artifact = Artifact::new(root, Vec::new()).unwrap();
        let mut bytes = encode(&artifact).unwrap();
        let root_offset = 4 + 2 + 32;
        bytes[root_offset] ^= 1;
        assert!(matches!(
            decode(&bytes),
            Err(ArtifactDecodeError::Graph(ArtifactGraphError::MissingRoot(
                _
            )))
        ));
    }

    #[test]
    fn decoder_checks_unit_count_before_allocation() {
        let root = unit(42, &[]);
        let artifact = Artifact::new(root, Vec::new()).unwrap();
        let mut bytes = encode(&artifact).unwrap();
        let count_offset = 4 + 2 + 32 + 32;
        bytes[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(ArtifactDecodeError::Limit("unit")));
    }

    #[test]
    fn decoder_checks_total_bytes_before_header_work() {
        let root = unit(42, &[]);
        let bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        let bundle = lm_abi::standard_bundle();
        let limits = ArtifactLimits {
            max_bytes: bytes.len() - 1,
            ..ArtifactLimits::default()
        };
        assert_eq!(
            decode_with_bundle(&bytes, &bundle, limits),
            Err(ArtifactDecodeError::Limit("total byte"))
        );
    }

    #[test]
    fn decoder_checks_module_bytes_before_module_decode() {
        let root = unit(42, &[]);
        let bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        let bundle = lm_abi::standard_bundle();
        let limits = ArtifactLimits {
            max_module_bytes: 0,
            ..ArtifactLimits::default()
        };
        assert_eq!(
            decode_with_bundle(&bytes, &bundle, limits),
            Err(ArtifactDecodeError::Limit("module byte"))
        );
    }

    #[test]
    fn decoder_rejects_another_abi_bundle_digest() {
        let root = unit(42, &[]);
        let mut bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        bytes[4 + 2] ^= 1;
        assert!(matches!(
            decode(&bytes),
            Err(ArtifactDecodeError::BadBundle { .. })
        ));
    }

    #[test]
    fn every_artifact_truncation_rejects() {
        let root = unit(42, &[]);
        let bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err(), "prefix {end} decoded");
        }
    }

    #[test]
    fn duplicate_unit_rejects() {
        let root = unit(42, &[]);
        let root_id = root.id();
        assert_eq!(
            Artifact::from_units(root_id, vec![root.clone(), root]).unwrap_err(),
            ArtifactGraphError::DuplicateUnit(root_id)
        );
    }

    #[test]
    fn unreachable_unit_rejects() {
        let root = unit_at("app.main", 42, &[], Vec::new());
        let extra = unit_at("app.extra", 7, &[], Vec::new());
        assert_eq!(
            Artifact::new(root, vec![extra.clone()]).unwrap_err(),
            ArtifactGraphError::UnreachableUnit(extra.id())
        );
    }

    #[test]
    fn dependency_cycle_rejects() {
        let left_id = id(1);
        let right_id = id(2);
        let left_module = module(1, &[]);
        let right_module = module(2, &[]);
        let left = LinkUnit {
            id: left_id,
            bundle_digest: lm_abi::standard_bundle().digest(),
            path: "cycle.left".to_string(),
            interface: interface("cycle.left", &left_module),
            identity: crate::identity::module_identity(&left_module)
                .expect("the left module has an identity"),
            module: left_module,
            dependencies: vec![ArtifactDependency::new("cycle.right", right_id).unwrap()],
        };
        let right = LinkUnit {
            id: right_id,
            bundle_digest: lm_abi::standard_bundle().digest(),
            path: "cycle.right".to_string(),
            interface: interface("cycle.right", &right_module),
            identity: crate::identity::module_identity(&right_module)
                .expect("the right module has an identity"),
            module: right_module,
            dependencies: vec![ArtifactDependency::new("cycle.left", left_id).unwrap()],
        };
        assert!(matches!(
            Artifact::from_units(left_id, vec![left, right]),
            Err(ArtifactGraphError::DependencyCycle(_))
        ));
    }

    #[test]
    fn deep_dependency_graph_does_not_use_the_host_stack() {
        let mut units = Vec::new();
        let mut root = unit_at("chain.n0", 0, &[], Vec::new());
        for value in 1..2048 {
            units.push(root.clone());
            let dependency_path = root.module_path().to_string();
            root = unit_at(
                &format!("chain.n{value}"),
                value,
                &[],
                vec![ArtifactDependency::new(dependency_path, root.id()).unwrap()],
            );
        }
        let artifact = Artifact::new(root, units).unwrap();
        assert_eq!(artifact.units().len(), 2048);
    }

    #[test]
    fn debug_data_moves_container_hash_but_not_artifact_identity() {
        let first = Artifact::new(unit(42, b"first"), Vec::new()).unwrap();
        let second = Artifact::new(unit(42, b"second"), Vec::new()).unwrap();
        let first_bytes = encode(&first).unwrap();
        let second_bytes = encode(&second).unwrap();
        assert_eq!(first.id(), second.id());
        assert_ne!(
            crate::identity::container_hash(&first_bytes),
            crate::identity::container_hash(&second_bytes)
        );
    }

    #[test]
    fn payload_mutations_never_panic() {
        let bytes = encode(&Artifact::new(unit(42, b"debug"), Vec::new()).unwrap()).unwrap();
        for index in codec::HEADER_LEN..bytes.len() {
            let mut changed = bytes.clone();
            changed[index] ^= 1;
            let _ = decode(&changed);
        }
    }

    #[test]
    fn decoder_rejects_noncanonical_unit_order() {
        let first = unit_at("app.first", 1, &[], Vec::new());
        let second = unit_at("app.second", 2, &[], Vec::new());
        let mut units = vec![first, second];
        units.sort_by_key(LinkUnit::id);
        units.reverse();
        let artifact = Artifact {
            root: units[0].id(),
            units: units.into_iter().map(Arc::new).collect(),
        };
        let bytes = encode(&artifact).unwrap();
        assert_eq!(decode(&bytes), Err(ArtifactDecodeError::NonCanonicalUnits));
    }

    #[test]
    fn decoder_rejects_noncanonical_dependency_order() {
        let mut root = unit_at(
            "app.main",
            42,
            &[],
            vec![
                ArtifactDependency::new("app.first", id(1)).unwrap(),
                ArtifactDependency::new("app.second", id(2)).unwrap(),
            ],
        );
        root.dependencies.reverse();
        let artifact = Artifact {
            root: root.id(),
            units: vec![Arc::new(root)].into(),
        };
        let bytes = encode(&artifact).unwrap();
        assert_eq!(
            decode(&bytes),
            Err(ArtifactDecodeError::NonCanonicalDependencies)
        );
    }

    #[test]
    fn embedded_dependency_module_path_must_match() {
        let dependency = unit_at("lib.actual", 1, &[], Vec::new());
        let root = unit_at(
            "app.main",
            42,
            &[],
            vec![ArtifactDependency::new("lib.claimed", dependency.id()).unwrap()],
        );
        assert!(matches!(
            Artifact::new(root, vec![dependency]),
            Err(ArtifactGraphError::DependencyPathMismatch { .. })
        ));
    }
}
