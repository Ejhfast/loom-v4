//! Artifact-backed code preparation for snapshots.

use super::{Image, ImageError, ImageReason, SnapshotFail};
use crate::NamespaceRuntime;
use lm_bytecode::artifact::{Artifact, ArtifactId, ArtifactLimits, LinkUnit};
use lm_link::{CodeArena, CodeRelocation};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

/// Verified code namespaces used by one admitted image.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotCode(Arc<SnapshotCodeInner>);

#[derive(Debug)]
struct SnapshotCodeInner {
    artifacts: Arc<[Arc<Artifact>]>,
    namespaces: Arc<[Arc<NamespaceRuntime>]>,
    namespace_ids: Option<Arc<[lm_link::NamespaceId]>>,
}

/// Weak canonical layouts for external snapshots of one world.
#[derive(Default)]
pub(crate) struct SnapshotCodeCache {
    entries: HashMap<SnapshotCodeKey, Vec<CachedSnapshotCode>>,
}

struct CachedSnapshotCode {
    artifacts: Arc<[Vec<u8>]>,
    code: Weak<SnapshotCodeInner>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SnapshotCodeKey {
    bundle: [u8; 32],
    core: Option<ArtifactId>,
    artifacts: Vec<ArtifactId>,
    namespaces: Vec<Vec<u32>>,
}

/// One portable table layout for a trusted snapshot container.
pub(crate) struct PortableLayout {
    pub(crate) code: SnapshotCode,
    pub(crate) combined: CodeRelocation,
    pub(crate) namespaces: Vec<CodeRelocation>,
}

impl SnapshotCode {
    pub(crate) fn new(
        artifacts: Vec<Arc<Artifact>>,
        namespaces: Vec<Arc<NamespaceRuntime>>,
    ) -> SnapshotCode {
        SnapshotCode(Arc::new(SnapshotCodeInner {
            artifacts: artifacts.into(),
            namespaces: namespaces.into(),
            namespace_ids: None,
        }))
    }

    pub(crate) fn trusted(
        artifacts: Arc<[Arc<Artifact>]>,
        namespaces: Vec<Arc<NamespaceRuntime>>,
        namespace_ids: Vec<lm_link::NamespaceId>,
    ) -> SnapshotCode {
        SnapshotCode(Arc::new(SnapshotCodeInner {
            artifacts,
            namespaces: namespaces.into(),
            namespace_ids: Some(namespace_ids.into()),
        }))
    }

    pub(crate) fn artifacts(&self) -> &[Arc<Artifact>] {
        &self.0.artifacts
    }

    pub(crate) fn artifact(&self, ordinal: u32) -> Option<&Artifact> {
        self.0.artifacts.get(ordinal as usize).map(Arc::as_ref)
    }

    pub(crate) fn artifact_store(&self, ordinal: u32) -> Option<Arc<Artifact>> {
        self.0.artifacts.get(ordinal as usize).cloned()
    }

    pub(crate) fn namespace(&self, ordinal: u32) -> Option<&Arc<NamespaceRuntime>> {
        self.0.namespaces.get(ordinal as usize)
    }

    pub(crate) fn namespace_id(&self, ordinal: usize) -> Option<lm_link::NamespaceId> {
        self.0.namespace_ids.as_deref()?.get(ordinal).copied()
    }

    pub(crate) fn namespace_id_store(&self) -> Option<Arc<[lm_link::NamespaceId]>> {
        self.0.namespace_ids.clone()
    }

    /// True when live arena indices differ from portable indices.
    pub(crate) fn needs_portable_layout(&self) -> bool {
        if self.0.namespace_ids.is_none() {
            return false;
        }
        let mut previous: Option<&[Arc<Artifact>]> = None;
        for runtime in self.0.namespaces.iter() {
            let namespace = runtime.code_namespace();
            if !namespace.has_canonical_layout() {
                return true;
            }
            let chain = namespace.artifacts();
            if let Some(prefix) = previous {
                let extends_prefix = prefix.len() <= chain.len()
                    && prefix
                        .iter()
                        .zip(chain)
                        .all(|(left, right)| left.id() == right.id());
                if !extends_prefix {
                    return true;
                }
            }
            previous = Some(chain);
        }
        false
    }

    pub(crate) fn namespaces(&self) -> &[Arc<NamespaceRuntime>] {
        &self.0.namespaces
    }

    pub(crate) fn tables(&self) -> Option<&Arc<NamespaceRuntime>> {
        self.0.namespaces.last()
    }

    pub(crate) fn contains_function(&self, function: u32) -> bool {
        self.0
            .namespaces
            .iter()
            .any(|namespace| namespace.code_namespace().contains_function(function))
    }

    pub(crate) fn contains_class(&self, class: u32) -> bool {
        self.0
            .namespaces
            .iter()
            .any(|namespace| namespace.code_namespace().contains_class(class))
    }

    pub(crate) fn bundle(&self) -> Option<&Arc<lm_abi::AbiBundle>> {
        self.tables().map(|namespace| namespace.bundle())
    }

    /// Build the deterministic snapshot-local table layout.
    ///
    /// The returned maps convert live arena indices into portable
    /// indices. External loading builds the same layout from the
    /// artifact and namespace tables.
    pub(crate) fn portable_layout(&self) -> Result<PortableLayout, SnapshotFail> {
        let bundle = self.bundle().cloned().ok_or_else(|| {
            SnapshotFail::Fault(
                lm_abi::FaultCode::MalformedState,
                "the snapshot has no code namespace".to_string(),
            )
        })?;
        let mut arena = CodeArena::with_bundle(bundle);
        let mut runtimes = Vec::new();
        runtimes
            .try_reserve_exact(self.0.namespaces.len())
            .map_err(|_| SnapshotFail::LimitExceeded)?;
        let mut maps = Vec::new();
        maps.try_reserve_exact(self.0.namespaces.len())
            .map_err(|_| SnapshotFail::LimitExceeded)?;
        let mut combined: Option<CodeRelocation> = None;
        for source in self.0.namespaces.iter() {
            let target_id = arena
                .replay_namespace(source.code_namespace())
                .map_err(portable_link_error)?;
            let target = arena
                .namespace(target_id)
                .cloned()
                .ok_or_else(|| portable_error("the portable code namespace is missing"))?;
            let map = source
                .code_namespace()
                .relocation_to(&target)
                .map_err(portable_link_error)?;
            match &mut combined {
                Some(combined) => combined.merge(&map).map_err(portable_link_error)?,
                None => combined = Some(map.clone()),
            }
            maps.push(map);
            runtimes.push(Arc::new(crate::prepare_namespace(target)));
        }
        let combined = combined
            .ok_or_else(|| portable_error("the snapshot has no portable code namespace"))?;
        Ok(PortableLayout {
            code: SnapshotCode::new(self.0.artifacts.to_vec(), runtimes),
            combined,
            namespaces: maps,
        })
    }
}

fn portable_link_error(error: lm_link::LinkError) -> SnapshotFail {
    portable_error(error.to_string())
}

fn portable_error(detail: impl Into<String>) -> SnapshotFail {
    SnapshotFail::Fault(lm_abi::FaultCode::MalformedState, detail.into())
}

impl SnapshotCodeCache {
    fn get(&mut self, key: &SnapshotCodeKey, artifacts: &[Vec<u8>]) -> Option<SnapshotCode> {
        let mut found = None;
        let mut empty = false;
        if let Some(entries) = self.entries.get_mut(key) {
            entries.retain(|entry| entry.code.strong_count() != 0);
            found = entries
                .iter()
                .find(|entry| entry.artifacts.as_ref() == artifacts)
                .and_then(|entry| entry.code.upgrade())
                .map(SnapshotCode);
            empty = entries.is_empty();
        }
        if empty {
            self.entries.remove(key);
        }
        found
    }

    fn insert(&mut self, key: SnapshotCodeKey, artifacts: &[Vec<u8>], code: &SnapshotCode) {
        self.entries
            .entry(key)
            .or_default()
            .push(CachedSnapshotCode {
                artifacts: artifacts.to_vec().into(),
                code: Arc::downgrade(&code.0),
            });
    }
}

impl SnapshotCodeKey {
    fn from_encoded(
        image: &Image,
        bundle: &lm_abi::AbiBundle,
        runtime_core: Option<&LinkUnit>,
    ) -> Option<SnapshotCodeKey> {
        let artifacts = image
            .artifacts
            .iter()
            .map(|bytes| lm_bytecode::artifact::encoded_id_with_bundle(bytes, bundle))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(SnapshotCodeKey {
            bundle: bundle.digest(),
            core: runtime_core.map(LinkUnit::id),
            artifacts,
            namespaces: image
                .namespaces
                .iter()
                .map(|namespace| namespace.artifacts.clone())
                .collect(),
        })
    }
}

/// Decode, verify, and publish the code of one external image.
pub(crate) fn prepare_external(
    image: &Image,
    runtime_core: Option<&LinkUnit>,
    bundle: Arc<lm_abi::AbiBundle>,
    mut cache: Option<&mut SnapshotCodeCache>,
) -> Result<SnapshotCode, ImageError> {
    let key = cache
        .as_ref()
        .and_then(|_| SnapshotCodeKey::from_encoded(image, &bundle, runtime_core));
    if let (Some(cache), Some(key)) = (cache.as_deref_mut(), key.as_ref()) {
        if let Some(code) = cache.get(key, &image.artifacts) {
            return Ok(code);
        }
    }
    let artifacts = match image.artifact_values() {
        Some(values) => values.to_vec(),
        None => {
            let mut artifacts = Vec::new();
            artifacts
                .try_reserve_exact(image.artifact_count())
                .map_err(|_| {
                    ImageError::admission(ImageReason::Budget, "the artifact table is too large")
                })?;
            for (index, bytes) in image.artifacts.iter().enumerate() {
                let artifact = lm_bytecode::artifact::decode_with_bundle(
                    bytes,
                    &bundle,
                    ArtifactLimits::default(),
                )
                .map_err(|error| {
                    ImageError::admission(
                        ImageReason::Code,
                        format!("artifact {index} did not decode: {error}"),
                    )
                })?;
                artifacts.push(Arc::new(artifact));
            }
            artifacts
        }
    };

    let runtime_core = runtime_core.cloned().map(Arc::new);
    let mut arena = CodeArena::with_bundle(bundle);
    let mut namespaces = Vec::new();
    namespaces
        .try_reserve_exact(image.namespaces.len())
        .map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the namespace table is too large")
        })?;
    for (index, manifest) in image.namespaces.iter().enumerate() {
        let Some(first) = manifest.artifacts.first().copied() else {
            return Err(ImageError::admission(
                ImageReason::Code,
                format!("namespace {index} has no root artifact"),
            ));
        };
        let root = artifacts.get(first as usize).cloned().ok_or_else(|| {
            ImageError::admission(
                ImageReason::Code,
                format!("namespace {index} names missing artifact {first}"),
            )
        })?;
        let mut namespace = arena
            .publish(root.as_ref().clone(), runtime_core.clone())
            .map_err(|error| ImageError::admission(ImageReason::Code, error.to_string()))?;
        for ordinal in manifest.artifacts.iter().copied().skip(1) {
            let artifact = artifacts.get(ordinal as usize).cloned().ok_or_else(|| {
                ImageError::admission(
                    ImageReason::Code,
                    format!("namespace {index} names missing artifact {ordinal}"),
                )
            })?;
            namespace = arena
                .extend(namespace, artifact.as_ref().clone())
                .map_err(|error| ImageError::admission(ImageReason::Code, error.to_string()))?;
        }
        let linked = arena.namespace(namespace).cloned().ok_or_else(|| {
            ImageError::admission(ImageReason::Code, "the published namespace is missing")
        })?;
        namespaces.push(Arc::new(crate::prepare_namespace(linked)));
    }
    let code = SnapshotCode::new(artifacts, namespaces);
    if let (Some(cache), Some(key)) = (cache, key) {
        cache.insert(key, &image.artifacts, &code);
    }
    Ok(code)
}
