//! The content-addressed build directory.
//!
//! The directory holds three stages. Each stage keys on its own
//! input:
//!
//! | Stage | Key | Value |
//! | --- | --- | --- |
//! | 1 | the compile key of one module | the module artifact and interface |
//! | 2 | the module content hashes of one program | the linked artifact |
//! | 3 | the verified-code key of one artifact | the verifier verdict |
//!
//! Stage 1 removes a compiler run. Stage 2 removes a link run and the
//! verification of the merged program. Stage 3 removes the verifier
//! pass of a load.
//!
//! No stage is a trust boundary. Every entry decodes through the
//! ordinary decoder, and a damaged file is a miss, not a trust hole.
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

const PROGRAM_TAG: &[u8] = b"lm-program-key-v1\0";
const PROGRAM_MAGIC: &[u8; 16] = b"lm-prog-entry-v1";
const PROGRAM_ENTRY_VERSION: u32 = 1;

/// Compute the stage-2 key of one program.
///
/// `root_module` is the path of the entry module. `modules` is the
/// module list in **link order**: the order the build loop hands the
/// units to the linker. The order is part of the input, because the
/// linker reads the units in that order and the merged output follows
/// it. A sort would drop that input and could return an artifact the
/// linker never produced.
///
/// A module content hash is the container hash of the module
/// artifact. It is exact, it costs one SHA-256 pass over bytes the
/// build loop already holds, and it covers the embedded core image.
/// A core edit therefore moves every module hash and misses this key.
pub fn program_key(root_module: &str, modules: &[(String, [u8; 32])]) -> [u8; 32] {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(PROGRAM_TAG);
    // The container format version: a new container encodes the
    // merged program differently.
    bytes.extend_from_slice(&(lm_bytecode::VERSION as u32).to_le_bytes());
    // The compiler ABI version: it fixes every hash domain, so it
    // fixes the definition hashes the linker merges on.
    bytes.extend_from_slice(&lm_bytecode::identity::COMPILER_ABI_VERSION.to_le_bytes());
    // The verifier version: the link step verifies the merged
    // program, so a stage-2 hit replays that verdict.
    bytes.extend_from_slice(&lm_verify::VERIFIER_VERSION.to_le_bytes());
    // The operation manifest: the linker and the verifier both read
    // the operation table.
    bytes.extend_from_slice(&lm_abi::manifest_digest());
    // The entry module: the same module set with another entry links
    // to another program.
    write_str(&mut bytes, root_module);
    bytes.extend_from_slice(&(modules.len() as u32).to_le_bytes());
    for (path, hash) in modules {
        write_str(&mut bytes, path);
        bytes.extend_from_slice(hash);
    }
    sha256(&bytes)
}

/// One stage-2 entry: the linked artifact and its two hashes.
///
/// The entry carries the hashes because the semantic hash costs a
/// full `module_identity` pass over the merged program. That pass is
/// a large part of the work a stage-2 hit removes, so a hit must not
/// pay it again. The container hash is cheap, and it travels with the
/// semantic hash to keep one format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramEntry {
    pub artifact: Vec<u8>,
    pub semantic_hash: [u8; 32],
    pub container_hash: [u8; 32],
}

/// Encode one stage-2 entry.
fn encode_program_entry(entry: &ProgramEntry) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(entry.artifact.len() + 88);
    bytes.extend_from_slice(PROGRAM_MAGIC);
    bytes.extend_from_slice(&PROGRAM_ENTRY_VERSION.to_le_bytes());
    bytes.extend_from_slice(&entry.semantic_hash);
    bytes.extend_from_slice(&entry.container_hash);
    bytes.extend_from_slice(&(entry.artifact.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&entry.artifact);
    bytes
}

/// Decode one stage-2 entry. `None` marks a damaged file, which the
/// caller treats as a miss.
fn decode_program_entry(bytes: &[u8]) -> Option<ProgramEntry> {
    let mut input = Reader::new(bytes);
    if input.take(PROGRAM_MAGIC.len())? != PROGRAM_MAGIC {
        return None;
    }
    if input.u32()? != PROGRAM_ENTRY_VERSION {
        return None;
    }
    let semantic_hash = input.hash()?;
    let container_hash = input.hash()?;
    // `bytes` checks the length against the remaining input before it
    // copies, so a large length field allocates nothing.
    let artifact = input.bytes()?;
    if !input.at_end() {
        return None;
    }
    Some(ProgramEntry {
        artifact,
        semantic_hash,
        container_hash,
    })
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

    fn programs(&self) -> PathBuf {
        self.root.join("cache").join("programs")
    }

    fn program_path(&self, key: &[u8; 32]) -> PathBuf {
        self.programs().join(format!("{}.lma", hex(key)))
    }

    /// Read one stage-2 entry. A missing, unreadable, or damaged
    /// entry is a miss, never an error: the linker simply runs again.
    ///
    /// The stored artifact decodes through the ordinary decoder
    /// before the caller uses it, exactly like a stage-1 entry.
    pub fn read_program(&self, key: &[u8; 32]) -> Option<ProgramEntry> {
        let bytes = std::fs::read(self.program_path(key)).ok()?;
        let entry = decode_program_entry(&bytes)?;
        lm_bytecode::decode(&entry.artifact).ok()?;
        Some(entry)
    }

    /// Write one stage-2 entry with an atomic rename.
    pub fn write_program(&self, key: &[u8; 32], entry: &ProgramEntry) -> Result<(), String> {
        // The length field is a `u32`. Reject a larger artifact
        // instead of writing a truncated length.
        if entry.artifact.len() > u32::MAX as usize {
            return Err(format!(
                "error: the program is {} bytes; the cache entry holds at \
                 most {} bytes\n",
                entry.artifact.len(),
                u32::MAX
            ));
        }
        let dir = self.programs();
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("error: cannot create `{}`: {e}\n", dir.display()))?;
        write_atomic(&self.program_path(key), &encode_program_entry(entry))
    }
}

// ---------------------------------------------------------------
// Stage 3: the persistent verified-code store.
// ---------------------------------------------------------------

const VERDICT_MAGIC: &[u8; 16] = b"lm-verdict-v1\0\0\0";
const VERDICT_VERSION: u32 = 1;
/// The key of one verified-code record: the verification hash, the
/// compiler ABI version, and the verifier version.
///
/// The type repeats `lm_vm::VerifiedKey`. `lm-compiler` must not
/// depend on `lm-vm`, so the store holds plain values and the caller
/// converts. The caller must build the key with `lm_vm::verified_key`
/// from the decoded module, never from a stored value.
pub type VerdictKey = ([u8; 32], u32, u32);

/// One stored admission verdict. It carries no fact.
///
/// The artifact declares its own core role table, and the verifier
/// proves the shape of every filled slot before the verdict exists.
/// A verdict therefore records only that the verifier admitted the
/// key, and a damaged entry can never supply a wrong resolved value.
/// The type matches `lm_vm::VerifiedRecord`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Verdict;

/// Encode one verdict record.
///
/// The format is a magic, a version, and the full key. The verdict
/// itself carries no field: it records that the verifier admitted the
/// key.
pub fn encode_verdict(key: &VerdictKey, _verdict: &Verdict) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(VERDICT_MAGIC);
    bytes.extend_from_slice(&VERDICT_VERSION.to_le_bytes());
    bytes.extend_from_slice(&key.0);
    bytes.extend_from_slice(&key.1.to_le_bytes());
    bytes.extend_from_slice(&key.2.to_le_bytes());
    bytes
}

/// Decode one verdict record. `None` marks a damaged file.
///
/// The reader checks every field against the remaining input before it
/// reads, so a short or crafted file allocates nothing.
pub fn decode_verdict(bytes: &[u8]) -> Option<(VerdictKey, Verdict)> {
    let mut input = Reader::new(bytes);
    if input.take(VERDICT_MAGIC.len())? != VERDICT_MAGIC {
        return None;
    }
    if input.u32()? != VERDICT_VERSION {
        return None;
    }
    let key: VerdictKey = (input.hash()?, input.u32()?, input.u32()?);
    if !input.at_end() {
        return None;
    }
    Some((key, Verdict))
}

/// The persistent verified-code store.
///
/// The store answers "did the verifier admit these exact bytes
/// before?" across processes. The in-process cache of `lm-vm` answers
/// the same question inside one load, and a tool that loads one
/// program once never hits it.
///
/// The store is a cache, not an authority. The key binds a record to
/// one module. `VerifiedStore::read` compares the whole stored key.
/// `lm_vm::load_with_record` then recomputes the key from the decoded
/// content. It rejects a record under any other key. A damaged or
/// wrongly filed record is a miss.
///
/// A verdict carries no fact, so a damaged or forged entry supplies
/// no resolved value: the core layout comes from the artifact, and the
/// verifier proves it. A hand-written entry under the right key still
/// asserts an admission the verifier never gave, so the store needs
/// the same trust as the rest of the build directory. Keep the store
/// in a directory the user controls, never beside a foreign artifact.
#[derive(Debug, Clone)]
pub struct VerifiedStore {
    root: PathBuf,
}

/// The user cache directory of this tool.
///
/// The value follows `LM_CACHE_DIR`, then `XDG_CACHE_HOME/lm`, then
/// `HOME/.cache/lm`. With none of the three set, the store falls back
/// to `.lm-cache` in the current directory.
pub fn user_cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LM_CACHE_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Ok(dir) = std::env::var("XDG_CACHE_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("lm");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".cache").join("lm");
        }
    }
    PathBuf::from(".lm-cache")
}

impl VerifiedStore {
    /// The store inside one build directory.
    pub fn new(build_root: &Path) -> VerifiedStore {
        VerifiedStore {
            root: build_root.join("cache").join("verified"),
        }
    }

    /// The store of one artifact on disk.
    ///
    /// Stage 3 must stay reachable for an artifact with no source,
    /// for example `lm run third-party.lma`. The rule walks up from
    /// the artifact directory: a package above the artifact gives
    /// `<package>/build`, and no package gives the user cache
    /// directory. The directory is created on the first write, never
    /// on a read.
    pub fn for_artifact(artifact: &Path) -> VerifiedStore {
        let root = match crate::graph::find_package_dir(artifact) {
            Ok(package) => package.join("build"),
            // An artifact with no package around it, for example a
            // foreign `.lma`, keeps its store in the user cache. A
            // directory beside the artifact would let the supplier of
            // the artifact write the verdict of the artifact, and a
            // verdict replaces a verifier run.
            Err(_) => user_cache_dir(),
        };
        VerifiedStore::new(&root)
    }

    /// The directory the store writes into.
    pub fn dir(&self) -> &Path {
        &self.root
    }

    /// The record path of one key. The name holds the 32-byte hash
    /// part; the file holds the full key, and a read checks it.
    pub fn record_path(&self, key: &VerdictKey) -> PathBuf {
        self.root.join(format!("{}.lmv", hex(&key.0)))
    }

    /// Read the record of one key. A missing, unreadable, damaged, or
    /// wrongly filed record is a miss, never an error.
    pub fn read(&self, key: &VerdictKey) -> Option<Verdict> {
        let bytes = std::fs::read(self.record_path(key)).ok()?;
        let (stored_key, verdict) = decode_verdict(&bytes)?;
        // The file name holds only part of the key. Check the whole
        // key before the record leaves the store.
        if stored_key != *key {
            return None;
        }
        Some(verdict)
    }

    /// Write the record of one key with an atomic rename.
    pub fn write(&self, key: &VerdictKey, verdict: &Verdict) -> Result<(), String> {
        std::fs::create_dir_all(&self.root)
            .map_err(|e| format!("error: cannot create `{}`: {e}\n", self.root.display()))?;
        write_atomic(&self.record_path(key), &encode_verdict(key, verdict))
    }
}

/// Admit one module through the persistent verified-code store.
///
/// The function holds the store policy and no VM type, because
/// `lm-compiler` must not depend on `lm-vm`. The caller supplies the
/// two admission paths:
///
/// - `from_record` admits the module through a stored verdict. It
///   must call `lm_vm::load_with_record`, which proves the verdict
///   belongs to the module.
/// - `verify` runs the full verifier and returns the admitted module
///   and the verdict it produced.
///
/// The caller must build `key` with `lm_vm::verified_key` from this
/// decoded module. A rejected module returns the error of `verify`
/// and writes no record.
pub fn load_through_store<T, E>(
    store: &VerifiedStore,
    key: &VerdictKey,
    module: lm_bytecode::Module,
    from_record: impl FnOnce(lm_bytecode::Module, &Verdict) -> Result<T, E>,
    verify: impl FnOnce(lm_bytecode::Module) -> Result<(T, Verdict), E>,
) -> Result<T, E> {
    if let Some(verdict) = store.read(key) {
        // An unlinked module never runs. `lm_vm::load_with_record`
        // owns that decision and repeats the check. The test here only
        // chooses the path, so a stale entry stays a miss and never
        // becomes a failure.
        if module.imports.is_empty() {
            return from_record(module, &verdict);
        }
    }
    let (loaded, verdict) = verify(module)?;
    // The write is best effort. A read-only or full build directory
    // costs a verifier run on the next load, and nothing else.
    let _ = store.write(key, &verdict);
    Ok(loaded)
}

/// A bounds-checked reader over a cache entry.
///
/// Every method checks the remaining input before it reads. A count
/// never sizes an allocation before the check.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn at_end(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        if count > self.remaining() {
            return None;
        }
        let out = &self.bytes[self.at..self.at + count];
        self.at += count;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_le_bytes(bytes))
    }

    fn hash(&mut self) -> Option<[u8; 32]> {
        self.take(32)?.try_into().ok()
    }

    /// A `u32` length and that many bytes. The length is checked
    /// against the remaining input before the copy.
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let count = self.u32()? as usize;
        Some(self.take(count)?.to_vec())
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
