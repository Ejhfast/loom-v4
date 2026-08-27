//! Artifact-backed code preparation for snapshots.

use super::{Image, ImageError, ImageReason};
use crate::NamespaceRuntime;
use lm_bytecode::artifact::{Artifact, ArtifactLimits, LinkUnit};
use lm_link::CodeArena;
use std::sync::Arc;

/// Verified code namespaces used by one admitted image.
#[derive(Debug, Clone)]
pub(crate) struct SnapshotCode {
    artifacts: Arc<[Arc<Artifact>]>,
    namespaces: Arc<[Arc<NamespaceRuntime>]>,
}

impl SnapshotCode {
    pub(crate) fn new(
        artifacts: Vec<Arc<Artifact>>,
        namespaces: Vec<Arc<NamespaceRuntime>>,
    ) -> SnapshotCode {
        SnapshotCode {
            artifacts: artifacts.into(),
            namespaces: namespaces.into(),
        }
    }

    pub(crate) fn artifacts(&self) -> &[Arc<Artifact>] {
        &self.artifacts
    }

    pub(crate) fn artifact(&self, ordinal: u32) -> Option<&Artifact> {
        self.artifacts.get(ordinal as usize).map(Arc::as_ref)
    }

    pub(crate) fn namespace(&self, ordinal: u32) -> Option<&Arc<NamespaceRuntime>> {
        self.namespaces.get(ordinal as usize)
    }

    pub(crate) fn namespaces(&self) -> &[Arc<NamespaceRuntime>] {
        &self.namespaces
    }

    pub(crate) fn tables(&self) -> Option<&Arc<NamespaceRuntime>> {
        self.namespaces.last()
    }

    pub(crate) fn contains_function(&self, function: u32) -> bool {
        self.namespaces
            .iter()
            .any(|namespace| namespace.code_namespace().contains_function(function))
    }

    pub(crate) fn contains_class(&self, class: u32) -> bool {
        self.namespaces
            .iter()
            .any(|namespace| namespace.code_namespace().contains_class(class))
    }

    pub(crate) fn bundle(&self) -> Option<&Arc<lm_abi::AbiBundle>> {
        self.tables().map(|namespace| namespace.bundle())
    }
}

/// Decode, verify, and publish the code of one external image.
pub(crate) fn prepare_external(
    image: &Image,
    runtime_core: Option<Arc<LinkUnit>>,
    bundle: Arc<lm_abi::AbiBundle>,
    known: Option<&[Arc<NamespaceRuntime>]>,
) -> Result<SnapshotCode, ImageError> {
    let mut artifacts = Vec::new();
    artifacts
        .try_reserve_exact(image.artifacts.len())
        .map_err(|_| {
            ImageError::admission(ImageReason::Budget, "the artifact table is too large")
        })?;
    for (index, bytes) in image.artifacts.iter().enumerate() {
        let artifact =
            lm_bytecode::artifact::decode_with_bundle(bytes, &bundle, ArtifactLimits::default())
                .map_err(|error| {
                    ImageError::admission(
                        ImageReason::Code,
                        format!("artifact {index} did not decode: {error}"),
                    )
                })?;
        artifacts.push(Arc::new(artifact));
    }

    if let Some(known) = known {
        for artifact in &mut artifacts {
            if let Some(existing) = known
                .iter()
                .flat_map(|runtime| runtime.code_namespace().artifacts())
                .find(|candidate| candidate.id() == artifact.id())
            {
                *artifact = existing.clone();
            }
        }
        let mut shared_namespaces = Vec::new();
        shared_namespaces
            .try_reserve_exact(image.namespaces.len())
            .map_err(|_| {
                ImageError::admission(ImageReason::Budget, "the namespace table is too large")
            })?;
        for manifest in &image.namespaces {
            let found = known.iter().find(|runtime| {
                let chain = runtime.code_namespace().artifacts();
                manifest.artifacts.len() == chain.len()
                    && manifest
                        .artifacts
                        .iter()
                        .zip(chain.iter())
                        .all(|(ordinal, expected)| {
                            artifacts
                                .get(*ordinal as usize)
                                .is_some_and(|artifact| artifact.id() == expected.id())
                        })
            });
            let Some(found) = found else {
                shared_namespaces.clear();
                break;
            };
            shared_namespaces.push((*found).clone());
        }
        if shared_namespaces.len() == image.namespaces.len() {
            return Ok(SnapshotCode::new(artifacts, shared_namespaces));
        }
    }

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
    Ok(SnapshotCode::new(artifacts, namespaces))
}
