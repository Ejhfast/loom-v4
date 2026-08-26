//! Semantic artifact identities and exact dependency bindings.
//!
//! A `Module` is one bytecode payload. An `ArtifactRecord` binds that
//! payload to exact artifact dependencies. The package codec lives in
//! this module because it must recompute every stored identity.

use crate::identity::{module_identity_with_bundle, IdentityError, COMPILER_ABI_VERSION};
use crate::{hash, Module, VERSION};
use std::fmt;

const ARTIFACT_ID_TAG: &[u8] = b"lm-artifact-id-v1\0";

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

    /// Return the canonical dependency bindings.
    pub fn dependencies(&self) -> &[ArtifactDependency] {
        &self.dependencies
    }

    /// Consume the record and return its payload and dependencies.
    pub fn into_parts(self) -> (Module, Vec<ArtifactDependency>) {
        (self.module, self.dependencies)
    }
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

fn canonicalize_dependencies(
    dependencies: &mut Vec<ArtifactDependency>,
) -> Result<(), ArtifactError> {
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
}
