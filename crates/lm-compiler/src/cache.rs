//! The content-addressed build directory.
//!
//! The directory holds two stages. Each stage keys on its own
//! input:
//!
//! | Stage | Key | Value |
//! | --- | --- | --- |
//! | 1 | the compile key of one module | the module artifact and interface |
//! | 2 | the root `ArtifactId` | the LMAR artifact bytes |
//!
//! Stage 1 removes a compiler run. Stage 2 reuses artifact bytes.
//!
//! Every stage is a trust boundary, because every entry is a file. An
//! earlier claim here said no stage is one, on the ground that a
//! damaged file is a miss. That covers damage and never covers
//! forgery: a writer of the directory builds a well-formed entry under
//! the correct key.
//!
//! Two rules hold the boundary:
//!
//! A stage-2 hit checks the stored root identity.
//! A damaged entry causes a fresh encoding.
//!
//! # Stage 1
//!
//! One entry holds the artifact and the interface of one compiled
//! module, named by the compile key. The key covers everything the
//! compilation of that module reads:
//!
//! - the container format version, the compiler ABI version, the
//!   verifier version, and the operation manifest digest;
//! - the digest of the core sources, because every module embeds the
//!   core image;
//! - the module path and whether the module holds the program entry;
//! - the exact source bytes;
//! - the root names the module may use, and the **interface identity**
//!   of every visible module.
//!
//! The interface identity covers the export names, kinds, and
//! interface hashes, and no definition hash. An edit to an exported
//! body therefore leaves the key of every dependent module unchanged,
//! so only the edited package rebuilds. An edit to a signature moves
//! the interface identity and rebuilds the dependents.
//!
//! This cache answers whether the compiler must run again.
//! Neither key stands in for the other.

use lm_bytecode::hash::hash256;
use lm_bytecode::interface::Interface;
use std::path::{Path, PathBuf};

const TAG: &[u8] = b"lm-build-key-v1\0";

/// The interface identity of one module: the export surface without
/// any implementation hash.
pub fn interface_identity(interface: &Interface) -> [u8; 32] {
    lm_bytecode::interface::interface_identity(interface)
}

fn write_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Compute the compile key of one module.
pub fn compile_key(
    module_path: &str,
    is_main: bool,
    source: &str,
    roots: &[(String, String)],
    visible: &[(String, [u8; 32])],
) -> [u8; 32] {
    compile_key_with_bundle(
        module_path,
        is_main,
        source,
        roots,
        visible,
        &lm_abi::standard_bundle(),
    )
}

/// Compute one module compile key under an ABI bundle.
pub fn compile_key_with_bundle(
    module_path: &str,
    is_main: bool,
    source: &str,
    roots: &[(String, String)],
    visible: &[(String, [u8; 32])],
    bundle: &lm_abi::AbiBundle,
) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TAG);
    bytes.extend_from_slice(&(lm_bytecode::VERSION as u32).to_le_bytes());
    bytes.extend_from_slice(&lm_bytecode::identity::COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&lm_verify::VERIFIER_VERSION.to_le_bytes());
    bytes.extend_from_slice(&bundle.digest());
    bytes.extend_from_slice(&lm_hir::core_source_digest());
    write_str(&mut bytes, module_path);
    bytes.push(u8::from(is_main));
    write_str(&mut bytes, source);
    let mut roots: Vec<(String, String)> = roots.to_vec();
    roots.sort();
    bytes.extend_from_slice(&(roots.len() as u32).to_le_bytes());
    for (name, prefix) in &roots {
        write_str(&mut bytes, name);
        write_str(&mut bytes, prefix);
    }
    let mut visible: Vec<(String, [u8; 32])> = visible.to_vec();
    visible.sort();
    bytes.extend_from_slice(&(visible.len() as u32).to_le_bytes());
    for (path, id) in &visible {
        write_str(&mut bytes, path);
        bytes.extend_from_slice(id);
    }
    hash256(&bytes)
}

/// The build directory layout.
#[derive(Debug, Clone)]
pub struct BuildDir {
    root: PathBuf,
}

impl BuildDir {
    pub fn new(root: &Path) -> BuildDir {
        BuildDir {
            root: root.to_path_buf(),
        }
    }

    /// The directory of the linked program artifacts.
    pub fn debug(&self) -> PathBuf {
        self.root.join("debug")
    }

    fn modules(&self) -> PathBuf {
        self.root.join("cache").join("modules")
    }

    fn entry_paths(&self, key: &[u8; 32]) -> (PathBuf, PathBuf) {
        let name = hex(key);
        let dir = self.modules();
        (
            dir.join(format!("{name}.lma")),
            dir.join(format!("{name}.lmi")),
        )
    }

    /// Read one cached entry. A missing or unreadable entry is a miss,
    /// never an error: the compiler simply runs again.
    pub fn read(&self, key: &[u8; 32]) -> Option<(Vec<u8>, Vec<u8>)> {
        let (artifact, interface) = self.entry_paths(key);
        let artifact = std::fs::read(artifact).ok()?;
        let interface = std::fs::read(interface).ok()?;
        Some((artifact, interface))
    }

    /// Write one cached entry with atomic renames.
    pub fn write(&self, key: &[u8; 32], artifact: &[u8], interface: &[u8]) -> Result<(), String> {
        let dir = self.modules();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("error: cannot create `{}`: {e}\n", dir.display()))?;
        let (artifact_path, interface_path) = self.entry_paths(key);
        write_atomic(&artifact_path, artifact)?;
        write_atomic(&interface_path, interface)
    }

    fn artifacts(&self) -> PathBuf {
        self.root.join("cache").join("artifacts")
    }

    fn artifact_path(&self, id: &lm_bytecode::artifact::ArtifactId) -> PathBuf {
        self.artifacts().join(format!("{id}.lma"))
    }

    /// Read one artifact by its semantic identity.
    /// A damaged entry is a cache miss.
    pub fn read_artifact(&self, id: &lm_bytecode::artifact::ArtifactId) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.artifact_path(id)).ok()?;
        let artifact = lm_bytecode::artifact::decode(&bytes).ok()?;
        (artifact.id() == *id).then_some(bytes)
    }

    /// Write one artifact with an atomic rename.
    pub fn write_artifact(
        &self,
        id: &lm_bytecode::artifact::ArtifactId,
        bytes: &[u8],
    ) -> Result<(), String> {
        let dir = self.artifacts();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("error: cannot create `{}`: {e}\n", dir.display()))?;
        write_atomic(&self.artifact_path(id), bytes)
    }
}

/// Write one file atomically: a fresh temporary file in the same
/// directory, then a rename over the final path.
///
/// The temporary name is unique and the create is exclusive, so the
/// write never opens a file that already exists. A fixed `.tmp` name
/// with a plain write followed a symbolic link. A package that
/// shipped `build/debug/<name>.lma.tmp` as a link therefore made a
/// build write outside the package.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{unique}.tmp", std::process::id()));
    let tmp = Path::new(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", tmp.display()))?;
    let written = file
        .write_all(bytes)
        .map_err(|e| format!("error: cannot write `{}`: {e}\n", tmp.display()));
    drop(file);
    if let Err(message) = written {
        let _ = std::fs::remove_file(tmp);
        return Err(message);
    }
    std::fs::rename(tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        format!("error: cannot rename to `{}`: {e}\n", path.display())
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
