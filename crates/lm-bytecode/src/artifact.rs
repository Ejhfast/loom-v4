//! Semantic artifact identities and exact dependency bindings.
//!
//! A `Module` is one bytecode payload. An `ArtifactRecord` binds that
//! payload to exact artifact dependencies. The package codec lives in
//! this module because it must recompute every stored identity.

use crate::identity::{module_identity_with_bundle, IdentityError, COMPILER_ABI_VERSION};
use crate::{hash, Module, VERSION};
use std::collections::BTreeMap;
use std::fmt;

mod codec;

pub use codec::{
    decode, decode_with_bundle, encode, encode_with_bundle, ArtifactDecodeError,
    ArtifactEncodeError, ArtifactLimits, FORMAT_VERSION,
};

const ARTIFACT_ID_TAG: &[u8] = b"lm-artifact-id-v1\0";

/// The logical dependency namespace of the standard core.
pub const CORE_NAMESPACE: &str = "core";

/// The semantic identity of one artifact record.
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

/// The exact identity of one encoded artifact blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash([u8; 32]);

impl BlobHash {
    /// Construct a hash from its canonical bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> BlobHash {
        BlobHash(bytes)
    }

    /// Return the canonical hash bytes.
    pub const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Borrow the canonical hash bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_digest(out, &self.0)
    }
}

/// One exact dependency in the logical artifact namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDependency {
    namespace: String,
    artifact: ArtifactId,
}

impl ArtifactDependency {
    /// Create one dependency binding.
    pub fn new(
        namespace: impl Into<String>,
        artifact: ArtifactId,
    ) -> Result<ArtifactDependency, ArtifactError> {
        let namespace = namespace.into();
        validate_namespace(&namespace)?;
        Ok(ArtifactDependency {
            namespace,
            artifact,
        })
    }

    /// Return the logical namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Return the exact dependency identity.
    pub fn artifact(&self) -> ArtifactId {
        self.artifact
    }
}

/// One module payload with its exact direct dependencies.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRecord {
    id: ArtifactId,
    bundle_digest: [u8; 32],
    module: Module,
    dependencies: Vec<ArtifactDependency>,
}

impl ArtifactRecord {
    /// Create one record under the standard ABI bundle.
    pub fn new(
        module: Module,
        dependencies: Vec<ArtifactDependency>,
    ) -> Result<ArtifactRecord, ArtifactError> {
        let bundle = lm_abi::standard_bundle();
        ArtifactRecord::new_with_bundle(module, dependencies, &bundle)
    }

    /// Create one record under an explicit ABI bundle.
    pub fn new_with_bundle(
        module: Module,
        mut dependencies: Vec<ArtifactDependency>,
        bundle: &lm_abi::AbiBundle,
    ) -> Result<ArtifactRecord, ArtifactError> {
        canonicalize_dependencies(&mut dependencies)?;
        let id = compute_artifact_id(&module, &dependencies, bundle)?;
        Ok(ArtifactRecord {
            id,
            bundle_digest: bundle.digest(),
            module,
            dependencies,
        })
    }

    /// Return the semantic artifact identity.
    pub fn id(&self) -> ArtifactId {
        self.id
    }

    /// Return the bytecode payload.
    pub fn module(&self) -> &Module {
        &self.module
    }

    pub(crate) fn bundle_digest(&self) -> [u8; 32] {
        self.bundle_digest
    }

    /// Return the canonical dependency bindings.
    pub fn dependencies(&self) -> &[ArtifactDependency] {
        &self.dependencies
    }

    /// Consume the record and return its payload and dependencies.
    pub fn into_parts(self) -> (Module, Vec<ArtifactDependency>) {
        (self.module, self.dependencies)
    }
}

/// One root artifact and its embedded dependency records.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    root: ArtifactId,
    records: Vec<ArtifactRecord>,
}

impl Artifact {
    /// Create one artifact from a root and embedded dependencies.
    pub fn new(
        root: ArtifactRecord,
        embedded: Vec<ArtifactRecord>,
    ) -> Result<Artifact, ArtifactGraphError> {
        let root_id = root.id();
        let mut records = Vec::with_capacity(embedded.len().saturating_add(1));
        records.push(root);
        records.extend(embedded);
        Artifact::from_records(root_id, records)
    }

    pub(crate) fn from_records(
        root: ArtifactId,
        mut records: Vec<ArtifactRecord>,
    ) -> Result<Artifact, ArtifactGraphError> {
        records.sort_by_key(ArtifactRecord::id);
        for pair in records.windows(2) {
            if pair[0].id() == pair[1].id() {
                return Err(ArtifactGraphError::DuplicateRecord(pair[0].id()));
            }
        }
        let artifact = Artifact { root, records };
        artifact.validate_graph()?;
        Ok(artifact)
    }

    /// Return the root identity.
    pub fn id(&self) -> ArtifactId {
        self.root
    }

    /// Return the root record.
    pub fn root(&self) -> &ArtifactRecord {
        self.record(self.root)
            .expect("artifact graph validation keeps the root record")
    }

    /// Return all embedded records, including the root.
    pub fn records(&self) -> &[ArtifactRecord] {
        &self.records
    }

    /// Find one embedded record by semantic identity.
    pub fn record(&self, id: ArtifactId) -> Option<&ArtifactRecord> {
        self.records
            .binary_search_by_key(&id, ArtifactRecord::id)
            .ok()
            .map(|index| &self.records[index])
    }

    /// Resolve the package through an optional runtime core.
    pub fn resolve<'a>(
        &'a self,
        runtime_core: Option<&'a ArtifactRecord>,
    ) -> Result<ResolvedArtifact<'a>, ArtifactResolveError> {
        let package: BTreeMap<ArtifactId, &ArtifactRecord> = self
            .records
            .iter()
            .map(|record| (record.id(), record))
            .collect();
        let mut selected: BTreeMap<ArtifactId, &ArtifactRecord> = BTreeMap::new();
        let mut work = vec![self.root];
        while let Some(id) = work.pop() {
            if selected.contains_key(&id) {
                continue;
            }
            let record = package
                .get(&id)
                .copied()
                .ok_or(ArtifactResolveError::MissingRoot(id))?;
            selected.insert(id, record);
            for dependency in record.dependencies.iter().rev() {
                if package.contains_key(&dependency.artifact) {
                    work.push(dependency.artifact);
                    continue;
                }
                if dependency.namespace == CORE_NAMESPACE {
                    let Some(core) = runtime_core else {
                        return Err(ArtifactResolveError::MissingCore {
                            expected: dependency.artifact,
                        });
                    };
                    if core.id() != dependency.artifact {
                        return Err(ArtifactResolveError::CoreMismatch {
                            expected: dependency.artifact,
                            found: core.id(),
                        });
                    }
                    if let std::collections::btree_map::Entry::Vacant(entry) =
                        selected.entry(core.id())
                    {
                        entry.insert(core);
                        for child in core.dependencies.iter().rev() {
                            if package.contains_key(&child.artifact) {
                                work.push(child.artifact);
                            } else {
                                return Err(ArtifactResolveError::MissingDependency {
                                    parent: core.id(),
                                    namespace: child.namespace.clone(),
                                    expected: child.artifact,
                                });
                            }
                        }
                    }
                    continue;
                }
                return Err(ArtifactResolveError::MissingDependency {
                    parent: record.id(),
                    namespace: dependency.namespace.clone(),
                    expected: dependency.artifact,
                });
            }
        }
        resolved_from_records(self.root, selected)
    }

    fn validate_graph(&self) -> Result<(), ArtifactGraphError> {
        let index: BTreeMap<ArtifactId, u32> = self
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| (record.id(), index as u32))
            .collect();
        let Some(root) = index.get(&self.root).copied() else {
            return Err(ArtifactGraphError::MissingRoot(self.root));
        };
        let mut successors = vec![Vec::new(); self.records.len()];
        for (record_index, record) in self.records.iter().enumerate() {
            for dependency in &record.dependencies {
                if let Some(target) = index.get(&dependency.artifact) {
                    successors[record_index].push(*target);
                }
            }
        }
        let mut reached = vec![false; self.records.len()];
        let mut work = vec![root];
        while let Some(node) = work.pop() {
            if reached[node as usize] {
                continue;
            }
            reached[node as usize] = true;
            work.extend(successors[node as usize].iter().copied());
        }
        if let Some((index, _)) = reached.iter().enumerate().find(|(_, item)| !**item) {
            return Err(ArtifactGraphError::UnreachableRecord(
                self.records[index].id(),
            ));
        }
        reject_cycles(&self.records, &successors)
    }
}

/// One fully resolved artifact graph in dependency-first order.
#[derive(Debug)]
pub struct ResolvedArtifact<'a> {
    root: ArtifactId,
    records: Vec<&'a ArtifactRecord>,
}

impl ResolvedArtifact<'_> {
    /// Return the root identity.
    pub fn id(&self) -> ArtifactId {
        self.root
    }

    /// Return the root record.
    pub fn root(&self) -> &ArtifactRecord {
        self.records
            .iter()
            .copied()
            .find(|record| record.id() == self.root)
            .expect("resolved artifact validation keeps the root record")
    }

    /// Return records in dependency-first order.
    pub fn records(&self) -> &[&ArtifactRecord] {
        &self.records
    }
}

/// An invalid artifact package graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactGraphError {
    DuplicateRecord(ArtifactId),
    MissingRoot(ArtifactId),
    UnreachableRecord(ArtifactId),
    DependencyCycle(ArtifactId),
}

impl fmt::Display for ArtifactGraphError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactGraphError::DuplicateRecord(id) => {
                write!(out, "artifact record {id} occurs twice")
            }
            ArtifactGraphError::MissingRoot(id) => {
                write!(out, "root artifact record {id} is missing")
            }
            ArtifactGraphError::UnreachableRecord(id) => {
                write!(out, "artifact record {id} is unreachable from the root")
            }
            ArtifactGraphError::DependencyCycle(id) => {
                write!(out, "artifact record {id} belongs to a dependency cycle")
            }
        }
    }
}

impl std::error::Error for ArtifactGraphError {}

/// An artifact dependency resolution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactResolveError {
    MissingRoot(ArtifactId),
    MissingCore {
        expected: ArtifactId,
    },
    CoreMismatch {
        expected: ArtifactId,
        found: ArtifactId,
    },
    MissingDependency {
        parent: ArtifactId,
        namespace: String,
        expected: ArtifactId,
    },
    DependencyCycle(ArtifactId),
}

impl fmt::Display for ArtifactResolveError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactResolveError::MissingRoot(id) => {
                write!(out, "root artifact record {id} is missing")
            }
            ArtifactResolveError::MissingCore { expected } => {
                write!(out, "the artifact needs unavailable core {expected}")
            }
            ArtifactResolveError::CoreMismatch { expected, found } => write!(
                out,
                "the artifact needs core {expected}, but the runtime provides {found}"
            ),
            ArtifactResolveError::MissingDependency {
                parent,
                namespace,
                expected,
            } => write!(
                out,
                "artifact {parent} needs unavailable dependency `{namespace}` {expected}"
            ),
            ArtifactResolveError::DependencyCycle(id) => {
                write!(out, "artifact record {id} belongs to a dependency cycle")
            }
        }
    }
}

impl std::error::Error for ArtifactResolveError {}

fn resolved_from_records<'a>(
    root: ArtifactId,
    records: BTreeMap<ArtifactId, &'a ArtifactRecord>,
) -> Result<ResolvedArtifact<'a>, ArtifactResolveError> {
    let entries: Vec<(ArtifactId, &ArtifactRecord)> = records.into_iter().collect();
    let index: BTreeMap<ArtifactId, u32> = entries
        .iter()
        .enumerate()
        .map(|(index, (id, _))| (*id, index as u32))
        .collect();
    let mut successors = vec![Vec::new(); entries.len()];
    for (record_index, (_, record)) in entries.iter().enumerate() {
        for dependency in record.dependencies() {
            let Some(target) = index.get(&dependency.artifact()) else {
                return Err(ArtifactResolveError::MissingDependency {
                    parent: record.id(),
                    namespace: dependency.namespace().to_string(),
                    expected: dependency.artifact(),
                });
            };
            successors[record_index].push(*target);
        }
    }
    let (components, _) = lm_scc::components(entries.len(), &successors);
    let mut ordered = Vec::with_capacity(entries.len());
    for component in components {
        let first = component[0];
        if component.len() != 1 || successors[first as usize].contains(&first) {
            return Err(ArtifactResolveError::DependencyCycle(
                entries[first as usize].0,
            ));
        }
        ordered.push(entries[first as usize].1);
    }
    Ok(ResolvedArtifact {
        root,
        records: ordered,
    })
}

fn reject_cycles(
    records: &[ArtifactRecord],
    successors: &[Vec<u32>],
) -> Result<(), ArtifactGraphError> {
    let (components, _) = lm_scc::components(records.len(), successors);
    for component in components {
        let first = component[0];
        if component.len() != 1 || successors[first as usize].contains(&first) {
            return Err(ArtifactGraphError::DependencyCycle(
                records[first as usize].id(),
            ));
        }
    }
    Ok(())
}

/// An artifact identity or dependency failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    InvalidNamespace(String),
    DuplicateNamespace(String),
    TooManyDependencies,
    Identity(IdentityError),
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactError::InvalidNamespace(namespace) => {
                write!(out, "the artifact namespace `{namespace}` is invalid")
            }
            ArtifactError::DuplicateNamespace(namespace) => {
                write!(out, "the artifact namespace `{namespace}` is bound twice")
            }
            ArtifactError::TooManyDependencies => {
                out.write_str("the artifact has too many direct dependencies")
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

/// Compute the exact hash of encoded artifact bytes.
pub fn blob_hash(bytes: &[u8]) -> BlobHash {
    BlobHash(hash::hash256(bytes))
}

fn write_digest(out: &mut fmt::Formatter<'_>, digest: &[u8; 32]) -> fmt::Result {
    for byte in digest {
        write!(out, "{byte:02x}")?;
    }
    Ok(())
}

fn compute_artifact_id(
    module: &Module,
    dependencies: &[ArtifactDependency],
    bundle: &lm_abi::AbiBundle,
) -> Result<ArtifactId, ArtifactError> {
    let count =
        u32::try_from(dependencies.len()).map_err(|_| ArtifactError::TooManyDependencies)?;
    let identity = module_identity_with_bundle(module, bundle)?;
    let mut bytes = Vec::with_capacity(ARTIFACT_ID_TAG.len() + 74 + dependencies.len() * 40);
    bytes.extend_from_slice(ARTIFACT_ID_TAG);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&bundle.digest());
    bytes.extend_from_slice(&identity.semantic_hash);
    bytes.extend_from_slice(&count.to_le_bytes());
    for dependency in dependencies {
        let namespace = dependency.namespace.as_bytes();
        let length = u32::try_from(namespace.len())
            .map_err(|_| ArtifactError::InvalidNamespace(dependency.namespace.clone()))?;
        bytes.extend_from_slice(&length.to_le_bytes());
        bytes.extend_from_slice(namespace);
        bytes.extend_from_slice(dependency.artifact.as_bytes());
    }
    Ok(ArtifactId(hash::hash256(&bytes)))
}

fn canonicalize_dependencies(dependencies: &mut [ArtifactDependency]) -> Result<(), ArtifactError> {
    for dependency in dependencies.iter() {
        validate_namespace(&dependency.namespace)?;
    }
    dependencies.sort_by(|left, right| {
        left.namespace
            .cmp(&right.namespace)
            .then(left.artifact.cmp(&right.artifact))
    });
    for pair in dependencies.windows(2) {
        if pair[0].namespace == pair[1].namespace {
            return Err(ArtifactError::DuplicateNamespace(pair[0].namespace.clone()));
        }
    }
    Ok(())
}

fn validate_namespace(namespace: &str) -> Result<(), ArtifactError> {
    let valid = !namespace.is_empty()
        && namespace.split('.').all(|part| {
            let mut chars = part.chars();
            chars.next().is_some_and(|first| {
                (first.is_ascii_alphabetic() || first == '_')
                    && chars.all(|item| item.is_ascii_alphanumeric() || item == '_' || item == '-')
            })
        });
    if valid {
        Ok(())
    } else {
        Err(ArtifactError::InvalidNamespace(namespace.to_string()))
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

    #[test]
    fn dependency_order_does_not_move_identity() {
        let first = ArtifactRecord::new(
            module(7, &[]),
            vec![
                ArtifactDependency::new("z", id(2)).unwrap(),
                ArtifactDependency::new("a", id(1)).unwrap(),
            ],
        )
        .unwrap();
        let second = ArtifactRecord::new(
            module(7, &[]),
            vec![
                ArtifactDependency::new("a", id(1)).unwrap(),
                ArtifactDependency::new("z", id(2)).unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.dependencies()[0].namespace(), "a");
    }

    #[test]
    fn dependency_identity_moves_artifact_identity() {
        let first = ArtifactRecord::new(
            module(7, &[]),
            vec![ArtifactDependency::new("core", id(1)).unwrap()],
        )
        .unwrap();
        let second = ArtifactRecord::new(
            module(7, &[]),
            vec![ArtifactDependency::new("core", id(2)).unwrap()],
        )
        .unwrap();
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn dependency_namespace_moves_artifact_identity() {
        let first = ArtifactRecord::new(
            module(7, &[]),
            vec![ArtifactDependency::new("left", id(1)).unwrap()],
        )
        .unwrap();
        let second = ArtifactRecord::new(
            module(7, &[]),
            vec![ArtifactDependency::new("right", id(1)).unwrap()],
        )
        .unwrap();
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn debug_data_does_not_move_artifact_identity() {
        let first = ArtifactRecord::new(module(7, b"first"), Vec::new()).unwrap();
        let second = ArtifactRecord::new(module(7, b"second"), Vec::new()).unwrap();
        assert_eq!(first.id(), second.id());
    }

    #[test]
    fn module_semantics_move_artifact_identity() {
        let first = ArtifactRecord::new(module(7, &[]), Vec::new()).unwrap();
        let second = ArtifactRecord::new(module(8, &[]), Vec::new()).unwrap();
        assert_ne!(first.id(), second.id());
    }

    #[test]
    fn duplicate_namespace_rejects() {
        let error = ArtifactRecord::new(
            module(7, &[]),
            vec![
                ArtifactDependency::new("core", id(1)).unwrap(),
                ArtifactDependency::new("core", id(2)).unwrap(),
            ],
        )
        .unwrap_err();
        assert_eq!(error, ArtifactError::DuplicateNamespace("core".to_string()));
    }

    #[test]
    fn filesystem_namespace_rejects() {
        let error = ArtifactDependency::new("../core", id(1)).unwrap_err();
        assert_eq!(
            error,
            ArtifactError::InvalidNamespace("../core".to_string())
        );
    }

    #[test]
    fn blob_hash_reads_exact_bytes() {
        assert_ne!(blob_hash(b"one"), blob_hash(b"two"));
        assert_eq!(blob_hash(b"one"), blob_hash(b"one"));
    }

    #[test]
    fn thin_and_fat_artifacts_resolve_to_one_graph() {
        let core = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![ArtifactDependency::new(CORE_NAMESPACE, core.id()).unwrap()],
        )
        .unwrap();
        let thin_bytes = encode(&Artifact::new(root.clone(), Vec::new()).unwrap()).unwrap();
        let fat_bytes = encode(&Artifact::new(root, vec![core.clone()]).unwrap()).unwrap();
        let thin = decode(&thin_bytes).unwrap();
        let fat = decode(&fat_bytes).unwrap();
        let thin_ids: Vec<ArtifactId> = thin
            .resolve(Some(&core))
            .unwrap()
            .records()
            .iter()
            .map(|record| record.id())
            .collect();
        let fat_ids: Vec<ArtifactId> = fat
            .resolve(None)
            .unwrap()
            .records()
            .iter()
            .map(|record| record.id())
            .collect();
        assert_eq!(thin.id(), fat.id());
        assert_eq!(thin_ids, fat_ids);
        assert_ne!(blob_hash(&thin_bytes), blob_hash(&fat_bytes));
    }

    #[test]
    fn thin_artifact_rejects_another_runtime_core() {
        let expected = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let found = ArtifactRecord::new(module(2, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![ArtifactDependency::new(CORE_NAMESPACE, expected.id()).unwrap()],
        )
        .unwrap();
        let artifact = Artifact::new(root, Vec::new()).unwrap();
        assert_eq!(
            artifact.resolve(Some(&found)).unwrap_err(),
            ArtifactResolveError::CoreMismatch {
                expected: expected.id(),
                found: found.id(),
            }
        );
    }

    #[test]
    fn runtime_core_cannot_fill_another_namespace() {
        let library = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![ArtifactDependency::new("math", library.id()).unwrap()],
        )
        .unwrap();
        let artifact = Artifact::new(root.clone(), Vec::new()).unwrap();
        assert_eq!(
            artifact.resolve(Some(&library)).unwrap_err(),
            ArtifactResolveError::MissingDependency {
                parent: root.id(),
                namespace: "math".to_string(),
                expected: library.id(),
            }
        );
    }

    #[test]
    fn embedded_core_needs_no_ambient_core() {
        let core = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![ArtifactDependency::new(CORE_NAMESPACE, core.id()).unwrap()],
        )
        .unwrap();
        let artifact = Artifact::new(root, vec![core]).unwrap();
        assert_eq!(artifact.resolve(None).unwrap().records().len(), 2);
    }

    #[test]
    fn package_encoding_is_canonical() {
        let left = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let right = ArtifactRecord::new(module(2, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![
                ArtifactDependency::new("right", right.id()).unwrap(),
                ArtifactDependency::new("left", left.id()).unwrap(),
            ],
        )
        .unwrap();
        let first = Artifact::new(root.clone(), vec![left.clone(), right.clone()]).unwrap();
        let second = Artifact::new(root, vec![right, left]).unwrap();
        assert_eq!(encode(&first).unwrap(), encode(&second).unwrap());
    }

    #[test]
    fn package_round_trip_preserves_every_record() {
        let core = ArtifactRecord::new(module(1, &[]), Vec::new()).unwrap();
        let root = ArtifactRecord::new(
            module(42, &[]),
            vec![ArtifactDependency::new(CORE_NAMESPACE, core.id()).unwrap()],
        )
        .unwrap();
        let artifact = Artifact::new(root, vec![core]).unwrap();
        let bytes = encode(&artifact).unwrap();
        assert_eq!(decode(&bytes).unwrap(), artifact);
    }

    #[test]
    fn decoder_recomputes_stored_record_identity() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
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
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
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
    fn decoder_checks_record_count_before_allocation() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
        let artifact = Artifact::new(root, Vec::new()).unwrap();
        let mut bytes = encode(&artifact).unwrap();
        let count_offset = 4 + 2 + 32 + 32;
        bytes[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&bytes), Err(ArtifactDecodeError::Limit("record")));
    }

    #[test]
    fn decoder_checks_total_bytes_before_header_work() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
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
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
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
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
        let mut bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        bytes[4 + 2] ^= 1;
        assert!(matches!(
            decode(&bytes),
            Err(ArtifactDecodeError::BadBundle { .. })
        ));
    }

    #[test]
    fn every_package_truncation_rejects() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
        let bytes = encode(&Artifact::new(root, Vec::new()).unwrap()).unwrap();
        for end in 0..bytes.len() {
            assert!(decode(&bytes[..end]).is_err(), "prefix {end} decoded");
        }
    }

    #[test]
    fn duplicate_record_rejects() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
        let root_id = root.id();
        assert_eq!(
            Artifact::from_records(root_id, vec![root.clone(), root]).unwrap_err(),
            ArtifactGraphError::DuplicateRecord(root_id)
        );
    }

    #[test]
    fn unreachable_record_rejects() {
        let root = ArtifactRecord::new(module(42, &[]), Vec::new()).unwrap();
        let extra = ArtifactRecord::new(module(7, &[]), Vec::new()).unwrap();
        assert_eq!(
            Artifact::new(root, vec![extra.clone()]).unwrap_err(),
            ArtifactGraphError::UnreachableRecord(extra.id())
        );
    }

    #[test]
    fn dependency_cycle_rejects() {
        let left_id = id(1);
        let right_id = id(2);
        let left = ArtifactRecord {
            id: left_id,
            bundle_digest: lm_abi::standard_bundle().digest(),
            module: module(1, &[]),
            dependencies: vec![ArtifactDependency::new("right", right_id).unwrap()],
        };
        let right = ArtifactRecord {
            id: right_id,
            bundle_digest: lm_abi::standard_bundle().digest(),
            module: module(2, &[]),
            dependencies: vec![ArtifactDependency::new("left", left_id).unwrap()],
        };
        assert!(matches!(
            Artifact::from_records(left_id, vec![left, right]),
            Err(ArtifactGraphError::DependencyCycle(_))
        ));
    }

    #[test]
    fn deep_dependency_graph_does_not_use_the_host_stack() {
        let mut records = Vec::new();
        let mut root = ArtifactRecord::new(module(0, &[]), Vec::new()).unwrap();
        for value in 1..2048 {
            records.push(root.clone());
            root = ArtifactRecord::new(
                module(value, &[]),
                vec![ArtifactDependency::new("next", root.id()).unwrap()],
            )
            .unwrap();
        }
        let artifact = Artifact::new(root, records).unwrap();
        assert_eq!(artifact.resolve(None).unwrap().records().len(), 2048);
    }

    #[test]
    fn debug_data_moves_blob_hash_but_not_artifact_identity() {
        let first = Artifact::new(
            ArtifactRecord::new(module(42, b"first"), Vec::new()).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let second = Artifact::new(
            ArtifactRecord::new(module(42, b"second"), Vec::new()).unwrap(),
            Vec::new(),
        )
        .unwrap();
        let first_bytes = encode(&first).unwrap();
        let second_bytes = encode(&second).unwrap();
        assert_eq!(first.id(), second.id());
        assert_ne!(blob_hash(&first_bytes), blob_hash(&second_bytes));
    }
}
