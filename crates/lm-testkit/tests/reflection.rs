//! Exact source reflection and opaque descriptor checks.

use lm_compiler::build_package;
use lm_vm::snapshot::LoadLimits;
use lm_vm::{Engine, EngineMode, RecordingHost, RootEvent, TaskKey, Vm, VmConfig, World};
use std::path::PathBuf;
use std::sync::Arc;

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
        r#"
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
"#,
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
fn descriptors_from_another_module_miss_and_survive_a_snapshot() {
    let tree = TempTree::new("foreign-open");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/first.lm",
        r#"
class Sample
  def value(self): Int
    1
  end
end

def answer(): Int
  42
end
"#,
    );
    tree.write("lib/src/second.lm", "def answer(): Int\n  7\nend\n");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [dependencies]\nlib = { path = \"../lib\" }\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
use lib.first
use lib.first.Sample
use lib.second

first_module = codeof(first)
second_module = codeof(second)
declarations = first_module.declarations()
opened_declaration = second_module.open(declarations.at(1))
opened_member = second_module.open(declarations.at(0).members().at(0))

declaration_miss = case opened_declaration
in Def[() -> Int](_) then 0
in _ then 1
end
member_miss = case opened_member
in Method[(Sample) -> Int](_) then 0
in _ then 1
end

(declaration_miss, member_miss, opened_declaration, opened_member)
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the foreign open package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    for mode in [
        EngineMode::Interpreter,
        EngineMode::Auto,
        EngineMode::Native,
    ] {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
        let mut world = World::new_with_engine(
            arena,
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::new(Engine::new(mode)),
        );
        let outcome = lm_proc::run_world(&mut world);
        let shown = world.show_outcome(&outcome);
        let dump = world.dump_live(&outcome);
        assert_eq!(
            shown, "Done((1, 1, <opened code>, <opened code>))",
            "mode {mode:?}\n{dump}"
        );

        let gate = world.next_gate();
        let snapshot = world
            .capture_snapshot(gate, 0, false)
            .expect("the foreign open result captures");
        let encoded = snapshot.bytes().expect("the foreign open snapshot encodes");
        let admitted =
            lm_testkit::load_snapshot_for_artifact_bytes(&bytes, encoded, LoadLimits::default())
                .expect("the foreign open snapshot admits");

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
        assert_eq!(
            restored.show_value_of(vm, value),
            "(1, 1, <opened code>, <opened code>)"
        );
    }
}

#[test]
fn an_exact_open_returns_portable_function_code() {
    let tree = TempTree::new("exact-open");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def twice(value: Int): Int\n  value * 2\nend\n",
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

def run(module: ModuleCode): Int with Vm
  for declaration in module.declarations()
    case module.open(declaration)
    in Code[(Int) -> Int](portable)
      child = sys.vm.Vm()
      binding = child.install(portable).expect("the code installs")
      active = child.activate(binding, args: (21,)).expect("the code activates")
      return active.run().expect("the code runs")
    in _ then ()
    end
  end
  0
end

run(codeof(cases))
"##,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the exact open package builds");
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
    assert_eq!(world.show_outcome(&outcome), "Done(42)");
}

#[test]
fn an_exact_member_open_preserves_a_mutable_receiver() {
    let tree = TempTree::new("exact-mutable-member");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        r#"
class Counter
  value: Int = 41

  def bump(mut self): Int
    self.value = self.value + 1
    self.value
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
        "app/src/main.lm",
        r##"
use lib.cases

def run(module: ModuleCode): Int with Vm
  for declaration in module.declarations()
    case module.open(declaration)
    in Class[type C, () -> C](make)
      instance = make()
      for member in declaration.members()
        if member.name() == "bump"
          case module.open(member)
          in Code[(mut C) -> Int](portable)
            child = sys.vm.Vm()
            binding = child.install(portable).expect("the method installs")
            active = child.activate(binding, args: (instance,)).expect("the method activates")
            return active.run().expect("the method runs")
          in _ then return -1
          end
        end
      end
    in _ then ()
    end
  end
  0
end

run(codeof(cases))
"##,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the exact member package builds");
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
    assert_eq!(world.show_outcome(&outcome), "Done(42)");
}

#[test]
fn a_live_open_reads_the_current_image_binding() {
    let tree = TempTree::new("live-binding");
    tree.write(
        "lib/lm.package",
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "lib/src/cases.lm",
        "def twice(value: Int): Int\n  value * 2\nend\n",
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
use lib.cases.twice

def direct(value: Int): Int
  twice(value)
end

def reflected(value: Int): Int
  module = codeof(cases)
  for declaration in module.declarations()
    if declaration.name() == "twice"
      case module.open(declaration)
      in Def[(Int) -> Int](call) then return call(value)
      in _ then return -1
      end
    end
  end
  -2
end

def run(
  image: Vm,
  function: FunctionBinding[(Int) -> Int],
  value: Int
): Int with Vm
  active = image.activate(function, args: (value,)).expect("the call activates")
  active.run().expect("the call runs")
end

def execute(): (Int, Int, Int, Int) with Compiler, Vm
  image = sys.vm.Vm()
  original = image.install(codeof(twice)).expect("the original installs")
  direct_call = image.install(direct).expect("the direct call installs")
  reflected_call = image.install(reflected).expect("the reflected call installs")
  first_direct = run(image, direct_call, 21)
  first_reflected = run(image, reflected_call, 21)

  spec = codeof(twice).definition()
  definitions = List[(String, DefinitionSpec)]()
  definitions.push(("twice", spec))
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    definitions
  )
  options = CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = sys.compiler.compile(
    spec.identity.module_name,
    "twice-revision.lm",
    "def twice(value: Int): Int\n  value * 3\nend\n",
    env,
    options
  ).expect("the revision compiles")
  verified = artifact.verify().expect("the revision verifies")
  code = verified.function_code[(Int) -> Int]("twice").expect("the function exists")
  replacement = image.install(code).expect("the revision installs")
  image.replace(original, replacement).expect("the binding changes")

  (
    first_direct,
    first_reflected,
    run(image, direct_call, 21),
    run(image, reflected_call, 21)
  )
end

execute()
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the live binding package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(lm_host::CliHost::new(1)),
    );
    world.allow("Compiler").expect("the compiler grant exists");
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done((42, 42, 63, 63))");
}

#[test]
fn a_runtime_module_links_and_opens_in_the_current_image() {
    let tree = TempTree::new("runtime-link");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/baseline.lm",
        "def triple(value: Int): Int\n  value * 3\nend\n",
    );
    tree.write(
        "app/src/main.lm",
        r##"
use baseline

def options(): CompileOptions
  CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
end

def find_live(module: ModuleCode): Int
  for declaration in module.declarations()
    case module.open(declaration)
    in Def[(Int) -> Int](call) then return call(21)
    in _ then ()
    end
  end
  -1
end

def run_exact(module: ModuleCode): Int with Vm
  for declaration in module.declarations()
    case module.open(declaration)
    in Code[(Int) -> Int](portable)
      image = sys.vm.Vm()
      binding = image.install(portable).expect("the exact code installs")
      run = image.activate(binding, args: (21,)).expect("the exact code activates")
      return run.run().expect("the exact code runs")
    in _ then ()
    end
  end
  -1
end

def run_live_in(module: ModuleCode, image: Vm): Int with Vm
  for declaration in module.declarations()
    case module.open(declaration)
    in Def[(Int) -> Int](call)
      case image.activate(call, args: (21,))
      in Ok(run)
        case run.run()
        in Ok(value) then return value
        in Err(_) then return -2
        end
      in Err(_) then return -2
      end
    in _ then ()
    end
  end
  -1
end

def module_is_local(module: ModuleCode): Bool with Vm
  image = sys.vm.Vm()
  program = do || module.name() end
  image.activate(program, args: ()).is_err()
end

def misses_static_module(declaration: DeclarationCode): Bool
  case codeof(baseline).open(declaration)
  in Def[(Int) -> Int](_) then false
  in _ then true
  end
end

def execute(): (String, String, Int, Int, Int, Int, Int, Bool, Bool) with Compiler, Vm
  old_image = sys.vm.Vm()
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  artifact = sys.compiler.compile(
    "plugin",
    "plugin.lm",
    "def twice(value: Int): Int\n  value * 2\nend\n",
    env,
    options()
  ).expect("the plugin compiles")
  verified = artifact.verify().expect("the plugin verifies")
  described = "#{verified.name()}.#{verified.declarations().at(0).name()}"
  foreign_miss = misses_static_module(verified.declarations().at(0))
  first = sys.vm.link(verified).expect("the plugin links")
  second = sys.vm.link(verified).expect("the plugin links once")
  new_image = sys.vm.Vm()
  (
    described,
    first.name(),
    find_live(first),
    run_exact(first),
    find_live(second),
    run_live_in(first, new_image),
    run_live_in(first, old_image),
    module_is_local(first),
    foreign_miss
  )
end

execute()
"##,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the runtime link package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    for mode in [
        EngineMode::Interpreter,
        EngineMode::Auto,
        EngineMode::Native,
    ] {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
        let mut world = World::new_with_engine(
            arena,
            namespace,
            VmConfig::default(),
            Box::new(lm_host::CliHost::new(1)),
            Arc::new(Engine::new(mode)),
        );
        world.allow("Compiler").expect("the compiler grant exists");
        world.allow("Vm").expect("the VM grant exists");
        let outcome = lm_proc::run_world(&mut world);
        let shown = world.show_outcome(&outcome);
        let dump = world.dump_live(&outcome);
        assert_eq!(
            shown, "Done((\"plugin.twice\", \"plugin\", 42, 42, 42, 42, -2, true, true))",
            "mode {mode:?}\n{dump}"
        );
        let gate = world.next_gate();
        let snapshot = world
            .capture_snapshot(gate, 0, false)
            .expect("the linked image captures");
        assert_eq!(snapshot.world().vm_images[0].instances.len(), 1);
        let encoded = snapshot.bytes().expect("the linked snapshot encodes");
        lm_testkit::load_snapshot_for_artifact_bytes(&bytes, encoded, LoadLimits::default())
            .expect("the linked snapshot admits");
    }
}

#[test]
fn a_runtime_link_rejects_an_existing_module_path() {
    let tree = TempTree::new("link-conflict");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    let source = r#"
def options(): CompileOptions
  CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
end

def execute(): Bool with Compiler, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  artifact = sys.compiler.compile(
    "app.main",
    "replacement.lm",
    "def replacement(): Int\n  42\nend\n",
    env,
    options()
  ).expect("the conflicting module compiles")
  verified = artifact.verify().expect("the conflicting module verifies")
  sys.vm.link(verified).is_err()
end

execute()
"#;
    tree.write("app/src/main.lm", source);
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the conflict test package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(lm_host::CliHost::new(1)),
    );
    world.allow("Compiler").expect("the compiler grant exists");
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    let gate = world.next_gate();
    let snapshot = world
        .capture_snapshot(gate, 0, false)
        .expect("the unchanged image captures");
    assert!(snapshot.world().vm_images[0].instances.is_empty());
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
fn a_runtime_linked_descriptor_survives_an_external_snapshot() {
    let tree = TempTree::new("linked-snapshot");
    tree.write(
        "app/lm.package",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    tree.write(
        "app/src/main.lm",
        r#"
def options(): CompileOptions
  CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
end

def execute(): ModuleCode with Compiler, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  artifact = sys.compiler.compile(
    "plugin",
    "plugin.lm",
    "def answer(): Int\n  42\nend\n",
    env,
    options()
  ).expect("the plugin compiles")
  verified = artifact.verify().expect("the plugin verifies")
  sys.vm.link(verified).expect("the plugin links")
end

execute()
"#,
    );
    let report = build_package(&tree.root.join("app"), &tree.root.join("build"))
        .expect("the linked snapshot package builds");
    let bytes = std::fs::read(report.artifact.expect("the program artifact exists"))
        .expect("the artifact reads");
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the artifact publishes");
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(lm_host::CliHost::new(1)),
        Arc::new(Engine::new(EngineMode::Native)),
    );
    world.allow("Compiler").expect("the compiler grant exists");
    world.allow("Vm").expect("the VM grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(<linked module>)");
    let gate = world.next_gate();
    let snapshot = world
        .capture_snapshot(gate, 0, false)
        .expect("the linked result captures");
    assert_eq!(snapshot.world().vm_images[0].instances.len(), 1);
    let encoded = snapshot.bytes().expect("the snapshot encodes");
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
    assert_eq!(restored.show_value_of(vm, value), "<linked module>");
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
