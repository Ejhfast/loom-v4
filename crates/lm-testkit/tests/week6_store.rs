//! Stage 2 and stage 3 of the build store.
//!
//! Stage 2 keys the linked artifact on the module content hashes, so
//! a rebuild with no source change never links again. Stage 3 keys
//! the verifier verdict on the verified-code key, so a second load of
//! one artifact never meets the verifier again.
//!
//! Every case builds a real package tree in a temporary directory, so
//! the tests exercise the same path the `lm` tool uses.

use lm_compiler::{build_package, load_through_store, BuildReport, Verdict, VerifiedStore};
use std::path::{Path, PathBuf};

/// One temporary directory that removes itself.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> TempTree {
        // The label plus the process id and a counter keeps two runs
        // apart without an external crate.
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("lm-store-{label}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the temporary tree is created");
        TempTree { root }
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().expect("a file has a directory"))
            .expect("the directory is created");
        std::fs::write(path, text).expect("the file is written");
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    fn build(&self, package: &str) -> Result<BuildReport, String> {
        build_package(&self.path(package), &self.path("build"))
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The two-package workspace of `examples/05-modules`, as text.
fn workspace(tree: &TempTree) {
    tree.write(
        "mathlib/lm.package",
        "[package]\nname = \"mathlib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "mathlib/src/matrix.lm",
        "class Matrix\n\
         \x20 rows: Int = 1\n\
         \x20 cols: Int = 1\n\
         \n\
         \x20 def init(mut self, rows: Int, cols: Int)\n\
         \x20   self.rows = rows\n\
         \x20   self.cols = cols\n\
         \x20 end\n\
         \n\
         \x20 def area(self): Int\n\
         \x20   self.rows * self.cols\n\
         \x20 end\n\
         end\n\
         \n\
         def describe(m: Matrix): String\n\
         \x20 \"#{m.rows}x#{m.cols}\"\n\
         end\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nmathlib = { path = \"../mathlib\" }\n",
    );
    tree.write(
        "app/src/greeting.lm",
        "use mathlib.matrix\n\
         \n\
         def greet(name: String): String\n\
         \x20 \"Hello #{name}!\"\n\
         end\n\
         \n\
         def report(m: matrix.Matrix): String\n\
         \x20 \"#{matrix.describe(m)} has #{m.area()} cells\"\n\
         end\n",
    );
    tree.write(
        "app/src/main.lm",
        "use sys.io.write\n\
         use greeting\n\
         use mathlib.matrix\n\
         \n\
         def run() with Io.Write\n\
         \x20 m = matrix.Matrix(2, 3)\n\
         \x20 line = greeting.greet(\"Ada\")\n\
         \x20 print(\"#{line}\\n\")\n\
         \x20 print(\"#{greeting.report(m)}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
}

/// A one-package tree with no dependency, for the stage-3 cases.
fn small_package(tree: &TempTree) {
    tree.write(
        "solo/lm.package",
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "solo/src/main.lm",
        "def twice(n: Int): Int\n\x20 n * 2\nend\n\ntwice(21)\n",
    );
}

/// The stage-2 entry files of one build directory.
fn program_entries(tree: &TempTree) -> Vec<PathBuf> {
    let dir = tree.path("build/cache/programs");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no stage-2 directory `{}`: {e}", dir.display()))
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    entries
}

// ---------------------------------------------------------------
// The two record shapes.
//
// `lm-compiler` must not depend on `lm-vm`, so the store keeps plain
// values and the caller converts. `lm` converts the same way.
// ---------------------------------------------------------------

fn to_record(_verdict: &Verdict) -> lm_vm::VerifiedRecord {
    lm_vm::VerifiedRecord
}

fn to_verdict(_record: &lm_vm::VerifiedRecord) -> Verdict {
    Verdict
}

/// The load path of `lm run` on an artifact, with the verifier run
/// count the caller can read.
///
/// The body repeats `load_stored` in `lm-cli`: decode, compute the
/// key from the decoded content, and admit through the store. The
/// tool calls `lm_vm::load` on a miss. This test calls
/// `lm_vm::load_cached` with an empty cache, which runs the same
/// passes and counts them.
fn load_artifact(path: &Path, verifications: &mut u64) -> Result<lm_vm::LoadedModule, String> {
    load_artifact_in(&VerifiedStore::for_artifact(path), path, verifications)
}

/// The same path against an explicit store. A test that must not
/// write into the user cache directory names its own store.
fn load_artifact_in(
    store: &VerifiedStore,
    path: &Path,
    verifications: &mut u64,
) -> Result<lm_vm::LoadedModule, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    let module = lm_bytecode::decode(&bytes).map_err(|e| format!("cannot decode: {e}"))?;
    let key = lm_vm::verified_key(&module);
    load_through_store(
        store,
        &key,
        module,
        |module, verdict| lm_vm::load_with_record(module, &key, &to_record(verdict)),
        |module| {
            let mut cache = lm_vm::VerifiedCache::new();
            let loaded = lm_vm::load_cached(module, &mut cache)?;
            *verifications += cache.verifications;
            Ok((loaded, to_verdict(&lm_vm::VerifiedRecord)))
        },
    )
    .map_err(|e| format!("the loader rejected the artifact: {e}"))
}

/// Run one loaded program and return its result text.
fn run_loaded(loaded: &lm_vm::LoadedModule) -> String {
    let mut vm = lm_vm::Vm::new(loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert!(
        matches!(outcome, lm_vm::Outcome::Done(_)),
        "the program faulted"
    );
    format!("{outcome:?}")
}

// ---------------------------------------------------------------
// Stage 2: the linked-artifact cache.
// ---------------------------------------------------------------

/// A second build of one package hits stage 2 and never links again.
#[test]
fn a_second_build_hits_the_program_cache() {
    let tree = TempTree::new("stage2-hit");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    assert!(!first.program_cached, "the first build must link");
    assert_eq!(program_entries(&tree).len(), 1, "one stage-2 entry");
    let second = tree.build("app").expect("builds");
    assert!(second.program_cached, "the second build linked again");
    assert_eq!(second.compiled(), 0, "the second build compiled a module");
    // A hit still reports both program hashes and writes the program.
    assert_eq!(first.program_semantic, second.program_semantic);
    assert_eq!(first.program_container, second.program_container);
    let a = std::fs::read(first.program.clone().unwrap()).unwrap();
    let b = std::fs::read(second.program.clone().unwrap()).unwrap();
    assert_eq!(a, b, "the cached program bytes moved");
    assert_eq!(program_entries(&tree).len(), 1, "the hit wrote an entry");
}

/// A source edit moves a module content hash, so stage 2 misses and
/// the linker runs again.
#[test]
fn a_source_edit_misses_the_program_cache() {
    let tree = TempTree::new("stage2-edit");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("\"#{m.rows}x#{m.cols}\"", "\"#{m.rows} by #{m.cols}\""),
    );
    let second = tree.build("app").expect("builds");
    assert!(!second.program_cached, "the edit hit the program cache");
    assert_ne!(
        first.program_container, second.program_container,
        "the edit did not change the program"
    );
    assert_eq!(program_entries(&tree).len(), 2, "the edit kept one entry");
    // The old module set still hits, so the key covers the set only.
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("\"#{m.rows} by #{m.cols}\"", "\"#{m.rows}x#{m.cols}\""),
    );
    let third = tree.build("app").expect("builds");
    assert!(third.program_cached, "the restored set missed");
    assert_eq!(first.program_container, third.program_container);
}

/// A damaged stage-2 entry is a miss, not an error. The build runs
/// the linker again and produces byte-identical program bytes.
#[test]
fn a_damaged_program_entry_is_a_miss() {
    let tree = TempTree::new("stage2-damaged");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    let good = std::fs::read(first.program.clone().unwrap()).unwrap();
    let entries = program_entries(&tree);
    assert_eq!(entries.len(), 1);
    for damage in [
        b"".to_vec(),
        b"not an entry".to_vec(),
        // A good header with a length field past the end of the file.
        {
            let mut bytes = std::fs::read(&entries[0]).unwrap();
            bytes.truncate(90);
            bytes
        },
        // A good entry with an artifact the decoder rejects. The
        // header is 88 bytes, so byte 88 is the artifact magic.
        {
            let mut bytes = std::fs::read(&entries[0]).unwrap();
            bytes[88] ^= 0xff;
            bytes
        },
    ] {
        std::fs::write(&entries[0], &damage).expect("writes");
        let report = tree.build("app").expect("the damaged entry must build");
        assert!(!report.program_cached, "the damaged entry hit");
        let again = std::fs::read(report.program.clone().unwrap()).unwrap();
        assert_eq!(good, again, "the rebuild changed the program");
    }
    // The last miss rewrote the entry, so the next build hits again.
    let last = tree.build("app").expect("builds");
    assert!(last.program_cached, "the rewritten entry missed");
}

/// Determinism: the program bytes are the same with a stage-2 hit and
/// with a stage-2 miss, and the same in a second build directory.
#[test]
fn the_program_bytes_are_the_same_with_and_without_a_hit() {
    let tree = TempTree::new("stage2-deterministic");
    workspace(&tree);
    let cold = build_package(&tree.path("app"), &tree.path("build")).expect("builds");
    let hot = build_package(&tree.path("app"), &tree.path("build")).expect("builds");
    let other = build_package(&tree.path("app"), &tree.path("build-b")).expect("builds");
    assert!(!cold.program_cached);
    assert!(hot.program_cached);
    assert!(!other.program_cached, "a fresh directory must link");
    let a = std::fs::read(cold.program.unwrap()).unwrap();
    let b = std::fs::read(hot.program.unwrap()).unwrap();
    let c = std::fs::read(other.program.unwrap()).unwrap();
    assert_eq!(a, b, "the hit changed the program bytes");
    assert_eq!(a, c, "the program bytes are not reproducible");
}

// ---------------------------------------------------------------
// Stage 3: the persistent verified-code store.
// ---------------------------------------------------------------

/// A second load of one artifact hits stage 3 and skips the verifier.
#[test]
fn a_second_load_hits_the_verified_store() {
    let tree = TempTree::new("stage3-hit");
    small_package(&tree);
    let report = build_package(&tree.path("solo"), &tree.path("solo/build")).expect("builds");
    let program = report.program.expect("the package builds a program");

    // The test names its own store, because the store of an artifact
    // now sits in the user cache directory.
    let store = VerifiedStore::new(&tree.path("store"));
    let mut first_runs = 0;
    let first = load_artifact_in(&store, &program, &mut first_runs).expect("loads");
    assert_eq!(first_runs, 1, "the first load skipped the verifier");
    let first_text = run_loaded(&first);

    let mut second_runs = 0;
    let second = load_artifact_in(&store, &program, &mut second_runs).expect("loads");
    assert_eq!(second_runs, 0, "the second load ran the verifier");
    assert_eq!(first_text, run_loaded(&second), "the results differ");
    // Both loads read the same declared core layout.
    assert_eq!(
        format!("{:?}", first.core_layout()),
        format!("{:?}", second.core_layout())
    );
}

/// Review regression: the store location never follows the path of
/// the artifact.
///
/// A verdict replaces a verifier run. An earlier rule walked up from
/// the artifact to a directory holding `lm.package`, so a supplier who
/// shipped a manifest beside the artifact owned the directory that
/// holds the verdict of that artifact. A forged record then admitted a
/// module the verifier rejects.
#[test]
fn a_shipped_manifest_never_moves_the_verdict_store() {
    let tree = TempTree::new("stage3-shipped");
    small_package(&tree);
    let report = build_package(&tree.path("solo"), &tree.path("solo/build")).expect("builds");
    let program = report.program.expect("the package builds a program");

    // The supplier ships a manifest beside the artifact.
    let vendor = tree.path("vendor");
    std::fs::create_dir_all(&vendor).expect("creates");
    std::fs::write(
        vendor.join("lm.package"),
        "[package]\nname = \"vendor\"\nversion = \"0.1.0\"\n",
    )
    .expect("writes");
    let shipped = vendor.join("thirdparty.lma");
    std::fs::copy(&program, &shipped).expect("copies");

    let chosen = VerifiedStore::for_artifact(&shipped);
    assert!(
        !chosen.dir().starts_with(&vendor),
        "the store landed inside the supplier's directory: {}",
        chosen.dir().display()
    );
    match lm_compiler::user_cache_dir() {
        Some(cache) => assert!(
            chosen.dir().starts_with(&cache),
            "the store left the user cache directory: {}",
            chosen.dir().display()
        ),
        None => assert!(
            !chosen.is_enabled(),
            "the store stayed enabled with no trusted directory"
        ),
    }
}

/// With no trusted directory, the store is disabled. It reads no
/// record and writes none, so every load meets the verifier. An
/// earlier rule fell back to `.lm-cache` in the current directory,
/// which an unpacked archive supplies.
#[test]
fn no_trusted_directory_disables_the_store() {
    let key = lm_vm::verified_key(&lm_testkit::compile_text("t.lm", "1\n").expect("compiles"));
    // The disabled shape is reachable whatever this host sets, so the
    // case asserts on every run.
    let disabled = VerifiedStore::disabled();
    assert!(!disabled.is_enabled());
    assert!(disabled.read(&key).is_none(), "a disabled store answered");
    assert!(
        disabled.write(&key, &Verdict).is_ok(),
        "a disabled write failed"
    );
    assert!(
        disabled.read(&key).is_none(),
        "a disabled store kept a record"
    );
    // The live rule still holds on this host.
    let chosen = VerifiedStore::for_artifact(Path::new("/nonexistent/x.lma"));
    assert_eq!(chosen.is_enabled(), lm_compiler::user_cache_dir().is_some());
}

/// Stage 3 stays reachable for an artifact with no package around it.
/// The store then sits beside the artifact.
#[test]
fn the_store_is_reachable_without_a_package() {
    let tree = TempTree::new("stage3-loose");
    small_package(&tree);
    let report = build_package(&tree.path("solo"), &tree.path("solo/build")).expect("builds");
    let built = report.program.expect("the package builds a program");
    // Copy the artifact to a directory with no `lm.package` above it.
    // The temporary root holds no manifest, so the walk finds none.
    let loose = tree.path("third-party/vendor.lma");
    std::fs::create_dir_all(loose.parent().unwrap()).expect("creates");
    std::fs::copy(&built, &loose).expect("copies");

    // The store of an artifact with no package sits in the user cache
    // directory, never beside the artifact: the supplier of a foreign
    // artifact must not write the verdict of that artifact.
    let chosen = VerifiedStore::for_artifact(&loose);
    assert!(
        lm_compiler::user_cache_dir()
            .map(|c| chosen.dir().starts_with(c))
            .unwrap_or(!chosen.is_enabled()),
        "the store of a loose artifact left the user cache directory: {}",
        chosen.dir().display()
    );
    assert!(
        !chosen.dir().starts_with(tree.path("third-party")),
        "the store landed beside the artifact"
    );
    // The store still works. The test names its own directory so it
    // writes nothing into the real user cache.
    let store = VerifiedStore::new(&tree.path("cache"));
    let mut first_runs = 0;
    load_artifact_in(&store, &loose, &mut first_runs).expect("loads");
    assert_eq!(first_runs, 1, "the first load skipped the verifier");
    assert!(store.dir().is_dir(), "the store wrote no record");
    let mut second_runs = 0;
    load_artifact_in(&store, &loose, &mut second_runs).expect("loads");
    assert_eq!(second_runs, 0, "the second load ran the verifier");
}

/// A damaged stage-3 record is a miss. The load still succeeds and
/// the verifier runs again.
#[test]
fn a_damaged_verified_record_is_a_miss() {
    let tree = TempTree::new("stage3-damaged");
    small_package(&tree);
    let report = build_package(&tree.path("solo"), &tree.path("solo/build")).expect("builds");
    let program = report.program.expect("the package builds a program");
    let store = VerifiedStore::new(&tree.path("store"));
    let mut runs = 0;
    load_artifact_in(&store, &program, &mut runs).expect("loads");
    let dir = store.dir().to_path_buf();
    let record: PathBuf = std::fs::read_dir(&dir)
        .expect("the store exists")
        .map(|e| e.expect("entry").path())
        .next()
        .expect("one record");
    let good = std::fs::read(&record).expect("reads");
    for damage in [
        b"".to_vec(),
        b"not a record".to_vec(),
        // A truncated record: the counts outrun the file.
        good[..good.len() - 8].to_vec(),
        // A bad magic byte.
        {
            let mut bytes = good.clone();
            bytes[0] ^= 0xff;
            bytes
        },
        // A bad presence tag in the core slots.
        {
            let mut bytes = good.clone();
            let last = bytes.len() - 5;
            bytes[last] = 7;
            bytes
        },
        // A record with a huge class count and no bytes behind it.
        // The count sits at offset 60, after the magic, the version,
        // and the three key fields.
        {
            let mut bytes = good[..60].to_vec();
            bytes.extend_from_slice(&u32::MAX.to_le_bytes());
            bytes
        },
    ] {
        std::fs::write(&record, &damage).expect("writes");
        let mut runs = 0;
        let loaded =
            load_artifact_in(&store, &program, &mut runs).expect("the damaged record must load");
        assert_eq!(runs, 1, "the damaged record hit the store");
        run_loaded(&loaded);
    }
}

/// A record filed under a key that does not belong to the artifact
/// never admits the module. `load_with_record` recomputes the key
/// from the decoded content and rejects it, and the store read
/// rejects it as well.
///
/// The key is the whole binding. `load_with_record` reads no other
/// proof from the record, so the store must compare the full key
/// before a record leaves it. The file name holds 32 bytes of the
/// key, and `VerifiedStore::read` compares all three fields.
#[test]
fn a_wrong_key_never_admits_the_module() {
    let tree = TempTree::new("stage3-wrong-key");
    small_package(&tree);
    tree.write(
        "other/lm.package",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "other/src/main.lm",
        "def thrice(n: Int): Int\n\x20 n * 3\nend\n\
         \n\
         def once(n: Int): Int\n\x20 n\nend\n\
         \n\
         thrice(once(14))\n",
    );
    let solo = build_package(&tree.path("solo"), &tree.path("solo/build"))
        .expect("builds")
        .program
        .expect("a program");
    let other = build_package(&tree.path("other"), &tree.path("other/build"))
        .expect("builds")
        .program
        .expect("a program");

    let solo_module = lm_bytecode::decode(&std::fs::read(&solo).unwrap()).expect("decodes");
    let other_module = lm_bytecode::decode(&std::fs::read(&other).unwrap()).expect("decodes");
    let solo_key = lm_vm::verified_key(&solo_module);
    let other_key = lm_vm::verified_key(&other_module);
    assert_ne!(solo_key, other_key, "the two programs share a key");

    // A verdict under a wrong key is rejected: the key comes from the
    // decoded module, never from the caller. The verdict itself
    // carries no fact, so a wrong verdict can supply nothing.
    lm_vm::load(other_module).expect("the other program loads");
    lm_vm::load(solo_module.clone()).expect("the program loads");
    assert!(
        lm_vm::load_with_record(solo_module.clone(), &other_key, &lm_vm::VerifiedRecord).is_err(),
        "a wrong key admitted the module"
    );
    assert!(
        lm_vm::load_with_record(solo_module, &solo_key, &lm_vm::VerifiedRecord).is_ok(),
        "the right key rejected the module"
    );

    // The production path: file the other program's record under the
    // name of this one. The store read compares the whole key, so the
    // entry is a miss and the verifier runs.
    let store = VerifiedStore::for_artifact(&solo);
    store
        .write(&other_key, &to_verdict(&lm_vm::VerifiedRecord))
        .expect("writes");
    let wrong = store.record_path(&other_key);
    let target = store.record_path(&solo_key);
    std::fs::rename(&wrong, &target).expect("renames");
    let mut runs = 0;
    let loaded = load_artifact(&solo, &mut runs).expect("the wrong record must load");
    assert_eq!(runs, 1, "the wrong record hit the store");
    run_loaded(&loaded);
}

/// A rejected artifact writes no record.
#[test]
fn a_rejected_artifact_writes_no_record() {
    let tree = TempTree::new("stage3-rejected");
    small_package(&tree);
    let program = build_package(&tree.path("solo"), &tree.path("solo/build"))
        .expect("builds")
        .program
        .expect("a program");
    // Move the artifact out of the package, so a written record would
    // land in a fresh directory the test can look at.
    let loose = tree.path("loose/broken.lma");
    std::fs::create_dir_all(loose.parent().unwrap()).expect("creates");
    let good = std::fs::read(&program).expect("reads");
    // Find one damaged byte the decoder accepts and the verifier
    // rejects. The search uses the plain loader, so it writes no
    // record and leaves the store empty.
    let mut broken = None;
    for at in good.len() / 2..good.len() {
        let mut bytes = good.clone();
        bytes[at] ^= 0xff;
        if lm_vm::load_bytes(&bytes).is_err() {
            broken = Some(bytes);
            break;
        }
    }
    let broken = broken.expect("no damaged byte reached the loader");
    std::fs::write(&loose, &broken).expect("writes");
    let mut runs = 0;
    load_artifact(&loose, &mut runs).expect_err("the damaged artifact loaded");
    let dir = tree.path("loose/build/cache/verified");
    let count = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    assert_eq!(count, 0, "a rejected artifact wrote a record");
}
