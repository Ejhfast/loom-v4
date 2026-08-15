//! The content-addressed build directory.
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
//! This cache and the verified-code cache answer different questions.
//! This one answers "must the compiler run again?". The verified-code
//! cache answers "did the verifier admit these exact bytes before?".
//! Neither key stands in for the other.

use lm_bytecode::hash::sha256;
use lm_bytecode::interface::Interface;
use std::path::{Path, PathBuf};

const TAG: &[u8] = b"lm-build-key-v1\0";

/// The interface identity of one module: the export surface without
/// any implementation hash.
pub fn interface_identity(interface: &Interface) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"lm-iface-set-v1\0");
    write_str(&mut bytes, &interface.module_path);
    let mut exports: Vec<(u8, &str, [u8; 32])> = interface
        .exports
        .iter()
        .map(|e| (e.kind.tag(), e.name.as_str(), e.iface_hash))
        .collect();
    exports.sort();
    bytes.extend_from_slice(&(exports.len() as u32).to_le_bytes());
    for (kind, name, hash) in exports {
        bytes.push(kind);
        write_str(&mut bytes, name);
        bytes.extend_from_slice(&hash);
    }
    sha256(&bytes)
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
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TAG);
    bytes.extend_from_slice(&(lm_bytecode::VERSION as u32).to_le_bytes());
    bytes.extend_from_slice(&lm_bytecode::identity::COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&lm_verify::VERIFIER_VERSION.to_le_bytes());
    bytes.extend_from_slice(&lm_abi::manifest_digest());
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
    sha256(&bytes)
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
}

/// Write one file atomically: a temporary file in the same directory,
/// then a rename over the final path.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    std::fs::write(tmp, bytes)
        .map_err(|e| format!("error: cannot write `{}`: {e}\n", tmp.display()))?;
    std::fs::rename(tmp, path)
        .map_err(|e| format!("error: cannot rename to `{}`: {e}\n", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
