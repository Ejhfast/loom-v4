//! The artifact build store.
//!
//! The store keys an artifact on its exact module inputs.
//! A rebuild with no source change reuses its bytes.
//!
//! Every case builds a real package tree in a temporary directory, so
//! the tests exercise the same path the `lm` tool uses.

use lm_compiler::{build_package, BuildReport};
use std::path::PathBuf;

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

/// The stage-2 entry files of one build directory.
fn artifact_entries(tree: &TempTree) -> Vec<PathBuf> {
    let dir = tree.path("build/cache/artifacts");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("no stage-2 directory `{}`: {e}", dir.display()))
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    entries
}

// ---------------------------------------------------------------
// The artifact cache.
// ---------------------------------------------------------------

/// A second build of one package reuses the stage-2 artifact bytes.
#[test]
fn a_second_build_hits_the_artifact_cache() {
    let tree = TempTree::new("stage2-hit");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    assert!(
        !first.artifact_cached,
        "the first build reused artifact bytes"
    );
    assert_eq!(artifact_entries(&tree).len(), 1, "one stage-2 entry");
    let second = tree.build("app").expect("builds");
    assert!(
        second.artifact_cached,
        "the second build missed the artifact bytes"
    );
    assert_eq!(second.compiled(), 0, "the second build compiled a module");
    // A hit reports both identities and writes the artifact.
    assert_eq!(first.artifact_id, second.artifact_id);
    assert_eq!(first.container_hash, second.container_hash);
    let a = std::fs::read(first.artifact.clone().unwrap()).unwrap();
    let b = std::fs::read(second.artifact.clone().unwrap()).unwrap();
    assert_eq!(a, b, "the cached artifact bytes moved");
    assert_eq!(artifact_entries(&tree).len(), 1, "the hit wrote an entry");
}

/// A source edit moves the root artifact identity.
#[test]
fn a_source_edit_misses_the_artifact_cache() {
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
    assert!(!second.artifact_cached, "the edit hit the artifact cache");
    assert_ne!(
        first.container_hash, second.container_hash,
        "the edit did not change the artifact"
    );
    assert_eq!(artifact_entries(&tree).len(), 2, "the edit kept one entry");
    // The former artifact identity still names the former bytes.
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("\"#{m.rows} by #{m.cols}\"", "\"#{m.rows}x#{m.cols}\""),
    );
    let third = tree.build("app").expect("builds");
    assert!(third.artifact_cached, "the restored set missed");
    assert_eq!(first.container_hash, third.container_hash);
}

/// A damaged artifact entry causes a fresh encoding.
#[test]
fn a_damaged_artifact_entry_is_a_miss() {
    let tree = TempTree::new("stage2-damaged");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    let good = std::fs::read(first.artifact.clone().unwrap()).unwrap();
    let entries = artifact_entries(&tree);
    assert_eq!(entries.len(), 1);
    for damage in [
        b"".to_vec(),
        b"not an entry".to_vec(),
        // A truncated artifact.
        {
            let mut bytes = std::fs::read(&entries[0]).unwrap();
            bytes.truncate(bytes.len() / 2);
            bytes
        },
        // An artifact with damaged magic.
        {
            let mut bytes = std::fs::read(&entries[0]).unwrap();
            bytes[0] ^= 0xff;
            bytes
        },
    ] {
        std::fs::write(&entries[0], &damage).expect("writes");
        let report = tree.build("app").expect("the damaged entry must build");
        assert!(!report.artifact_cached, "the damaged entry hit");
        let again = std::fs::read(report.artifact.clone().unwrap()).unwrap();
        assert_eq!(good, again, "the rebuild changed the artifact");
    }
    // The last miss rewrote the entry, so the next build hits again.
    let last = tree.build("app").expect("builds");
    assert!(last.artifact_cached, "the rewritten entry missed");
}

/// Determinism: the artifact bytes are the same with a stage-2 hit and
/// with a stage-2 miss, and the same in a second build directory.
#[test]
fn artifact_bytes_are_stable_with_and_without_a_hit() {
    let tree = TempTree::new("stage2-deterministic");
    workspace(&tree);
    let cold = build_package(&tree.path("app"), &tree.path("build")).expect("builds");
    let hot = build_package(&tree.path("app"), &tree.path("build")).expect("builds");
    let other = build_package(&tree.path("app"), &tree.path("build-b")).expect("builds");
    assert!(!cold.artifact_cached);
    assert!(hot.artifact_cached);
    assert!(!other.artifact_cached, "a fresh directory must encode");
    let a = std::fs::read(cold.artifact.unwrap()).unwrap();
    let b = std::fs::read(hot.artifact.unwrap()).unwrap();
    let c = std::fs::read(other.artifact.unwrap()).unwrap();
    assert_eq!(a, b, "the hit changed the artifact bytes");
    assert_eq!(a, c, "the artifact bytes are not reproducible");
}
