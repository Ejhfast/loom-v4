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
use lib.cases.Marker

def kind_name(kind: CodeKind): String
  case kind
  in CodeKind.Class then "class"
  in CodeKind.Enum then "enum"
  in CodeKind.Interface then "interface"
  in CodeKind.Function then "function"
  in CodeKind.Constant then "constant"
  in CodeKind.Method then "method"
  end
end

module_code = codeof(cases)
out = [module_code.name()]
for declaration in module_code.declarations()
  out.push("#{kind_name(declaration.kind())}:#{declaration.name()}")
  for member in declaration.members()
    out.push("#{declaration.name()}.#{kind_name(member.kind())}:#{member.name()}")
  end
end
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Class[type C: Marker](class_descriptor)
    out.push("matched:#{class_descriptor.name()}")
  in _ then ()
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
         \"function:twice\", \"constant:Answer\", \"matched:Sample\"])"
    );
}

#[test]
fn portable_descriptions_keep_generic_class_metadata() {
    let tree = TempTree::new("generic-description");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "use std.test\n\nclass Box[T] implements Test\nend\n",
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

module = codeof(cases)
out = List[String]()
for declaration in module.declarations()
  if declaration.name() == "Box"
    out.push("#{declaration.type_parameter_count()}:#{declaration.interface_names().at(0)}")
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
    assert_eq!(vm.show_outcome(&outcome), "Done([\"1:core.Test\"])");
}

#[test]
fn codeof_rejects_a_shadowed_module_alias() {
    let source = "value = 1\ncodeof(value)\n";
    let error = lm_testkit::compile_text("shadowed-codeof.lm", source)
        .expect_err("a local value cannot be reified");
    assert!(error.contains("cannot reify a local value"), "{error}");
}

#[test]
fn a_scoped_effect_row_cannot_escape_its_reflection_arm() {
    let tree = TempTree::new("scoped-effect");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def effectful(): Int with Io.Write\n  1\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.cases

module_code = codeof(cases)
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Def[effect e, () -> Int with e](call)
    call()
  in _ then ()
  end
end
0
"#,
    );
    let error = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect_err("the scoped effect row cannot enter the module row");
    assert!(error.contains("E1046"), "{error}");
    assert!(error.contains("scoped effect row `e`"), "{error}");
}

#[test]
fn a_scoped_effect_row_cannot_enter_an_inferred_closure() {
    let tree = TempTree::new("scoped-closure-effect");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def effectful(): Int with Io.Write\n  1\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.cases

module_code = codeof(cases)
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Def[effect e, () -> Int with e](call)
    action = do || call() end
    action()
  in _ then 0
  end
end
0
"#,
    );
    let error = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect_err("the scoped effect row cannot enter an inferred closure");
    assert!(error.contains("E1046"), "{error}");
    assert!(error.contains("scoped effect row `e`"), "{error}");
    assert!(!error.contains("e0"), "{error}");
}

#[test]
fn a_declared_closure_can_own_a_scoped_effect_row() {
    let tree = TempTree::new("declared-scoped-closure");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def effectful(): Int with Io.Write\n  1\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.cases

def run(module_code: ModuleCode): Int with Vm
  for declaration in module_code.declarations()
    case module_code.open(declaration)
    in Def[effect e, () -> Int with e](call)
      program = do ||: Int with e
        call() + 100
      end
      child = sys.vm.Vm().activate_or_fault(program, args: ())
      case child.run()
      in Ok(value) then return value
      in Err(_) then return -1
      end
    in _ then ()
    end
  end
  0
end

run(codeof(cases))
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the declared closure package builds");
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
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(101)");
}

#[test]
fn a_nested_closure_can_infer_an_owned_scoped_effect_row() {
    let tree = TempTree::new("nested-scoped-closure");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def effectful(): Int with Io.Write\n  1\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.cases

def preserve[effect x](escaping f: () -> Int with x): () -> Int with x
  f
end

def run(module_code: ModuleCode): Int with Vm
  for declaration in module_code.declarations()
    case module_code.open(declaration)
    in Def[effect e, () -> Int with e](call)
      program = do ||: Int with e
        nested = preserve(do || call() end)
        nested() + 100
      end
      child = sys.vm.Vm().activate_or_fault(program, args: ())
      case child.run()
      in Ok(value) then return value
      in Err(_) then return -1
      end
    in _ then ()
    end
  end
  0
end

run(codeof(cases))
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the nested closure package builds");
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
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(101)");
}

#[test]
fn descriptor_fields_are_not_source_visible() {
    let source = "def read(value: ModuleCode): Int\n  value._module\nend\n1\n";
    let error = lm_testkit::compile_text("opaque-descriptor.lm", source)
        .expect_err("a descriptor field cannot be read");
    assert!(error.contains("has no field named `_module`"), "{error}");
}

#[test]
fn a_refinement_can_match_a_nonescaping_callback() {
    let tree = TempTree::new("nonescaping");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/callbacks.lm",
        "def apply_nonescaping(f: (Int) -> Int): Int\n  f(41)\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.callbacks

answer = 0
module_code = codeof(callbacks)
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Def[(nonescaping (Int) -> Int) -> Int](call)
    answer = call(do |value: Int| value + 1 end)
  in _ then ()
  end
end
answer
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the callback refinement package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(42)");
}

#[test]
fn nonescaping_requires_a_function_type() {
    let error = lm_testkit::compile_text(
        "invalid-nonescaping.lm",
        "def invalid(value: nonescaping Int): Int\n  value\nend\n0\n",
    )
    .expect_err("a non-function type cannot be nonescaping");
    assert!(error.contains("`nonescaping` requires a function type"));
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

#[test]
fn an_active_reflection_scope_survives_an_external_snapshot() {
    let tree = TempTree::new("scoped-snapshot");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/data.lm",
        "def same(value: Int): Int\n  value\nend\n",
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.data

def inspect(module_code: ModuleCode): Int
  for declaration in module_code.declarations()
    case module_code.open(declaration)
    in Def[type T, (T) -> T](call)
      i = 0
      while i < 1000
        i = i + 1
      end
      return 42
    in _ then ()
    end
  end
  0
end

inspect(codeof(data))
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
    let mut active = None;
    for _ in 0..128 {
        world.drive_slice(root, 1);
        let gate = world.next_gate();
        let image = world
            .capture_snapshot(gate, 0, false)
            .expect("the active program captures");
        let frame = image
            .world()
            .machines
            .first()
            .and_then(|machine| machine.frames.last());
        let active_scope = frame
            .and_then(|frame| image.world().envs.get(frame.env as usize))
            .is_some_and(|environment| !environment.is_empty());
        if active_scope {
            active = Some(image);
            break;
        }
    }
    let image = active.expect("the capture reaches the reflection arm");
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
    assert_eq!(restored.show_value_of(vm, value), "42");
}

#[test]
fn scoped_callable_refinement_runs_in_the_enclosing_block() {
    let tree = TempTree::new("refinement");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        r#"
interface Marker
end

class Sample implements Marker
  value: Int = 7

  def read(self): Int
    self.value
  end
end

def answer(): Int
  42
end

const Answer: Int = 42
"#,
    );
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.cases
use lib.cases.Marker

def apply[T](value: T, f: (T) -> Int): Int
  f(value)
end

def preserve[effect e](escaping f: () -> Int with e): () -> Int with e
  f
end

def reflected_answer(module_code: ModuleCode): Int
  for declaration in module_code.declarations()
    case module_code.open(declaration)
    in Def[() -> Int](call) then return call()
    in _ then ()
    end
  end
  0
end

def activate_reflected(module_code: ModuleCode): Int with Vm
  for declaration in module_code.declarations()
    case module_code.open(declaration)
    in Class[type C: Marker, () -> C](make)
      instance = make()
      for member in declaration.members()
        case module_code.open(member)
        in Method[(mut C) -> Int](read)
          child = sys.vm.Vm().activate_or_fault(read, args: (instance,))
          case child.run()
          in Ok(value) then return value
          in Err(_) then return -1
          end
        in _ then ()
        end
      end
    in _ then ()
    end
  end
  0
end

total = 0
module_code = codeof(cases)
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Class[type C: Marker, () -> C](make)
    instance = make()
    for member in declaration.members()
      case module_code.open(member)
      in Method[(C) -> Int](read)
        total = total + apply(instance, read)
      in _ then ()
      end
    end
  in Def[effect e, () -> Int with e](call)
    ignored = preserve(call)
    total = total + 42
  in Const[Int](value) then total = total + value
  in _ then ()
  end
end
first = 0
for declaration in module_code.declarations()
  case module_code.open(declaration)
  in Def[() -> Int](call)
    first = call()
    break
  in _ then ()
  end
end
(total, first, reflected_answer(codeof(cases)), activate_reflected(codeof(cases)))
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the refinement package builds");
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
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done((91, 42, 42, 7))");
}
