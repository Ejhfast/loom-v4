//! Week-6 packages, modules, and the build loop.
//!
//! Every case builds a real package tree in a temporary directory,
//! so the tests exercise the same path the `lm` tool uses.

use lm_compiler::{build_package, BuildReport};
use lm_testkit::repo_root;
use lm_vm::{RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
            std::env::temp_dir().join(format!("lm-week6-{label}-{}-{unique}", std::process::id()));
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
         \x20 \"{m.rows}x{m.cols}\"\n\
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
         \x20 \"Hello {name}!\"\n\
         end\n\
         \n\
         def report(m: matrix.Matrix): String\n\
         \x20 \"{matrix.describe(m)} has {m.area()} cells\"\n\
         end\n",
    );
    tree.write(
        "app/src/main.lm",
        "use sys.io.print\n\
         use greeting\n\
         use mathlib.matrix\n\
         \n\
         def run() with Io.Print\n\
         \x20 m = matrix.Matrix(2, 3)\n\
         \x20 line = greeting.greet(\"Ada\")\n\
         \x20 print(\"{line}\\n\")\n\
         \x20 print(\"{greeting.report(m)}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
}

/// Run one linked artifact with the recording host and the given
/// grants. Returns the printed output.
fn run_artifact(path: &Path, allow: &[&str]) -> String {
    let bytes = std::fs::read(path).expect("the artifact reads");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host.clone()));
    for grant in allow {
        world.allow(grant).expect("the grant applies");
    }
    let outcome = world.run_root();
    assert!(
        matches!(outcome, lm_vm::Outcome::Done(_)),
        "the program faulted: {}",
        world.show_outcome(&outcome)
    );
    let printed = host.borrow().printed.join("");
    printed
}

fn report_of<'a>(report: &'a BuildReport, path: &str) -> &'a lm_compiler::ModuleReport {
    report
        .modules
        .iter()
        .find(|m| m.path == path)
        .unwrap_or_else(|| panic!("no module report for `{path}`"))
}

// ---------------------------------------------------------------
// The build loop.
// ---------------------------------------------------------------

/// The two-package workspace builds, links, and runs.
#[test]
fn a_two_package_workspace_builds_and_runs() {
    let tree = TempTree::new("workspace");
    workspace(&tree);
    let report = tree.build("app").expect("the workspace builds");
    assert_eq!(report.root, "app");
    assert_eq!(report.modules.len(), 3);
    assert_eq!(report.compiled(), 3, "the first build compiles everything");
    let program = report.program.clone().expect("the app builds a program");
    let output = run_artifact(&program, &["Io.Print"]);
    assert_eq!(output, "Hello Ada!\n2x3 has 6 cells\n");
}

/// A second build with unchanged inputs reports a cache hit for every
/// module and produces the same program bytes.
#[test]
fn a_second_build_hits_the_cache() {
    let tree = TempTree::new("cache");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    let first_bytes = std::fs::read(first.program.clone().unwrap()).unwrap();
    let second = tree.build("app").expect("builds");
    assert_eq!(second.compiled(), 0, "the second build ran the compiler");
    let second_bytes = std::fs::read(second.program.clone().unwrap()).unwrap();
    assert_eq!(first_bytes, second_bytes, "the program bytes moved");
    assert_eq!(first.program_semantic, second.program_semantic);
}

/// The rebuild gate: an edit to an exported body rebuilds the edited
/// module and keeps every dependent cached, because no interface
/// hash moved.
#[test]
fn a_body_edit_rebuilds_only_the_edited_package() {
    let tree = TempTree::new("body-edit");
    workspace(&tree);
    tree.build("app").expect("builds");
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("\"{m.rows}x{m.cols}\"", "\"{m.rows} by {m.cols}\""),
    );
    let report = tree.build("app").expect("builds");
    assert!(!report_of(&report, "mathlib.matrix").cached);
    assert!(
        report_of(&report, "app.greeting").cached,
        "the dependent recompiled although no interface moved"
    );
    assert!(report_of(&report, "app.main").cached);
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "Hello Ada!\n2 by 3 has 6 cells\n");
}

/// An edit to an exported signature moves the interface hash, so
/// every dependent module recompiles.
#[test]
fn a_signature_edit_rebuilds_the_dependents() {
    let tree = TempTree::new("sig-edit");
    workspace(&tree);
    tree.build("app").expect("builds");
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("def area(self): Int", "def area(self, scale: Int): Int")
            .replace("self.rows * self.cols", "self.rows * self.cols * scale"),
    );
    tree.write(
        "app/src/greeting.lm",
        &std::fs::read_to_string(tree.path("app/src/greeting.lm"))
            .unwrap()
            .replace("m.area()", "m.area(1)"),
    );
    let report = tree.build("app").expect("builds");
    assert!(!report_of(&report, "mathlib.matrix").cached);
    assert!(
        !report_of(&report, "app.greeting").cached,
        "the dependent kept a stale interface"
    );
}

/// An interface change that the dependent does not follow is a
/// compile error in the dependent, never a silent mismatch.
#[test]
fn a_stale_caller_fails_to_compile() {
    let tree = TempTree::new("stale");
    workspace(&tree);
    tree.build("app").expect("builds");
    tree.write(
        "mathlib/src/matrix.lm",
        &std::fs::read_to_string(tree.path("mathlib/src/matrix.lm"))
            .unwrap()
            .replace("def describe(m: Matrix)", "def describe(m: Matrix, n: Int)"),
    );
    let error = tree.build("app").expect_err("the build must fail");
    assert!(error.contains("describe"), "{error}");
}

/// The linked program is a closed artifact: it carries no import slot
/// and runs with no source and no dependency present.
#[test]
fn the_program_artifact_is_closed() {
    let tree = TempTree::new("closed");
    workspace(&tree);
    let report = tree.build("app").expect("builds");
    let bytes = std::fs::read(report.program.clone().unwrap()).unwrap();
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    assert!(module.imports.is_empty(), "the program has import slots");
    // Copy the artifact away from the sources and run it there.
    let alone = TempTree::new("alone");
    alone.write("app.lma", "");
    std::fs::write(alone.path("app.lma"), &bytes).unwrap();
    let output = run_artifact(&alone.path("app.lma"), &["Io.Print"]);
    assert_eq!(output, "Hello Ada!\n2x3 has 6 cells\n");
}

/// The core classes of every module become one core in the linked
/// program, so a core value keeps its class across a module boundary.
#[test]
fn the_linked_program_shares_one_core() {
    let tree = TempTree::new("core-share");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/find.lm",
        "def first(xs: [Int]): Option[Int]\n  xs.get(0)\nend\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "prog/src/main.lm",
        "use sys.io.print\n\
         use lib.find\n\
         \n\
         def show(o: Option[Int]): String\n\
         \x20 case o\n\
         \x20 in Some(v) then \"got {v}\"\n\
         \x20 in None then \"none\"\n\
         \x20 end\n\
         end\n\
         \n\
         def run() with Io.Print\n\
         \x20 line = show(find.first([7, 8]))\n\
         \x20 print(\"{line}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("prog").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(
        output, "got 7\n",
        "an Option built in one module did not match in another"
    );
}

/// A `use` grants no authority: an imported function that performs
/// still charges the row on its caller and still needs the grant.
#[test]
fn an_import_grants_no_authority() {
    let tree = TempTree::new("authority");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/say.lm",
        "use sys.io.print\n\
         \n\
         def shout(text: String) with Io.Print\n\
         \x20 print(\"{text}\\n\")\n\
         end\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    // The caller does not declare the row of the imported function.
    tree.write(
        "prog/src/main.lm",
        "use lib.say\n\ndef run()\n  say.shout(\"hi\")\nend\n\nrun()\n",
    );
    let error = tree.build("prog").expect_err("the row must be charged");
    assert!(error.contains("E1046"), "{error}");
    // With the row declared the program builds, and it still needs the
    // policy grant at run time.
    tree.write(
        "prog/src/main.lm",
        "use lib.say\n\ndef run() with Io.Print\n  say.shout(\"hi\")\nend\n\nrun()\n",
    );
    let report = tree.build("prog").expect("builds");
    let bytes = std::fs::read(report.program.clone().unwrap()).unwrap();
    let loaded = lm_vm::load_bytes(&bytes).expect("loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    let outcome = world.run_root();
    assert_eq!(
        world.show_outcome(&outcome),
        "Fault(PolicyDenied)",
        "an import granted authority"
    );
}

// ---------------------------------------------------------------
// The package layout and the manifest.
// ---------------------------------------------------------------

/// `lm new` scaffolds a package that builds and runs.
#[test]
fn the_scaffold_builds_and_runs() {
    let tree = TempTree::new("scaffold");
    lm_compiler::scaffold::new_package(&tree.path("hello"), "hello").expect("scaffolds");
    let manifest = std::fs::read_to_string(tree.path("hello/lm.package")).unwrap();
    assert_eq!(
        manifest,
        "[package]\nname = \"hello\"\nversion = \"0.1.0\"\n"
    );
    let report = tree.build("hello").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "Hello world!\n");
}

/// A build works from any directory inside the package.
#[test]
fn a_build_starts_from_any_directory_inside_the_package() {
    let tree = TempTree::new("from-inside");
    workspace(&tree);
    let report =
        build_package(&tree.path("app/src"), &tree.path("build")).expect("builds from src");
    assert_eq!(report.root, "app");
}

/// A dependency key that collides with an own module name is a build
/// error, and the message names the manifest rename as the fix.
#[test]
fn a_dependency_name_collision_names_the_rename() {
    let tree = TempTree::new("collision");
    workspace(&tree);
    // The app gains a module named like the dependency key.
    tree.write("app/src/mathlib.lm", "def unused(): Int\n  1\nend\n");
    let error = tree.build("app").expect_err("the collision must reject");
    assert!(error.contains("rename the dependency key"), "{error}");
}

/// A `use` of an unknown root names the roots that exist.
#[test]
fn an_unknown_root_lists_the_roots() {
    let tree = TempTree::new("unknown-root");
    workspace(&tree);
    tree.write(
        "app/src/main.lm",
        "use nowhere.thing\n\ndef run(): Int\n  1\nend\n\nrun()\n",
    );
    let error = tree.build("app").expect_err("the root must reject");
    assert!(error.contains("is not a root name"), "{error}");
    assert!(error.contains("mathlib"), "{error}");
}

/// Only `src/main.lm` holds the program entry.
#[test]
fn a_library_module_carries_no_entry_expression() {
    let tree = TempTree::new("entry");
    workspace(&tree);
    tree.write(
        "mathlib/src/matrix.lm",
        "def area(): Int\n  6\nend\n\narea()\n",
    );
    let error = tree.build("app").expect_err("the entry must reject");
    assert!(error.contains("E1053"), "{error}");
    assert!(error.contains("src/main.lm"), "{error}");
}

/// A package without `src/main.lm` is a library: it builds every
/// module and produces no program.
#[test]
fn a_library_package_builds_no_program() {
    let tree = TempTree::new("library");
    workspace(&tree);
    let report = tree.build("mathlib").expect("builds");
    assert!(report.program.is_none());
    assert_eq!(report.modules.len(), 1);
}

/// The manifest rejects everything outside the accepted subset.
#[test]
fn the_manifest_subset_is_strict() {
    let tree = TempTree::new("manifest");
    tree.write(
        "pkg/lm.package",
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\nauthors = [\"a\"]\n",
    );
    tree.write("pkg/src/main.lm", "1\n");
    let error = tree.build("pkg").expect_err("the key must reject");
    assert!(error.contains("authors"), "{error}");
    assert!(error.contains("`[package]` key"), "{error}");
}

// ---------------------------------------------------------------
// The example workspace in the repository.
// ---------------------------------------------------------------

/// The checked-in example builds and prints the documented output.
#[test]
fn the_example_workspace_runs() {
    let out = TempTree::new("example");
    let report = build_package(
        &repo_root().join("examples/05-modules/app"),
        &out.path("build"),
    )
    .expect("the example builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "Hello Ada!\n2x3 has 6 cells\n");
}
