//! Canonical encoding for thin and fat artifacts.

use super::{Artifact, ArtifactError, ArtifactGraphError, ArtifactId, ArtifactRecord};
use crate::DecodeError;
use std::fmt;

const MAGIC: &[u8; 4] = b"LMAR";

/// The artifact package format version.
pub const FORMAT_VERSION: u16 = 1;

pub(super) const HEADER_LEN: usize = 4 + 2 + 32 + 32 + 4;
const MIN_RECORD_BYTES: usize = 32 + 4 + 4;
const MIN_DEPENDENCY_BYTES: usize = 4 + 1 + 32;

/// Limits for one artifact package decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactLimits {
    pub max_bytes: usize,
    pub max_records: usize,
    pub max_dependencies_per_record: usize,
    pub max_module_bytes: usize,
    pub max_code_bytes: usize,
    pub max_namespace_bytes: usize,
}

impl Default for ArtifactLimits {
    fn default() -> ArtifactLimits {
        ArtifactLimits {
            max_bytes: 256 * 1024 * 1024,
            max_records: 4096,
            max_dependencies_per_record: 4096,
            max_module_bytes: 64 * 1024 * 1024,
            max_code_bytes: 256 * 1024 * 1024,
            max_namespace_bytes: 4096,
        }
    }
}

/// An artifact encoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactEncodeError {
    LengthOverflow,
    BundleMismatch {
        record: ArtifactId,
        expected: [u8; 32],
        found: [u8; 32],
    },
}

impl fmt::Display for ArtifactEncodeError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactEncodeError::LengthOverflow => {
                out.write_str("the artifact is too large to encode")
            }
            ArtifactEncodeError::BundleMismatch {
                record,
                expected,
                found,
            } => write!(
                out,
                "artifact record {record} uses ABI bundle {}, but this encoder uses {}",
                digest_text(found),
                digest_text(expected)
            ),
        }
    }
}

impl std::error::Error for ArtifactEncodeError {}

/// An artifact decoding or graph failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactDecodeError {
    Truncated,
    BadMagic,
    BadVersion(u16),
    BadBundle {
        expected: [u8; 32],
        found: [u8; 32],
    },
    BadLength,
    BadUtf8,
    TrailingBytes,
    Limit(&'static str),
    Module(DecodeError),
    Record(ArtifactError),
    IdentityMismatch {
        stored: ArtifactId,
        computed: ArtifactId,
    },
    Graph(ArtifactGraphError),
}

impl fmt::Display for ArtifactDecodeError {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArtifactDecodeError::Truncated => out.write_str("the artifact is truncated"),
            ArtifactDecodeError::BadMagic => out.write_str("the artifact has a bad magic header"),
            ArtifactDecodeError::BadVersion(version) => {
                write!(out, "artifact format version {version} is unsupported")
            }
            ArtifactDecodeError::BadBundle { expected, found } => write!(
                out,
                "the artifact uses ABI bundle {}, but this decoder uses {}",
                digest_text(found),
                digest_text(expected)
            ),
            ArtifactDecodeError::BadLength => {
                out.write_str("an artifact length exceeds the remaining input")
            }
            ArtifactDecodeError::BadUtf8 => {
                out.write_str("an artifact namespace is not valid UTF-8")
            }
            ArtifactDecodeError::TrailingBytes => out.write_str("extra bytes follow the artifact"),
            ArtifactDecodeError::Limit(name) => {
                write!(out, "the artifact exceeds the {name} limit")
            }
            ArtifactDecodeError::Module(error) => {
                write!(out, "an artifact module is invalid: {error}")
            }
            ArtifactDecodeError::Record(error) => error.fmt(out),
            ArtifactDecodeError::IdentityMismatch { stored, computed } => write!(
                out,
                "artifact record identity {stored} does not match computed identity {computed}"
            ),
            ArtifactDecodeError::Graph(error) => error.fmt(out),
        }
    }
}

impl std::error::Error for ArtifactDecodeError {}

/// Encode one artifact under the standard ABI bundle.
pub fn encode(artifact: &Artifact) -> Result<Vec<u8>, ArtifactEncodeError> {
    let bundle = lm_abi::standard_bundle();
    encode_with_bundle(artifact, &bundle)
}

/// Encode one artifact under an explicit ABI bundle.
pub fn encode_with_bundle(
    artifact: &Artifact,
    bundle: &lm_abi::AbiBundle,
) -> Result<Vec<u8>, ArtifactEncodeError> {
    let mut modules = Vec::with_capacity(artifact.records().len());
    let mut total = HEADER_LEN;
    for record in artifact.records() {
        if record.bundle_digest() != bundle.digest() {
            return Err(ArtifactEncodeError::BundleMismatch {
                record: record.id(),
                expected: bundle.digest(),
                found: record.bundle_digest(),
            });
        }
        let module = crate::encode_with_bundle(record.module(), bundle);
        total = total
            .checked_add(record_size(record, module.len())?)
            .ok_or(ArtifactEncodeError::LengthOverflow)?;
        modules.push(module);
    }
    let count =
        u32::try_from(artifact.records().len()).map_err(|_| ArtifactEncodeError::LengthOverflow)?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&bundle.digest());
    out.extend_from_slice(artifact.id().as_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    for (record, module) in artifact.records().iter().zip(modules) {
        out.extend_from_slice(record.id().as_bytes());
        write_u32(&mut out, record.dependencies().len())?;
        for dependency in record.dependencies() {
            write_bytes(&mut out, dependency.namespace().as_bytes())?;
            out.extend_from_slice(dependency.artifact().as_bytes());
        }
        write_bytes(&mut out, &module)?;
    }
    Ok(out)
}

/// Decode one artifact under the standard ABI bundle and default limits.
pub fn decode(bytes: &[u8]) -> Result<Artifact, ArtifactDecodeError> {
    let bundle = lm_abi::standard_bundle();
    decode_with_bundle(bytes, &bundle, ArtifactLimits::default())
}

/// Decode one artifact under an explicit ABI bundle and explicit limits.
pub fn decode_with_bundle(
    bytes: &[u8],
    bundle: &lm_abi::AbiBundle,
    limits: ArtifactLimits,
) -> Result<Artifact, ArtifactDecodeError> {
    if bytes.len() > limits.max_bytes {
        return Err(ArtifactDecodeError::Limit("total byte"));
    }
    let mut cursor = Cursor::new(bytes);
    if cursor.take(4)? != MAGIC {
        return Err(ArtifactDecodeError::BadMagic);
    }
    let version = cursor.u16()?;
    if version != FORMAT_VERSION {
        return Err(ArtifactDecodeError::BadVersion(version));
    }
    let found = cursor.digest()?;
    let expected = bundle.digest();
    if found != expected {
        return Err(ArtifactDecodeError::BadBundle { expected, found });
    }
    let root = ArtifactId::from_bytes(cursor.digest()?);
    let count = cursor.count(limits.max_records, MIN_RECORD_BYTES, "record")?;
    let mut records = Vec::with_capacity(count);
    let mut code_bytes = 0usize;
    for _ in 0..count {
        let stored = ArtifactId::from_bytes(cursor.digest()?);
        let dependency_count = cursor.count(
            limits.max_dependencies_per_record,
            MIN_DEPENDENCY_BYTES,
            "direct dependency",
        )?;
        let mut dependencies = Vec::with_capacity(dependency_count);
        for _ in 0..dependency_count {
            let namespace = cursor.string(limits.max_namespace_bytes)?;
            let artifact = ArtifactId::from_bytes(cursor.digest()?);
            dependencies.push(
                super::ArtifactDependency::new(namespace, artifact)
                    .map_err(ArtifactDecodeError::Record)?,
            );
        }
        let module_length = cursor.length()?;
        if module_length > limits.max_module_bytes {
            return Err(ArtifactDecodeError::Limit("module byte"));
        }
        code_bytes = code_bytes
            .checked_add(module_length)
            .ok_or(ArtifactDecodeError::Limit("decoded code byte"))?;
        if code_bytes > limits.max_code_bytes {
            return Err(ArtifactDecodeError::Limit("decoded code byte"));
        }
        let module_bytes = cursor.take(module_length)?;
        let module =
            crate::decode_with_bundle(module_bytes, bundle).map_err(ArtifactDecodeError::Module)?;
        let record = ArtifactRecord::new_with_bundle(module, dependencies, bundle)
            .map_err(ArtifactDecodeError::Record)?;
        if record.id() != stored {
            return Err(ArtifactDecodeError::IdentityMismatch {
                stored,
                computed: record.id(),
            });
        }
        records.push(record);
    }
    if cursor.remaining() != 0 {
        return Err(ArtifactDecodeError::TrailingBytes);
    }
    Artifact::from_records(root, records).map_err(ArtifactDecodeError::Graph)
}

fn record_size(
    record: &ArtifactRecord,
    module_length: usize,
) -> Result<usize, ArtifactEncodeError> {
    let mut size = MIN_RECORD_BYTES;
    for dependency in record.dependencies() {
        size = size
            .checked_add(4 + dependency.namespace().len() + 32)
            .ok_or(ArtifactEncodeError::LengthOverflow)?;
    }
    size.checked_add(module_length)
        .ok_or(ArtifactEncodeError::LengthOverflow)
}

fn write_u32(out: &mut Vec<u8>, value: usize) -> Result<(), ArtifactEncodeError> {
    let value = u32::try_from(value).map_err(|_| ArtifactEncodeError::LengthOverflow)?;
    out.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ArtifactEncodeError> {
    write_u32(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn digest_text(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        write!(&mut out, "{byte:02x}").expect("writing to a string cannot fail");
    }
    out
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ArtifactDecodeError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(ArtifactDecodeError::Truncated)?;
        if end > self.bytes.len() {
            return Err(ArtifactDecodeError::Truncated);
        }
        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, ArtifactDecodeError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, ArtifactDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn digest(&mut self) -> Result<[u8; 32], ArtifactDecodeError> {
        let bytes = self.take(32)?;
        let mut digest = [0u8; 32];
        digest.copy_from_slice(bytes);
        Ok(digest)
    }

    fn length(&mut self) -> Result<usize, ArtifactDecodeError> {
        let count = self.u32()? as usize;
        if count > self.remaining() {
            return Err(ArtifactDecodeError::BadLength);
        }
        Ok(count)
    }

    fn count(
        &mut self,
        maximum: usize,
        minimum_size: usize,
        limit_name: &'static str,
    ) -> Result<usize, ArtifactDecodeError> {
        let count = self.u32()? as usize;
        if count > maximum {
            return Err(ArtifactDecodeError::Limit(limit_name));
        }
        if count > self.remaining() / minimum_size {
            return Err(ArtifactDecodeError::BadLength);
        }
        Ok(count)
    }

    fn string(&mut self, maximum: usize) -> Result<String, ArtifactDecodeError> {
        let length = self.u32()? as usize;
        if length > maximum {
            return Err(ArtifactDecodeError::Limit("namespace byte"));
        }
        let bytes = self.take(length)?;
        let text = std::str::from_utf8(bytes).map_err(|_| ArtifactDecodeError::BadUtf8)?;
        Ok(text.to_string())
    }
}
