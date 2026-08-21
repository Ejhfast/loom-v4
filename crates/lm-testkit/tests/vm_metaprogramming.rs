//! Reified compiler and VM integration.

use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_host::CliHost;
use lm_source::SourceFile;
use lm_testkit::compile_to_bytes;
use lm_vm::snapshot::{codec, LoadLimits, SnapshotFail};
use lm_vm::{load_bytes, RecordingHost, RootEvent, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn run_with_files(source: &str, files: &[(&str, Vec<u8>)]) -> String {
    let bytes = compile_to_bytes("meta.lm", source).expect("the test program compiles");
    let loaded = load_bytes(&bytes).expect("the test program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    for (name, bytes) in files {
        host.borrow_mut().set_file(*name, bytes.clone());
    }
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

fn run_with_compiler(source: &str) -> String {
    let bytes = compile_to_bytes("meta-compiler.lm", source).expect("the test program compiles");
    let loaded = load_bytes(&bytes).expect("the test program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
    for grant in ["Compiler", "Reflect", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn syntax_views_cannot_be_constructed_directly() {
    let error = compile_to_bytes("forged-syntax.lm", "SyntaxNode(\"x\", Bytes(), 0)\n")
        .expect_err("the opaque syntax view rejects construction");
    assert!(error.contains("error[E1026]"));
    assert!(error.contains("SyntaxNode` values cannot be constructed directly"));
}

#[test]
fn syntax_view_hierarchy_rejects_user_subclasses() {
    let error = compile_to_bytes(
        "syntax-subclass.lm",
        "class ForgedSyntax < SyntaxNode\nend\n0\n",
    )
    .expect_err("the sealed syntax hierarchy rejects a subclass");
    assert!(error.contains("error[E1040]"));
    assert!(error.contains("permits only core syntax classes"));
}

#[test]
fn verifier_rejects_direct_native_code_handle_allocation() {
    let mut module = lm_testkit::compile_text("native-code-forgery.lm", "0\n")
        .expect("the seed program compiles");
    let class = module.core_roles[lm_bytecode::corepin::ROLE_ARTIFACT];
    let entry = module.entry as usize;
    module.funcs[entry].blocks = vec![vec![
        lm_bytecode::Instr::New(class),
        lm_bytecode::Instr::Return,
    ]];
    let error = lm_verify::verify_module(&module).expect_err("the verifier rejects the forgery");
    assert!(error.message.contains("native core class"));
}

#[test]
fn public_syntax_preserves_text_structure_and_detaches() {
    let source = r#"
def execute(): (Bool, Bool, Bool, Bool) with Reflect.ParseSyntax
  text = "40 + 2 # answer\n"
  parsed = sys.reflect.parse_syntax(text)
  case parsed.status
  in ParseComplete
    syntax = parsed.tree.root()
    rebuilt = StringBuilder()
    for child in syntax.children()
      rebuilt.append(child.text())
    end
    expression = syntax.children().at(0)
    token = expression.children().at(0)
    detached = expression.detach()
    (
      rebuilt.build() == text,
      syntax.text() == text,
      token.is_token(),
      detached.text() == expression.text()
    )
  in ParseIncomplete then (false, false, false, false)
  in ParseInvalid then (false, false, false, false)
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done((true, true, true, true))");
}

#[test]
fn a_child_node_becomes_the_root_of_its_new_tree() {
    let source = r#"
def execute(): Bool with Reflect.ParseSyntax
  parsed = sys.reflect.parse_syntax("40 + 2\n")
  case parsed.status
  in ParseComplete
    child = parsed.tree.root().children().at(0) as SyntaxNode
    tree = child.to_tree()
    tree.source() == "40 + 2" and tree.root().text() == "40 + 2"
  in ParseIncomplete then false
  in ParseInvalid then false
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn loom_builds_syntax_without_parsing_and_runs_it() {
    let source = r#"
def execute(): Int with Compiler.CompileSyntax, Vm
  builder = SyntaxBuilder()
  statement_items = List[SyntaxElement]()
  statement_items.push(builder.integer("40"))
  statement_items.push(builder.whitespace(" "))
  statement_items.push(builder.plus())
  statement_items.push(builder.whitespace(" "))
  statement_items.push(builder.integer("2"))
  statement_items.push(builder.newline())
  statement = builder.statement(statement_items)
  module_items = List[SyntaxElement]()
  module_items.push(statement)
  syntax = builder.module(module_items).to_tree().root()

  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  artifact = case sys.compiler.compile_syntax(syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 5
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 5
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn invalid_built_syntax_returns_compile_errors() {
    let source = r#"
def execute(): Bool with Compiler.CompileSyntax
  builder = SyntaxBuilder()
  items = List[SyntaxElement]()
  items.push(builder.identifier("@"))
  syntax = builder.invalid(items)
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  case sys.compiler.compile_syntax(syntax, env, options)
  in Ok(_) then false
  in Err(error) then error.message.len() > 0
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn interaction_parser_classifies_definitions_incomplete_and_invalid_text() {
    let source = r#"
def classify(text: String): Int with Reflect.ParseSyntax
  parsed = sys.reflect.parse_syntax(text)
  case parsed.status
  in ParseIncomplete then 3
  in ParseInvalid
    if parsed.diagnostics.at(0).message.len() > 0 and
       parsed.tree.root().children().at(0).is_invalid()
      4
    else
      0
    end
  in ParseComplete
    has_definitions = false
    has_statements = false
    for child in parsed.tree.root().children()
      if child.is_definition()
        has_definitions = true
      elsif child.is_statement()
        has_statements = true
      end
    end
    if has_definitions and has_statements
      5
    elsif has_definitions
      2
    elsif has_statements
      1
    else
      0
    end
  end
end

(
  classify("def answer(): Int\n  42\nend\n"),
  classify("def answer("),
  classify("@"),
  classify("def answer(): Int\n  42\nend\nanswer()\n")
)
"#;
    assert_eq!(run_with_compiler(source), "Done((2, 3, 4, 5))");
}

#[test]
fn loom_compiles_an_expression_syntax_node() {
    let source = r#"
def execute(): Int with Compiler.CompileSyntax, Reflect.ParseSyntax, Vm
  parsed = sys.reflect.parse_syntax("40 + 2\n")
  syntax = case parsed.status
  in ParseComplete then parsed.tree.root()
  in ParseIncomplete
    return 0 - 2
  in ParseInvalid
    return 0 - 3
  end
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  artifact = case sys.compiler.compile_syntax(syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 5
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 6
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 7
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 8
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 8
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn loom_runs_an_expression_with_an_unknown_result_type() {
    let source = r#"
def execute(): Bool with Compiler.CompileSyntax, Reflect.ParseSyntax, Vm
  parsed = sys.reflect.parse_syntax("[1, 2, 3]\n")
  syntax = case parsed.status
  in ParseComplete then parsed.tree.root()
  in ParseIncomplete
    return false
  in ParseInvalid
    return false
  end
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    true,
    false,
    List[String](),
    List[String]()
  )
  artifact = case sys.compiler.compile_syntax(syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return false
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return false
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return false
  end
  entry = case instance.entry[(), DynValue]()
  in Ok(value) then value
  in Err(_)
    return false
  end
  case image.activate(entry, args: ())
  in Err(_) then false
  in Ok(run)
    case run.run()
    in Done(value) then value.render() == "[1, 2, 3]"
    in Fault(_) then false
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn loom_compiles_a_definition_syntax_node() {
    let source = r#"
def execute(): Int with Compiler.CompileSyntax, Reflect.ParseSyntax, Vm
  parsed = sys.reflect.parse_syntax(
    "def add(value: Int): Int\n  value + 2\nend\n"
  )
  syntax = case parsed.status
  in ParseComplete then parsed.tree.root()
  in ParseIncomplete
    return 0 - 2
  in ParseInvalid
    return 0 - 3
  end
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    false,
    false,
    false,
    List[String](),
    List[String]()
  )
  artifact = case sys.compiler.compile_syntax(syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 5
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 6
  end
  function = case instance.function[(Int,), Int]("add")
  in Ok(value) then value
  in Err(_)
    return 0 - 7
  end
  case image.activate(function, args: (40,))
  in Err(_) then 0 - 8
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 8
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn loom_compiles_verifies_installs_and_runs_source() {
    let source = r#"
def execute(): Int with Compiler.Compile, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  artifact = case sys.compiler.compile("runtime", "40 + 2\n", env, options)
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 5
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 5
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn runtime_compilation_links_an_explicit_provider_instance() {
    let source = r#"
def execute(): Int with Compiler.Compile, Vm
  empty_env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  library_options = CompileOptions(
    false,
    false,
    false,
    List[String](),
    List[String]()
  )
  library_artifact = case sys.compiler.compile(
    "dep",
    "def add(value: Int): Int\n  value + 2\nend\n",
    empty_env,
    library_options
  )
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  library_module = case library_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end

  image = sys.vm.Vm()
  library_instance = case image.install(library_module)
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end

  program_env = CompileEnv(
    [library_module],
    [("dep", "dep")]
  )
  program_options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  program_artifact = case sys.compiler.compile(
    "app",
    "use dep\ndep.add(40)\n",
    program_env,
    program_options
  )
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  program_module = case program_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 5
  end
  links = LinkEnv([library_instance])
  program_instance = case image.install(program_module, links)
  in Ok(value) then value
  in Err(_)
    return 0 - 6
  end
  entry = case program_instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 7
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 8
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 8
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn runtime_linked_instances_survive_an_external_snapshot() {
    let source = r#"
def execute(): Int with Compiler.Compile, Vm
  empty = CompileEnv(List[VerifiedModule](), List[(String, String)]())
  library_options = CompileOptions(
    false,
    false,
    false,
    List[String](),
    List[String]()
  )
  library_artifact = case sys.compiler.compile(
    "dep",
    "def add(value: Int): Int\n  value + 2\nend\n",
    empty,
    library_options
  )
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  library_module = case library_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end

  image = sys.vm.Vm()
  library_instance = case image.install(library_module)
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end
  program_env = CompileEnv([library_module], [("dep", "dep")])
  program_options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  program_artifact = case sys.compiler.compile(
    "app",
    "use dep\ndep.add(40)\n",
    program_env,
    program_options
  )
  in Ok(value) then value
  in Err(_)
    return 0 - 4
  end
  program_module = case program_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 5
  end
  program_instance = case image.install(program_module, LinkEnv([library_instance]))
  in Ok(value) then value
  in Err(_)
    return 0 - 6
  end
  entry = case program_instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 7
  end
  count = 0
  for _ in Range(0, 1000)
    count = count + 1
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 8
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 8
    end
  end
end

execute()
"#;
    let bytes =
        compile_to_bytes("snapshot-runtime-links.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
    for grant in ["Compiler", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }

    let mut captured = None;
    let mut ran = 0usize;
    for _ in 0..20_000 {
        let try_capture = match world.step_root() {
            RootEvent::Ran => {
                ran += 1;
                ran.is_multiple_of(32)
            }
            RootEvent::Waiting | RootEvent::Blocked => {
                world.poll_blocked();
                std::thread::sleep(std::time::Duration::from_millis(1));
                false
            }
            event => panic!("the source stopped before capture: {event:?}"),
        };
        if !try_capture {
            continue;
        }
        let gate = world.next_gate();
        match world.capture_snapshot(gate, 0, false) {
            Ok(image) if image.world().installations.len() == 2 => {
                captured = Some(image);
                break;
            }
            Ok(_) | Err(SnapshotFail::ResourceActive { .. }) => {}
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows both installations");
    let admitted = codec::load_external(
        captured.bytes().expect("the snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the linked snapshot admits");
    let mut restored = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    restored.allow("Vm").expect("the grant exists");
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the linked image restores");
    restored.allow_on(root, "Vm").expect("the grant exists");
    loop {
        match restored.run_machine(root) {
            RootEvent::Done(value) => {
                assert_eq!(restored.show_result_of(root, value), "42");
                break;
            }
            RootEvent::Ran => {}
            event => panic!("the restored linked run stopped: {event:?}"),
        }
    }
}

#[test]
fn runtime_compilation_returns_rendered_errors() {
    let source = r#"
def execute(): Bool with Compiler.Compile
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)]()
  )
  options = CompileOptions(
    true,
    false,
    false,
    List[String](),
    List[String]()
  )
  case sys.compiler.compile("broken", "def", env, options)
  in Ok(_) then false
  in Err(errors) then errors.message.len() > 0
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn compiler_policy_blocks_runtime_compilation() {
    let source = r#"
env = CompileEnv(
  List[VerifiedModule](),
  List[(String, String)]()
)
options = CompileOptions(
  true,
  false,
  false,
  List[String](),
  List[String]()
)
sys.compiler.compile("blocked", "1\n", env, options)
"#;
    let bytes = compile_to_bytes("blocked-compiler.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(format!("{outcome:?}"), "Fault(PolicyDenied)");
}

#[test]
fn loom_verifies_installs_and_activates_an_artifact() {
    let artifact = compile_to_bytes("installed.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("installed.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm
  artifact = sys.vm.artifact(artifact_bytes())
  case artifact.verify()
  in Err(_) then 0 - 1
  in Ok(module)
    image = sys.vm.Vm()
    case image.install(module)
    in Err(_) then 0 - 2
    in Ok(instance)
      case instance.entry[(), Int]()
      in Err(_) then 0 - 3
      in Ok(entry)
        case image.activate(entry, args: ())
        in Err(_) then 0 - 4
        in Ok(run)
          case run.run()
          in Done(value) then value
          in Fault(_) then 0 - 4
          end
        end
      end
    end
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("installed.lmbc", artifact)]),
        "Done(42)"
    );
}

fn revision_artifact(body: &str) -> Vec<u8> {
    let source = format!("def step(value: Int): Int\n  {body}\nend\nstep(1)\n");
    let compiled = compile_module_with_options(
        "revision",
        &SourceFile::new("revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_function("step"),
    )
    .expect("the revision compiles");
    lm_bytecode::encode(&compiled.module)
}

fn complete_slot_artifact() -> (Vec<u8>, usize, usize, usize, usize) {
    let source = "final class Box\nend\n\
                  def step(value: Int): Int\n\
                  \x20 value + 1\n\
                  end\n\
                  0\n";
    let compiled = compile_module_with_options(
        "slot-kinds",
        &SourceFile::new("slot-kinds.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new()
            .late_function("step")
            .late_class("Box"),
    )
    .expect("the slot artifact compiles");
    let mut module = compiled.module;
    let step = module
        .exports
        .iter()
        .find(|export| export.name == "step" && export.kind == lm_bytecode::ExportKind::Function)
        .expect("the function is exported")
        .def;
    let class = module
        .exports
        .iter()
        .find(|export| export.name == "Box" && export.kind == lm_bytecode::ExportKind::Class)
        .expect("the class is exported")
        .def;
    let function_slot = module
        .slots
        .iter()
        .position(|slot| slot.initial == Some(lm_bytecode::SlotTarget::Function(step)))
        .expect("the function slot exists");
    let class_slot = module
        .slots
        .iter()
        .position(|slot| slot.initial == Some(lm_bytecode::SlotTarget::Class(class)))
        .expect("the class slot exists");
    let int = module
        .types
        .iter()
        .position(|ty| *ty == lm_bytecode::BcType::Int)
        .expect("the Int type exists") as u32;
    let value_slot = module.slots.len();
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::slot_key("slot-kinds.value"),
        contract: lm_bytecode::SlotContract::Value { ty: int },
        initial: None,
    });
    let process_slot = module.slots.len();
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::slot_key("slot-kinds.process"),
        contract: lm_bytecode::SlotContract::Process {
            message: int,
            result: int,
        },
        initial: None,
    });
    lm_verify::verify_module(&module).expect("the complete slot artifact verifies");
    (
        lm_bytecode::encode(&module),
        function_slot,
        class_slot,
        value_slot,
        process_slot,
    )
}

#[test]
fn a_slot_replacement_changes_later_calls_only() {
    let first = revision_artifact("value + 1");
    let second = revision_artifact("value + 10");
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm
  bytes = case sys.fs.open(path, ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes)
end

def execute(): (Int, Int) with Fs.Open, Fs.Read, Fs.Close, Vm
  image = sys.vm.Vm()
  first_module = case read_artifact("first.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (0 - 1, 0 - 1)
  end
  second_module = case read_artifact("second.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (0 - 2, 0 - 2)
  end
  first = case image.install(first_module)
  in Ok(instance) then instance
  in Err(_)
    return (0 - 3, 0 - 3)
  end
  second = case image.install(second_module)
  in Ok(instance) then instance
  in Err(_)
    return (0 - 4, 0 - 4)
  end
  entry = case first.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return (0 - 5, 0 - 5)
  end
  before_run = case image.activate(entry, args: ())
  in Ok(run) then run
  in Err(_)
    return (0 - 6, 0 - 6)
  end
  before = case before_run.run()
  in Done(value) then value
  in Fault(_)
    return (0 - 6, 0 - 6)
  end
  slot = case first.slot(0)
  in Ok(value) then value
  in Err(_)
    return (0 - 7, 0 - 7)
  end
  target = case second.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return (0 - 8, 0 - 8)
  end
  case image.replace(slot, target)
  in Err(_)
    return (0 - 9, 0 - 9)
  in Ok(_)
    after = case image.activate(entry, args: ())
    in Err(_) then 0 - 10
    in Ok(run)
      case run.run()
      in Done(value) then value
      in Fault(_) then 0 - 10
      end
    end
    (before, after)
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("first.lmbc", first), ("second.lmbc", second)]),
        "Done((2, 11))"
    );
}

#[test]
fn cross_vm_definition_activation_returns_a_code_error() {
    let artifact = compile_to_bytes("cross-vm.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm
  bytes = case sys.fs.open("cross-vm.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes)
end

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm
  left = sys.vm.Vm()
  right = sys.vm.Vm()
  module = case read_artifact().verify()
  in Ok(value) then value
  in Err(_)
    return false
  end
  instance = case left.install(module)
  in Ok(value) then value
  in Err(_)
    return false
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return false
  end
  case right.activate(entry, args: ())
  in Ok(_) then false
  in Err(error) then error.message.len() > 0
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("cross-vm.lmbc", artifact)]),
        "Done(true)"
    );
}

#[test]
fn loom_replaces_every_slot_target_kind() {
    let (artifact, function_slot, class_slot, value_slot, process_slot) = complete_slot_artifact();
    let source = format!(
        r#"
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    7
  end
end

def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm
  bytes = case sys.fs.open("slot-kinds.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes)
end

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Proc
  image = sys.vm.Vm()
  module = case read_artifact().verify()
  in Ok(value) then value
  in Err(_)
    return false
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return false
  end
  function = case instance.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return false
  end
  class_def = case instance.class_def("Box")
  in Ok(value) then value
  in Err(_)
    return false
  end
  function_slot = case instance.slot({function_slot})
  in Ok(value) then value
  in Err(_)
    return false
  end
  class_slot = case instance.slot({class_slot})
  in Ok(value) then value
  in Err(_)
    return false
  end
  value_slot = case instance.slot({value_slot})
  in Ok(value) then value
  in Err(_)
    return false
  end
  process_slot = case instance.slot({process_slot})
  in Ok(value) then value
  in Err(_)
    return false
  end
  worker = Worker.spawn()
  function_ok = case image.replace_function(function_slot, function)
  in Ok(_) then true
  in Err(_) then false
  end
  class_ok = case image.replace_class(class_slot, class_def)
  in Ok(_) then true
  in Err(_) then false
  end
  value_ok = case image.replace_value(value_slot, 41)
  in Ok(_) then true
  in Err(_) then false
  end
  process_ok = case image.replace_process(process_slot, worker)
  in Ok(_) then true
  in Err(_) then false
  end
  case image.snapshot()
  in Err(_) then false
  in Ok(_) then function_ok and class_ok and value_ok and process_ok
  end
end

execute()
"#
    );
    let bytes = compile_to_bytes("slot-kinds-host.lm", &source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("slot-kinds.lmbc", artifact.clone());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Proc"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    let image = world.last_snapshot().expect("the full snapshot exists");
    let slots = &image.world().vm_images[0].slots;
    assert!(slots.iter().any(|slot| matches!(
        slot,
        lm_vm::snapshot::ImageSlotTarget::Value(lm_value::Value::Int(41))
    )));
    assert!(slots
        .iter()
        .any(|slot| matches!(slot, lm_vm::snapshot::ImageSlotTarget::Process { .. })));
}

#[test]
fn loom_captures_and_restores_a_complete_vm() {
    let artifact = compile_to_bytes("full-vm.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm
  bytes = case sys.fs.open("full-vm.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes)
end

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm
  image = sys.vm.Vm()
  module = case read_artifact().verify()
  in Ok(value) then value
  in Err(_)
    return false
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return false
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return false
  end
  case image.activate(entry, args: ())
  in Err(_)
    return false
  in Ok(_) then ()
  end
  snapshot = case image.snapshot()
  in Ok(value) then value
  in Err(_)
    return false
  end
  restored = case sys.vm.restore_vm(snapshot)
  in Ok(value) then value
  in Err(_)
    return false
  end
  case restored.snapshot()
  in Ok(_) then true
  in Err(_) then false
  end
end

execute()
"#;
    let bytes = compile_to_bytes("full-vm-host.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("full-vm.lmbc", artifact.clone());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    let image = world
        .last_snapshot()
        .expect("the second full snapshot exists");
    assert_eq!(image.world().distinguished, None);
    assert_eq!(image.world().full_vm, Some(0));
    assert_eq!(image.world().machines.len(), 1);
    assert_eq!(image.world().installations.len(), 1);
    let admitted = codec::load_external(
        image.bytes().expect("the full snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the external full snapshot admits");
    assert_eq!(admitted.world().distinguished, None);
    assert_eq!(admitted.world().full_vm, Some(0));
    assert_eq!(admitted.world().installations.len(), 1);
}

#[test]
fn loom_captures_and_restores_an_empty_vm() {
    let source = r#"
def execute(): Bool with Vm
  image = sys.vm.Vm()
  snapshot = case image.snapshot()
  in Ok(value) then value
  in Err(_)
    return false
  end
  restored = case sys.vm.restore_vm(snapshot)
  in Ok(value) then value
  in Err(_)
    return false
  end
  case restored.snapshot()
  in Ok(_) then true
  in Err(_) then false
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn loom_loads_an_external_snapshot_as_a_typed_result() {
    let source = r#"
def read_snapshot(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("seed.lms", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm
  case sys.vm.load_snapshot(read_snapshot())
  in Ok(_) then true
  in Err(_) then false
  end
end

execute()
"#;
    let bytes = compile_to_bytes("guest-load.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut seed = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let gate = seed.next_gate();
    let image = seed
        .capture_snapshot(gate, 0, false)
        .expect("the initial machine captures");
    let snapshot = image.bytes().expect("the snapshot encodes").to_vec();

    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("seed.lms", snapshot);
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
}

#[test]
fn loom_rejects_invalid_snapshot_bytes_as_a_typed_error() {
    let source = r#"
def execute(): Bool with Vm
  buffer = ByteBuffer()
  buffer.append(1)
  case sys.vm.load_snapshot(buffer.finish())
  in Ok(_) then false
  in Err(error)
    case error
    in BadImage(reason) then reason.len() > 0
    in ResourceActive(_, _) then false
    in SnapshotLimitExceeded then false
    end
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn installed_code_and_handles_survive_an_external_snapshot() {
    let artifact = compile_to_bytes("installed.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("installed.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm
  image = sys.vm.Vm()
  module = case sys.vm.artifact(artifact_bytes()).verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end
  case image.activate(entry, args: ())
  in Err(_) then 0 - 4
  in Ok(run)
    case run.run()
    in Done(value) then value
    in Fault(_) then 0 - 4
    end
  end
end

execute()
"#;
    let bytes = compile_to_bytes("snapshot-code.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("installed.lmbc", artifact.clone());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }

    let mut captured = None;
    for _ in 0..2000 {
        match world.step_root() {
            RootEvent::Ran => {}
            RootEvent::Waiting | RootEvent::Blocked => {
                world.poll_blocked();
                continue;
            }
            event => panic!("the source stopped before installation: {event:?}"),
        }
        let gate = world.next_gate();
        match world.capture_snapshot(gate, 0, false) {
            Ok(image) if !image.world().installations.is_empty() => {
                captured = Some(image);
                break;
            }
            Ok(_) | Err(SnapshotFail::ResourceActive { .. }) => {}
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows installation");
    assert_eq!(captured.world().installations.len(), 1);
    assert_eq!(captured.world().vm_images.len(), 1);
    assert_eq!(captured.world().vm_images[0].instances.len(), 1);

    let admitted = codec::load_external(
        captured.bytes().expect("the snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the external snapshot admits");
    let mut restored = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    restored.allow("Vm").expect("the grant exists");
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the code image restores");
    restored.allow_on(root, "Vm").expect("the grant exists");
    loop {
        match restored.run_machine(root) {
            RootEvent::Done(value) => {
                assert_eq!(restored.show_result_of(root, value), "42");
                break;
            }
            RootEvent::Ran => {}
            event => panic!("the restored run stopped: {event:?}"),
        }
    }
}

#[test]
fn a_dynamic_result_survives_an_external_snapshot() {
    let compiled = compile_module_with_options(
        "dynamic",
        &SourceFile::new("dynamic.lm", "[1, 2, 3]\n"),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().dynamic_result(),
    )
    .expect("the dynamic source compiles");
    let bytes = lm_bytecode::encode(&compiled.module);
    let loaded = load_bytes(&bytes).expect("the dynamic program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(DynValue([1, 2, 3]))");

    let gate = world.next_gate();
    let captured = world
        .capture_snapshot(gate, 0, false)
        .expect("the dynamic result captures");
    let admitted = codec::load_external(
        captured.bytes().expect("the snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the dynamic snapshot admits");
    let mut restored = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the dynamic image restores");
    match restored.run_machine(root) {
        RootEvent::Done(value) => {
            assert_eq!(restored.show_result_of(root, value), "DynValue([1, 2, 3])");
        }
        event => panic!("the restored result stopped: {event:?}"),
    }
}

#[test]
fn public_syntax_survives_an_external_snapshot() {
    let source = r#"
def execute(): Bool with Reflect.ParseSyntax
  text = "40 + 2\n"
  parsed = sys.reflect.parse_syntax(text)
  count = 0
  for _ in Range(0, 1000)
    count = count + 1
  end
  case parsed.status
  in ParseComplete
    parsed.diagnostics.len() == 0 and parsed.tree.root().text() == text
  in ParseIncomplete then false
  in ParseInvalid then false
  end
end

execute()
"#;
    let bytes = compile_to_bytes("snapshot-syntax.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
    world.allow("Reflect").expect("the grant exists");
    for _ in 0..100 {
        match world.step_root() {
            RootEvent::Ran => {}
            RootEvent::Waiting | RootEvent::Blocked => {
                world.poll_blocked();
            }
            event => panic!("the source stopped before capture: {event:?}"),
        }
    }

    let gate = world.next_gate();
    let captured = world
        .capture_snapshot(gate, 0, false)
        .expect("the syntax value captures");
    let admitted = codec::load_external(
        captured.bytes().expect("the snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the syntax snapshot admits");
    let mut restored = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the syntax image restores");
    loop {
        match restored.run_machine(root) {
            RootEvent::Done(value) => {
                assert_eq!(restored.show_result_of(root, value), "true");
                break;
            }
            RootEvent::Ran => {}
            event => panic!("the restored syntax run stopped: {event:?}"),
        }
    }
}
