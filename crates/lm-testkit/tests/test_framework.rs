//! Generated test entry and standard test runner checks.

use lm_compiler::{build_package, build_test_package};
use lm_vm::{RecordingHost, VmConfig, World};
use std::path::PathBuf;

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> TempTree {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lm-test-framework-{label}-{}-{unique}",
            std::process::id()
        ));
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
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_tests(tree: &TempTree) -> String {
    let report = build_test_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the test package builds");
    let bytes = std::fs::read(report.artifact.expect("the test artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Vm", "Args", "Io.Write"] {
        world.allow(grant).expect("the test grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn a_generated_entry_runs_root_package_tests_instead_of_main() {
    let tree = TempTree::new("root");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write("app/src/main.lm", "99\n");
    tree.write(
        "app/src/suite.lm",
        r#"
use std.test

class ArithmeticTest implements Test
  def adds(self): Result[(), test.TestFailure]
    test.equal(4, 2 + 2)
  end
end
"#,
    );
    assert_eq!(run_tests(&tree), "Done(0)");
}

#[test]
fn inherited_test_methods_run_and_dependency_tests_do_not_run() {
    let tree = TempTree::new("scope");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/suite.lm",
        r#"
use std.test

class HiddenTest implements Test
  def fails(self): Result[(), test.TestFailure]
    test.fail("a dependency test ran")
  end
end
"#,
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/suite.lm",
        r#"
use std.test

class Base
  def inherited(self): Result[(), test.TestFailure]
    test.fail("the inherited test ran")
  end
end

class DerivedTest < Base implements Test
end
"#,
    );
    assert_eq!(run_tests(&tree), "Done(1)");
}

#[test]
fn a_test_class_without_runnable_methods_is_a_failure() {
    let tree = TempTree::new("empty");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/suite.lm",
        "use std.test\n\nclass EmptyTest implements Test\nend\n",
    );
    assert_eq!(run_tests(&tree), "Done(1)");
}

#[test]
fn a_test_class_with_constructor_arguments_is_a_failure() {
    let tree = TempTree::new("constructor");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/suite.lm",
        r#"
use std.test

class ConfiguredTest implements Test
  value: Int

  def init(mut self, value: Int)
    self.value = value
  end

  def passes(self): Result[(), test.TestFailure]
    test.pass()
  end
end
"#,
    );
    assert_eq!(run_tests(&tree), "Done(1)");
}

#[test]
fn a_program_artifact_omits_an_unreachable_test_class() {
    let tree = TempTree::new("thin-program");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use std.test

class HiddenTest implements Test
  def passes(self): Result[(), test.TestFailure]
    test.pass()
  end
end

42
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the program package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let artifact = lm_bytecode::artifact::decode(&bytes).expect("the artifact decodes");
    assert!(artifact
        .units()
        .iter()
        .all(|unit| unit.module_path() != "std.test"));
    assert!(artifact.units().iter().all(|unit| {
        unit.module()
            .classes
            .iter()
            .all(|class| class.name != "HiddenTest")
    }));
}
