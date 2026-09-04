//! Exact source reflection and opaque descriptor checks.

use lm_compiler::build_package;
use lm_vm::snapshot::LoadLimits;
use lm_vm::{RecordingHost, RootEvent, TaskKey, Vm, VmConfig, World};
use std::path::PathBuf;

/// One temporary package tree.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new(label: &str) -> TempTree {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "lm-reflection-{label}-{}-{unique}",
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

#[test]
fn an_exact_module_lists_only_source_declarations() {
    let tree = TempTree::new("surface");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        r##"
interface Marker
  def marked(self): Bool
end

class Base
  def base(self): Int
    1
  end
end

class Sample < Base implements Marker
  def own(self): Int
    2
  end

  def marked(self): Bool
    true
  end
end

enum Choice
  Left
  Right(value: Int)
end

def twice(value: Int): Int
  value * 2
end

const Answer: Int = 42
"##,
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r##"
use lib.cases

module_code = codeof(cases)
out = [module_code.name()]
for declaration in module_code.declarations()
  out.push("#{declaration.kind()}:#{declaration.name()}")
  for member in declaration.members()
    out.push("#{declaration.name()}.#{member.kind()}:#{member.name()}")
  end
end
out
"##,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the reflective package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(
        vm.show_outcome(&outcome),
        "Done([\"lib.cases\", \"interface:Marker\", \"class:Base\", \
         \"Base.method:base\", \"class:Sample\", \"Sample.method:own\", \
         \"Sample.method:marked\", \"Sample.method:base\", \"enum:Choice\", \
         \"function:twice\", \"constant:Answer\"])"
    );
}

#[test]
fn codeof_rejects_a_shadowed_module_alias() {
    let source = "value = 1\ncodeof(value)\n";
    let error = lm_testkit::compile_text("shadowed-codeof.lm", source)
        .expect_err("a local value cannot be reified");
    assert!(error.contains("cannot reify a local value"), "{error}");
}

#[test]
fn descriptor_fields_are_not_source_visible() {
    let source = "def read(value: ModuleCode): Int\n  value._module\nend\n1\n";
    let error = lm_testkit::compile_text("opaque-descriptor.lm", source)
        .expect_err("a descriptor field cannot be read");
    assert!(error.contains("has no field named `_module`"), "{error}");
}

#[test]
fn a_module_descriptor_survives_an_external_snapshot() {
    let tree = TempTree::new("snapshot");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write("lib/src/data.lm", "const Answer: Int = 42\n");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.data

descriptor = codeof(data)
i = 0
while i < 100000
  i = i + 1
end
descriptor.name()
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the reflective package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let root = TaskKey {
        vm: 0,
        generation: 0,
    };
    world.drive_slice(root, 64);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the active program captures");
    let encoded = image.bytes().expect("the snapshot encodes");
    let admitted =
        lm_testkit::load_snapshot_for_artifact_bytes(&bytes, encoded, LoadLimits::default())
            .expect("the external snapshot admits");

    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact republishes");
    let mut restored = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = restored.new_child(0).expect("the restore target exists");
    let vm = restored
        .restore_image(0, target, &admitted)
        .expect("the snapshot restores");
    let RootEvent::Done(value) = restored.run_machine(vm) else {
        panic!("the restored program does not finish");
    };
    assert_eq!(restored.show_value_of(vm, value), "\"lib.data\"");
}
