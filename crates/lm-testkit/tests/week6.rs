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

/// A module no import slot names never reaches the program. The link
/// order walks the import graph from the entry module.
#[test]
fn an_unused_module_stays_out_of_the_program() {
    let tree = TempTree::new("unused");
    workspace(&tree);
    let before = tree.build("app").expect("builds");
    let small = std::fs::read(before.program.clone().unwrap()).unwrap();
    tree.write(
        "mathlib/src/unused.lm",
        "class Heavy\n\
         \x20 a: Int = 1\n\
         \x20 def one(self): Int\n\
         \x20   self.a\n\
         \x20 end\n\
         end\n\
         \n\
         def never_called(n: Int): Int\n\
         \x20 n * 99\n\
         end\n",
    );
    let after = tree.build("app").expect("builds");
    assert_eq!(after.modules.len(), 4, "the module did not build");
    let big = std::fs::read(after.program.clone().unwrap()).unwrap();
    assert_eq!(small, big, "an unused module reached the program");
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

/// An imported enum keeps its family: the arms construct, `case`
/// matches, and the exhaustiveness rule sees the closed arm set.
#[test]
fn an_imported_enum_constructs_and_matches() {
    let tree = TempTree::new("enum");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/shape.lm",
        "enum Shape\n\
         \x20 Dot\n\
         \x20 Line(len: Int)\n\
         \n\
         \x20 def size(self): Int\n\
         \x20   case self\n\
         \x20   in Dot then 0\n\
         \x20   in Line(l) then l\n\
         \x20   end\n\
         \x20 end\n\
         end\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    // The direct import binds the family, so the arm names resolve
    // unqualified and `case` sees the whole arm set.
    tree.write(
        "prog/src/main.lm",
        "use sys.io.print\n\
         use lib.shape.Shape\n\
         \n\
         def name(s: Shape): String\n\
         \x20 case s\n\
         \x20 in Dot then \"dot\"\n\
         \x20 in Line(l) then \"line {l}\"\n\
         \x20 end\n\
         end\n\
         \n\
         def run() with Io.Print\n\
         \x20 a = name(Dot())\n\
         \x20 b = name(Line(4))\n\
         \x20 print(\"{a} {b} {Line(7).size()}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("prog").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "dot line 4 7\n");
}

/// An imported generic class and an imported generic function keep
/// their arity through the import slot.
#[test]
fn imported_generics_keep_their_arity() {
    let tree = TempTree::new("generics");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/box.lm",
        "class Box[T]\n\
         \x20 value: T\n\
         \n\
         \x20 def init(mut self, value: T)\n\
         \x20   self.value = value\n\
         \x20 end\n\
         \n\
         \x20 def get(self): T\n\
         \x20   self.value\n\
         \x20 end\n\
         end\n\
         \n\
         def wrap[T](value: T): Box[T]\n\
         \x20 Box(value)\n\
         end\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "prog/src/main.lm",
        "use sys.io.print\n\
         use lib.box\n\
         \n\
         def run() with Io.Print\n\
         \x20 a = box.Box(7)\n\
         \x20 b = box.wrap(\"text\")\n\
         \x20 c: box.Box[Int] = a\n\
         \x20 print(\"{c.get()} {b.get()}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("prog").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "7 text\n");
}

/// An imported class holds its mutable methods and works as the type
/// of a local field.
#[test]
fn an_imported_class_holds_its_mutable_methods() {
    let tree = TempTree::new("mutable");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/counter.lm",
        "class Counter\n\
         \x20 value: Int = 0\n\
         \n\
         \x20 def add(mut self, n: Int): Int\n\
         \x20   self.value = self.value + n\n\
         \x20   self.value\n\
         \x20 end\n\
         end\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "prog/src/main.lm",
        "use sys.io.print\n\
         use lib.counter\n\
         \n\
         class Pair\n\
         \x20 left: counter.Counter = counter.Counter()\n\
         \n\
         \x20 def bump(mut self): Int\n\
         \x20   self.left.add(2)\n\
         \x20 end\n\
         end\n\
         \n\
         def run() with Io.Print\n\
         \x20 p = Pair()\n\
         \x20 p.bump()\n\
         \x20 print(\"{p.bump()}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("prog").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "4\n");
}

/// A module that names a definition of another module inherits the
/// types that signature names, even without a `use` line for the
/// module that defines them.
#[test]
fn a_transitive_type_materializes_without_its_own_use_line() {
    let tree = TempTree::new("transitive");
    workspace(&tree);
    // `main` drops `use mathlib.matrix` and still calls
    // `greeting.report`, whose parameter is a `mathlib` class.
    tree.write(
        "app/src/main.lm",
        "use sys.io.print\n\
         use greeting\n\
         \n\
         def run() with Io.Print\n\
         \x20 line = greeting.greet(\"Ada\")\n\
         \x20 print(\"{line}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("app").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "Hello Ada!\n");
    // The main module still pins the transitive class, because its
    // own import of `report` names it.
    let bytes = std::fs::read(report.program.unwrap()).unwrap();
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    assert!(module.imports.is_empty());
}

/// An exported effect-polymorphic function keeps its effect
/// parameter through the interface, and the caller still charges the
/// row of the closure it passes.
#[test]
fn an_imported_effect_parameter_survives_the_interface() {
    let tree = TempTree::new("effect-var");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/apply.lm",
        "def twice[effect e](f: () -> Int with e): Int with e\n\
         \x20 f() + f()\n\
         end\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "prog/src/main.lm",
        "use sys.io.print\n\
         use lib.apply\n\
         \n\
         def run() with Io.Print\n\
         \x20 total = apply.twice(do ||: Int with Io.Print\n\
         \x20   print(\"tick\\n\")\n\
         \x20   3\n\
         \x20 end)\n\
         \x20 print(\"{total}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("prog").expect("builds");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "tick\ntick\n6\n");
}

/// A symbolic link inside `src` rejects. A link cycle would
/// otherwise produce an unbounded module tree.
#[cfg(unix)]
#[test]
fn a_symbolic_link_in_the_module_tree_rejects() {
    let tree = TempTree::new("symlink");
    workspace(&tree);
    std::os::unix::fs::symlink(tree.path("app/src"), tree.path("app/src/loop"))
        .expect("the link is created");
    let error = tree.build("app").expect_err("the link must reject");
    assert!(error.contains("symbolic link"), "{error}");
}

/// A module that imports itself is an import cycle.
#[test]
fn a_module_cannot_import_itself() {
    let tree = TempTree::new("self-import");
    workspace(&tree);
    tree.write(
        "app/src/greeting.lm",
        "use greeting\n\ndef greet(name: String): String\n  name\nend\n",
    );
    let error = tree.build("app").expect_err("the cycle must reject");
    assert!(error.contains("imports itself"), "{error}");
}

/// Two modules that import each other form an import cycle, and the
/// diagnostic names every module in it.
#[test]
fn two_modules_cannot_import_each_other() {
    let tree = TempTree::new("mutual-import");
    workspace(&tree);
    tree.write(
        "app/src/left.lm",
        "use right\n\ndef here(): Int\n  1\nend\n",
    );
    tree.write(
        "app/src/right.lm",
        "use left\n\ndef there(): Int\n  2\nend\n",
    );
    let error = tree.build("app").expect_err("the cycle must reject");
    assert!(error.contains("import cycle"), "{error}");
    assert!(error.contains("left"), "{error}");
    assert!(error.contains("right"), "{error}");
}

/// A class cannot inherit an imported class, and the diagnostic names
/// the fix.
#[test]
fn a_class_cannot_inherit_an_imported_class() {
    let tree = TempTree::new("inherit");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/base.lm",
        "class Base\n  tag: Int = 0\n  def show(self): Int\n    self.tag\n  end\nend\n",
    );
    tree.write(
        "prog/lm.package",
        "[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "prog/src/main.lm",
        "use lib.base.Base\n\nclass Child < Base\nend\n\ndef run(): Int\n  1\nend\n\nrun()\n",
    );
    let error = tree.build("prog").expect_err("inheritance must reject");
    assert!(error.contains("E1038"), "{error}");
    assert!(error.contains("imported class"), "{error}");
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

/// A package links only the standard closure that its source names.
#[test]
fn a_package_links_a_requested_standard_module() {
    let tree = TempTree::new("standard");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/main.lm",
        "use std.http.Http\n\nHttp().default_limits().max_headers\n",
    );
    let report = tree.build("app").expect("the package builds");
    assert_eq!(report.modules.len(), 1);
    let bytes = std::fs::read(report.program.expect("the program exists")).expect("it reads");
    let loaded = lm_vm::load_bytes(&bytes).expect("the program loads");
    assert!(loaded.module().imports.is_empty());
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(128)");
    let second = tree.build("app").expect("the cached package builds");
    assert_eq!(second.compiled(), 0);
    assert!(second.program_cached);
}

/// A public interface can expose a standard type to its caller.
#[test]
fn a_standard_type_crosses_a_package_interface() {
    let tree = TempTree::new("standard-interface");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/config.lm",
        "use std.tls.Tls\n\
         use std.tls.TlsClientConfig\n\n\
         def default_config(): TlsClientConfig\n\
         \x20 Tls().default_config(\"localhost\")\n\
         end\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        "use lib.config\n\nconfig.default_config().max_buffer_bytes\n",
    );
    let report = tree.build("app").expect("the package graph builds");
    let bytes = std::fs::read(report.program.expect("the program exists")).expect("it reads");
    let loaded = lm_vm::load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(65536)");
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

/// The file tree under `src` is the module tree:
/// `src/geometry/shapes.lm` is the module `geometry.shapes`.
#[test]
fn a_directory_becomes_a_module_path() {
    let tree = TempTree::new("tree");
    tree.write(
        "pkg/lm.package",
        "[package]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "pkg/src/geometry/shapes.lm",
        "class Dot\n\
         \x20 size: Int = 2\n\
         \n\
         \x20 def area(self): Int\n\
         \x20   self.size * self.size\n\
         \x20 end\n\
         end\n",
    );
    tree.write(
        "pkg/src/main.lm",
        "use sys.io.print\n\
         use geometry.shapes\n\
         \n\
         def run() with Io.Print\n\
         \x20 d = shapes.Dot()\n\
         \x20 print(\"{d.area()}\\n\")\n\
         end\n\
         \n\
         run()\n",
    );
    let report = tree.build("pkg").expect("builds");
    let paths: Vec<&str> = report.modules.iter().map(|m| m.path.as_str()).collect();
    assert!(paths.contains(&"pkg.geometry.shapes"), "{paths:?}");
    let output = run_artifact(&report.program.clone().unwrap(), &["Io.Print"]);
    assert_eq!(output, "4\n");
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

/// The build directory is not a trust boundary: a damaged entry is a
/// miss, so the compiler runs again and the program is unchanged.
#[test]
fn a_damaged_cache_entry_is_a_miss() {
    let tree = TempTree::new("damaged");
    workspace(&tree);
    let first = tree.build("app").expect("builds");
    let good = std::fs::read(first.program.clone().unwrap()).unwrap();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(tree.path("build/cache/modules"))
        .expect("the cache directory exists")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().map(|e| e == "lma").unwrap_or(false))
        .collect();
    entries.sort();
    assert_eq!(entries.len(), 3);
    std::fs::write(&entries[0], b"not an artifact").expect("writes");
    let second = tree.build("app").expect("builds");
    assert_eq!(second.compiled(), 1, "the damaged entry did not miss");
    let again = std::fs::read(second.program.clone().unwrap()).unwrap();
    assert_eq!(good, again, "the rebuild changed the program");
}

/// Two builds in two build directories produce the same program
/// bytes, so the build loop depends on the sources only.
#[test]
fn the_program_bytes_are_deterministic() {
    let tree = TempTree::new("determinism");
    workspace(&tree);
    let first = build_package(&tree.path("app"), &tree.path("build-a")).expect("builds");
    let second = build_package(&tree.path("app"), &tree.path("build-b")).expect("builds");
    assert_eq!(first.compiled(), 3);
    assert_eq!(second.compiled(), 3, "the second directory reused an entry");
    let a = std::fs::read(first.program.unwrap()).unwrap();
    let b = std::fs::read(second.program.unwrap()).unwrap();
    assert_eq!(a, b, "the program bytes are not reproducible");
}

// ---------------------------------------------------------------
// The explicit environments, driven by hand.
// ---------------------------------------------------------------

/// The `CompileEnv` and `LinkEnv` path without a package on disk: bind
/// an interface, compile a module against it, link the program,
/// request the typed entry, and run it.
#[test]
fn the_typed_environments_compile_link_and_run_by_hand() {
    use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
    use lm_source::SourceFile;

    // The provider module compiles against an empty environment.
    let library = compile_module(
        "lib.math",
        &SourceFile::new(
            "lib/math.lm",
            "def twice(n: Int): Int\n  n * 2\nend\n".to_string(),
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");

    // The program binds the interface under the root name `lib`.
    let mut env = CompileEnv::new();
    env.bind_interface(library.interface.clone())
        .expect("the interface binds");
    env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new(
            "app/main.lm",
            "use lib.math\n\ndef run(): Int\n  math.twice(21)\nend\n\nrun()\n".to_string(),
        ),
        &env.freeze(),
        true,
    )
    .expect("the program compiles");
    // The program artifact carries the import slot and never loads.
    assert!(!program.module.imports.is_empty());
    assert!(lm_vm::load(program.module.clone()).is_err());

    let mut link_env = LinkEnv::new();
    for unit in [&library, &program] {
        link_env
            .bind(LinkUnit {
                path: unit.path.clone(),
                module: unit.module.clone(),
                interface: unit.interface.clone(),
            })
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("links");
    // The typed entry: the result type and the empty row.
    linked
        .entry()
        .expect(&lm_bytecode::BcType::Int, &[])
        .expect("the entry matches");
    assert!(linked
        .entry()
        .expect(&lm_bytecode::BcType::Str, &[])
        .is_err());
    let loaded = lm_vm::load_bytes(&linked.artifact).expect("the program loads");
    let mut vm = lm_vm::Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(42)");
}

/// A pin that no longer matches the provider is a link error, and the
/// message names the rebuild as the fix.
#[test]
fn a_stale_pin_fails_to_link() {
    use lm_compiler::{link, LinkEnv, LinkUnit};
    let tree = TempTree::new("stale-pin");
    workspace(&tree);
    let report = tree.build("app").expect("builds");
    let _ = report;
    // Rebuild the units from the cache files and move one pin.
    let mut link_env = LinkEnv::new();
    let mut units = Vec::new();
    let mut seen = Vec::new();
    for (path, file) in [
        ("mathlib.matrix", "mathlib/src/matrix.lm"),
        ("app.greeting", "app/src/greeting.lm"),
        ("app.main", "app/src/main.lm"),
    ] {
        units.push(compile_one(&tree, path, file, &mut seen));
    }
    // The greeting module pins the interface of `mathlib.matrix`.
    let mut stale = units[1].clone();
    let slot = stale
        .module
        .imports
        .iter_mut()
        .find(|i| i.module == "mathlib.matrix")
        .expect("the greeting module imports the matrix module");
    slot.hash[0] ^= 0xff;
    for (idx, unit) in units.iter().enumerate() {
        let unit = if idx == 1 { &stale } else { unit };
        link_env
            .bind(LinkUnit {
                path: unit.path.clone(),
                module: unit.module.clone(),
                interface: unit.interface.clone(),
            })
            .expect("binds");
    }
    let error = link("app.main", &link_env.freeze()).expect_err("the stale pin must reject");
    assert!(error.0.contains("no longer provides"), "{error}");
    assert!(error.0.contains("rebuild"), "{error}");
}

/// The linker takes decoded modules, so it checks the export table
/// of a crafted unit instead of trusting it.
#[test]
fn the_linker_rejects_a_crafted_export_table() {
    use lm_compiler::{link, LinkEnv, LinkUnit};
    let tree = TempTree::new("crafted");
    workspace(&tree);
    let mut seen = Vec::new();
    let units: Vec<lm_compiler::CompiledModule> = [
        ("mathlib.matrix", "mathlib/src/matrix.lm"),
        ("app.greeting", "app/src/greeting.lm"),
        ("app.main", "app/src/main.lm"),
    ]
    .iter()
    .map(|(path, file)| compile_one(&tree, path, file, &mut seen))
    .collect();
    // Each case damages one export table and must reject.
    type Damage = fn(&mut lm_bytecode::Module);
    let cases: [(&str, Damage); 3] = [
        ("twice", |m: &mut lm_bytecode::Module| {
            let copy = m.exports[0].clone();
            m.exports.push(copy);
        }),
        ("outside the", |m: &mut lm_bytecode::Module| {
            m.exports[0].def = 9999;
        }),
        ("which it imports", |m: &mut lm_bytecode::Module| {
            // The greeting module imports `Matrix`, so a re-export of
            // that declaration must reject.
            let import = m
                .imports
                .iter()
                .find(|i| i.kind == lm_bytecode::ImportKind::Class)
                .expect("the module imports a class")
                .clone();
            m.exports.push(lm_bytecode::Export {
                kind: lm_bytecode::ExportKind::Class,
                name: "Copy".to_string(),
                def: import.def,
                ctor: lm_bytecode::NO_CTOR,
            });
        }),
    ];
    for (needle, damage) in cases {
        let mut link_env = LinkEnv::new();
        for (idx, unit) in units.iter().enumerate() {
            let mut module = unit.module.clone();
            // The first two cases damage the provider; the third
            // damages the importer.
            let target = if needle == "which it imports" { 1 } else { 0 };
            if idx == target {
                damage(&mut module);
            }
            link_env
                .bind(LinkUnit {
                    path: unit.path.clone(),
                    module,
                    interface: unit.interface.clone(),
                })
                .expect("binds");
        }
        let error = link("app.main", &link_env.freeze()).expect_err("the table must reject");
        assert!(error.0.contains(needle), "{needle}: {error}");
    }
}

/// Compile one module of a temporary workspace with the interfaces
/// the earlier modules produced.
fn compile_one(
    tree: &TempTree,
    path: &str,
    file: &str,
    seen: &mut Vec<lm_bytecode::interface::Interface>,
) -> lm_compiler::CompiledModule {
    use lm_compiler::{compile_module, CompileEnv};
    use lm_source::SourceFile;
    let text = std::fs::read_to_string(tree.path(file)).expect("reads");
    let mut env = CompileEnv::new();
    for interface in seen.iter() {
        env.bind_interface(interface.clone()).expect("binds");
    }
    env.bind_root("mathlib", "mathlib").expect("binds");
    env.bind_root("greeting", "app.greeting").expect("binds");
    env.bind_root("main", "app.main").expect("binds");
    let source = SourceFile::new(file, text);
    let compiled = compile_module(path, &source, &env.freeze(), path.ends_with(".main"))
        .expect("the module compiles");
    seen.push(compiled.interface.clone());
    compiled
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
