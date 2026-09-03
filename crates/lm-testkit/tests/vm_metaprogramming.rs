//! Reified compiler and VM integration.

use lm_compiler::{compile_module_with_options, core_link_env, CompileEnv, CompileOptions};
use lm_host::CliHost;
use lm_source::SourceFile;
use lm_testkit::{compile_text, compile_to_bytes};
use lm_testkit::{publish_artifact, publish_artifact_bytes};
use lm_vm::snapshot::{LoadLimits, SnapshotFail};
use lm_vm::{RecordingHost, RootEvent, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn run_with_files(source: &str, files: &[(&str, Vec<u8>)]) -> String {
    run_with_files_and_grants(source, files, &[])
}

fn run_with_files_and_grants(
    source: &str,
    files: &[(&str, Vec<u8>)],
    extra_grants: &[&str],
) -> String {
    let bytes = compile_to_bytes("meta.lm", source).expect("the test program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the test program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    for (name, bytes) in files {
        host.borrow_mut().set_file(*name, bytes.clone());
    }
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Compiler.Verify"] {
        world.allow(grant).expect("the grant exists");
    }
    for grant in extra_grants {
        world.allow(grant).expect("the extra grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

fn run_with_compiler(source: &str) -> String {
    let bytes = compile_to_bytes("meta-compiler.lm", source).expect("the test program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the test program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
    for grant in ["Compiler", "Reflect", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn a_fault_exposes_its_source_location_and_trace() {
    let source = r#"
def fail(value: Int): Int
  total = 0
  for number in Range(0, 2)
    total = total + number
  end
  total + 10 / value
end

def call_fail(value: Int): Int
  fail(value)
end

def execute(): Bool with Vm
  image = sys.vm.Vm()
  definition = case image.install(codeof(call_fail))
  in Ok(value) then value
  in Err(_) then return false
  end
  run = case image.activate(definition, args: (0,))
  in Ok(value) then value
  in Err(_) then return false
  end
  case run.run()
  in Ok(_) then false
  in Err(fault)
    case fault.site()
    in None then false
    in Some(site)
      trace = fault.trace()
      mapped = case (site.path, site.range)
      in (Some(path), Some(range))
        path == "meta.lm" and range.len() > 0
      in _ then false
      end
      mapped and
      site.function == site.function and
      site.bytecode_offset >= 0 and
      trace.len() >= 2
    end
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn an_async_policy_fault_keeps_its_perform_location() {
    let source = r#"
def announce(): Int with Io.Write
  sys.io.write(b"hello").expect("the output writes")
  1
end

def execute(): Bool with Vm
  image = sys.vm.Vm()
  definition = case image.install(codeof(announce))
  in Ok(value) then value
  in Err(_) then return false
  end
  run = case image.activate(definition, args: ())
  in Ok(value) then value
  in Err(_) then return false
  end
  case run.run()
  in Ok(_) then false
  in Err(fault)
    case fault.site()
    in None then false
    in Some(site)
      case (site.path, site.range)
      in (Some(path), Some(range))
        path == "meta.lm" and range.len() > 0
      in _ then false
      end
    end
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn a_fault_trace_survives_an_external_snapshot() {
    let source = "def fail(value: Int): Int\n  10 / value\nend\nfail(0)\n";
    let bytes = compile_to_bytes("fault-snapshot.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    assert!(matches!(world.run_machine(0), RootEvent::Fault(_)));
    let original = world.root_fault().expect("the root fault exists").clone();
    assert!(!original.trace.is_empty());

    let gate = world.next_gate();
    let captured = world
        .capture_snapshot(gate, 0, false)
        .expect("the faulted machine captures");
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the snapshot admits");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut restored = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the snapshot restores");
    let restored_fault = restored.fault_of(root).expect("the restored fault exists");
    assert_eq!(restored_fault.trace, original.trace);
}

#[test]
fn a_stripped_fault_keeps_its_function_identity_and_offset() {
    let source = r#"
def fail(value: Int): Int
  10 / value
end

def execute(): Bool with Vm
  image = sys.vm.Vm()
  definition = case image.install(codeof(fail))
  in Ok(value) then value
  in Err(_) then return false
  end
  run = case image.activate(definition, args: (0,))
  in Ok(value) then value
  in Err(_) then return false
  end
  case run.run()
  in Ok(_) then false
  in Err(fault)
    case fault.site()
    in None then false
    in Some(site)
      site.path.is_none() and
      site.range.is_none() and
      site.function == site.function and
      site.bytecode_offset >= 0
    end
  end
end

    execute()
"#;
    let bytes = compile_to_bytes("stripped-fault.lm", source).expect("the program compiles");
    let artifact = lm_bytecode::artifact::decode(&bytes).expect("the artifact decodes");
    let mut module = artifact.root().module().clone();
    module.debug.clear();
    lm_verify::verify_module(&module).expect("the stripped program verifies");
    let artifact =
        lm_testkit::replace_artifact_root(&artifact, module).expect("the stripped artifact builds");
    let bytes = lm_bytecode::artifact::encode(&artifact).expect("the artifact encodes");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant exists");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
}

#[test]
fn portable_function_code_installs_without_a_module_handle() {
    let source = r#"
def execute(): Result[Int, String] with Compiler.Compile, Compiler.Verify, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = sys.compiler.compile(
    "portable-function.lm",
    "portable-function.lm",
    "def add(value: Int): Int\n  value + 1\nend\n0\n",
    env,
    options
  ).map_error() { |error: CompileErrors| error.message }?
  module = artifact.verify().map_error() { |error: CodeError| error.message }?
  code = module.function_code[(Int,), Int]("add").map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  definition = image.install(code).map_error() { |error: CodeError| error.message }?
  run = image.activate(definition, args: (41,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the installed function faulted")
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok(42))");
}

#[test]
fn a_named_loom_function_installs_without_source_text() {
    let source = r#"
def add(value: Int): Int
  value + 1
end

def execute(): Int with Vm
  code = codeof(add)
  image = sys.vm.Vm()
  definition = case image.install(code)
  in Ok(value) then value
  in Err(_) then return -1
  end
  run = case image.activate(definition, args: (41,))
  in Ok(value) then value
  in Err(_) then return -2
  end
  case run.run()
  in Ok(value) then value
  in Err(_) then -3
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(42)");
}

#[test]
fn named_function_bindings_replace_directly() {
    let source = r#"
def rate(value: Int): Int
  value * 2
end

def with_fee(value: Int): Int
  value * 20
end

def call(
  image: Vm,
  binding: FunctionBinding[(Int,), Int],
  value: Int
): Result[Int, String] with Vm
  run = image.activate(binding, args: (value,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(result) then Ok(result)
  in Err(_) then Err("the installed function faulted")
  end
end

def call_target(
  image: Vm,
  target: FunctionDef[(Int,), Int],
  value: Int
): Result[Int, String] with Vm
  run = image.activate(target, args: (value,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(result) then Ok(result)
  in Err(_) then Err("the installed target faulted")
  end
end

def execute(): Result[(Int, Int, Int), String] with Vm
  image = sys.vm.Vm()
  original = image.install(rate).map_error() {
    |error: CodeError| error.message
  }?
  replacement = image.install(with_fee).map_error() {
    |error: CodeError| error.message
  }?
  retained = original.target().map_error() { |error: CodeError| error.message }?
  before = call(image, original, 3)?
  image.replace(original, replacement).map_error() {
    |error: CodeError| error.message
  }?
  Ok((before, call(image, original, 3)?, call_target(image, retained, 3)?))
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(Ok((6, 60, 6)))");
}

#[test]
fn named_code_exposes_its_independent_source_record() {
    let source = r#"
def add(value: Int): Int
  value + 1
end

def execute(): Bool
  portable = codeof(add)
  definition = portable.definition()
  case portable.source()
  in Some(source)
    source.path == "meta.lm" and
    source.syntax.kind() == 6 and
    source.syntax.text().contains("value + 1") and
    source.definition.slots.len() == 1 and
    source.definition.identity.contract_hash == definition.identity.contract_hash and
    source.definition.identity.implementation_hash == definition.identity.implementation_hash and
    source.definition.module_hash == definition.module_hash
  in None then false
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn codeof_preserves_the_selected_source_for_equal_function_bodies() {
    let source = r#"
def first(): Int
  1
end

def second(): Int
  1
end

def execute(): (Bool, Bool, Bool, Bool, Bool, Bool)
  first_source = case codeof(first).source()
  in Some(source) then source.syntax.text().contains("def first")
  in None then false
  end
  second_source = case codeof(second).source()
  in Some(source) then source.syntax.text().contains("def second")
  in None then false
  end
  first_definition = codeof(first).definition()
  second_definition = codeof(second).definition()
  (
    first_source,
    second_source,
    first_definition.identity.qualified_key == "first",
    second_definition.identity.qualified_key == "second",
    first_definition.identity.contract_hash == second_definition.identity.contract_hash,
    first_definition.identity.implementation_hash == second_definition.identity.implementation_hash
  )
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[]),
        "Done((true, true, true, true, true, true))"
    );
}

#[test]
fn loom_edits_named_code_syntax_then_replaces_its_binding() {
    let source = r#"
def add(value: Int): Int
  value + 1
end

def call_add(
  image: Vm,
  function: FunctionBinding[(Int,), Int]
): Result[Int, String] with Vm
  run = image.activate(function, args: (1,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the edited function faulted")
  end
end

def execute(): Result[(Int, Int), String] with Compiler.CompileSyntax, Compiler.Verify, Vm
  portable = codeof(add)
  definition = portable.definition()
  original = case portable.source()
  in Some(source) then source.syntax
  in None then return Err("the function has no syntax")
  end
  children = original.children()
  builder = SyntaxBuilder()
  index = 0
  while index < children.len()
    if children.at(index).text() == "1"
      children.set(index, builder.integer("41"))
    end
    index = index + 1
  end
  edited = original.with_children(children)
  definitions = List[(String, DefinitionSpec)]()
  definitions.push(("add", definition))
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
  artifact = sys.compiler.compile_syntax(
    definition.identity.module_name,
    "edited-add.lm",
    edited,
    env,
    options
  ).map_error() { |error: CompileErrors| error.message }?
  module = artifact.verify().map_error() { |error: CodeError| error.message }?
  code = module.function_code[(Int,), Int]("add").map_error() {
    |error: CodeError| error.message
  }?
  replacement_definition = code.definition()
  if replacement_definition.identity.implementation_hash == definition.identity.implementation_hash
    return Err("the edited body kept its implementation identity")
  end
  if replacement_definition.identity.contract_hash != definition.identity.contract_hash
    return Err("the compatible edit changed its contract identity")
  end
  image = sys.vm.Vm()
  original_binding = image.install(portable).map_error() {
    |error: CodeError| error.message
  }?
  replacement = image.install(code).map_error() {
    |error: CodeError| error.message
  }?
  before = call_add(image, original_binding)?
  image.replace(original_binding, replacement).map_error() {
    |error: CodeError| error.message
  }?
  Ok((before, call_add(image, original_binding)?))
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok((2, 42)))");
}

#[test]
fn a_named_loom_class_becomes_portable_code_without_source_text() {
    let source = r#"
final class Box
  value: Int = 7
end

def execute(): Bool with Vm
  code = codeof(Box)
  has_source = case code.source()
  in Some(source)
    source.syntax.kind() == 4 and source.syntax.text().contains("value: Int = 7")
  in None then false
  end
  image = sys.vm.Vm()
  case image.install(code)
  in Ok(binding)
    case binding.slot()
    in Ok(_) then has_source
    in Err(_) then false
    end
  in Err(_) then false
  end
end

execute()
"#;
    assert_eq!(run_with_files(source, &[]), "Done(true)");
}

#[test]
fn compiled_class_code_returns_replaceable_bindings() {
    let source = r#"
def compile_box(source: String): Result[VerifiedModule, String] with Compiler.Compile, Compiler.Verify
  classes = List[String]()
  classes.push("Box")
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: classes
  )
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  artifact = sys.compiler.compile("box", "box.lm", source, env, options).map_error() {
    |error: CompileErrors| error.message
  }?
  artifact.verify().map_error() { |error: CodeError| error.message }
end

def run_entry(
  image: Vm,
  entry: FunctionBinding[(), Int]
): Result[Int, String] with Vm
  run = image.activate(entry, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the entry faulted")
  end
end

def execute(): Result[(Int, Int), String] with Compiler.Compile, Compiler.Verify, Vm
  first = compile_box("final class Box\n  value: Int = 5\nend\nBox().value\n")?
  second = compile_box("final class Box\n  value: Int = 50\nend\nBox().value\n")?
  first_code = first.class_code("Box").map_error() {
    |error: CodeError| error.message
  }?
  second_code = second.class_code("Box").map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  original = image.install(first_code).map_error() {
    |error: CodeError| error.message
  }?
  replacement = image.install(second_code).map_error() {
    |error: CodeError| error.message
  }?
  instance = original.instance().map_error() { |error: CodeError| error.message }?
  entry = instance.entry_binding[(), Int]().map_error() {
    |error: CodeError| error.message
  }?
  before = run_entry(image, entry)?
  image.replace(original, replacement).map_error() {
    |error: CodeError| error.message
  }?
  Ok((before, run_entry(image, entry)?))
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok((5, 50)))");
}

#[test]
fn direct_class_redefinition_uses_verified_binding_data() {
    let source = r#"
final class Box
  value: Int = 5

  def amount(self): Int
    self.value + 1
  end
end

def read_box(): Int
  Box().amount()
end

def run_read(image: Vm, function: FunctionBinding[(), Int]): Result[Int, String] with Vm
  run = image.activate(function, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the read faulted")
  end
end

def execute(): Result[(Int, Int, String), String] with Compiler.Compile, Compiler.Verify, Vm
  original_code = codeof(Box)
  definition = original_code.definition()
  definitions = List[(String, DefinitionSpec)]()
  definitions.push(("Box", definition))
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
    definition.identity.module_name,
    "unrelated-patch-name.lm",
    "final class Box\n  value: Int = 50\n\n  def amount(self): Int\n    self.value + 10\n  end\nend\n",
    env,
    options
  ).map_error() { |error: CompileErrors| error.message }?
  module = artifact.verify().map_error() { |error: CodeError| error.message }?
  replacement_code = module.class_code("Box").map_error() {
    |error: CodeError| error.message
  }?
  replacement_definition = replacement_code.definition()
  if definition.identity.contract_hash != replacement_definition.identity.contract_hash
    return Err("the compatible class revision changed its contract identity")
  end
  if definition.identity.implementation_hash == replacement_definition.identity.implementation_hash
    return Err("the class revision kept its implementation identity")
  end

  image = sys.vm.Vm()
  original = image.install(original_code).map_error() {
    |error: CodeError| error.message
  }?
  reader = image.install(read_box).map_error() { |error: CodeError| error.message }?
  replacement = image.install(replacement_code).map_error() {
    |error: CodeError| error.message
  }?
  original_instance = original.instance().map_error() {
    |error: CodeError| error.message
  }?
  replacement_instance = replacement.instance().map_error() {
    |error: CodeError| error.message
  }?
  original_amount = original_instance.function_binding[(Box,), Int]("Box.amount").map_error() {
    |error: CodeError| error.message
  }?
  replacement_amount = replacement_instance.function_binding[(Box,), Int]("Box.amount").map_error() {
    |error: CodeError| error.message
  }?
  before = run_read(image, reader)?
  changes = List[SlotChange]()
  changes.push(image.change(original, replacement).map_error() {
    |error: CodeError| error.message
  }?)
  changes.push(image.change(original_amount, replacement_amount).map_error() {
    |error: CodeError| error.message
  }?)
  image.replace_all(changes).map_error() { |error: CodeError| error.message }?
  after = run_read(image, reader)?
  Ok((before, after, definition.identity.qualified_key))
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok((6, 60, \"Box\")))");
}

#[test]
fn direct_class_redefinition_rejects_an_abi_change() {
    let source = r#"
final class Box
  value: Int = 5
end

def execute(): Bool with Compiler.Compile
  definition = codeof(Box).definition()
  definitions = List[(String, DefinitionSpec)]()
  definitions.push(("Box", definition))
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
  case sys.compiler.compile(
    definition.identity.module_name,
    "incompatible-box.lm",
    "final class Box\n  value: Int = 5\n  label: String = \"new\"\nend\n",
    env,
    options
  )
  in Ok(_) then false
  in Err(error) then error.message.len() > 0
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn batch_replacement_is_atomic_and_rejects_stale_changes() {
    let source = r#"
def first(value: Int): Int
  value + 1
end

def second(value: Int): Int
  value * 2
end

def next_first(value: Int): Int
  value + 10
end

def next_second(value: Int): Int
  value * 3
end

def total(value: Int): Int
  first(value) + second(value)
end

def call_total(
  image: Vm,
  function: FunctionBinding[(Int,), Int],
  value: Int
): Result[Int, String] with Vm
  run = image.activate(function, args: (value,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(result) then Ok(result)
  in Err(_) then Err("the total faulted")
  end
end

def execute(): Result[(Int, Int, Bool, Bool, Int), String] with Vm
  image = sys.vm.Vm()
  total_binding = image.install(total).map_error() { |error: CodeError| error.message }?
  first_binding = image.install(first).map_error() { |error: CodeError| error.message }?
  second_binding = image.install(second).map_error() { |error: CodeError| error.message }?
  next_first_binding = image.install(next_first).map_error() {
    |error: CodeError| error.message
  }?
  next_second_binding = image.install(next_second).map_error() {
    |error: CodeError| error.message
  }?
  before = call_total(image, total_binding, 5)?

  stale_first = image.change(first_binding, next_first_binding).map_error() {
    |error: CodeError| error.message
  }?
  pending_second = image.change(second_binding, next_second_binding).map_error() {
    |error: CodeError| error.message
  }?
  image.replace(first_binding, next_first_binding).map_error() {
    |error: CodeError| error.message
  }?
  changes = List[SlotChange]()
  changes.push(pending_second)
  changes.push(stale_first)
  stale_rejected = case image.replace_all(changes)
  in Ok(_) then false
  in Err(_) then true
  end
  after_failed_batch = call_total(image, total_binding, 5)?

  duplicate_second = image.change(second_binding, next_second_binding).map_error() {
    |error: CodeError| error.message
  }?
  duplicates = List[SlotChange]()
  duplicates.push(duplicate_second)
  duplicates.push(duplicate_second)
  duplicate_rejected = case image.replace_all(duplicates)
  in Ok(_) then false
  in Err(_) then true
  end

  fresh_first = image.change(first_binding, next_first_binding).map_error() {
    |error: CodeError| error.message
  }?
  fresh_second = image.change(second_binding, next_second_binding).map_error() {
    |error: CodeError| error.message
  }?
  fresh = List[SlotChange]()
  fresh.push(fresh_first)
  fresh.push(fresh_second)
  image.replace_all(fresh).map_error() { |error: CodeError| error.message }?
  after_batch = call_total(image, total_binding, 5)?
  Ok((before, after_failed_batch, stale_rejected, duplicate_rejected, after_batch))
end

execute()
"#;
    assert_eq!(
        run_with_compiler(source),
        "Done(Ok((16, 25, true, true, 30)))"
    );
}

#[test]
fn batch_replacement_publishes_function_and_class_targets_together() {
    let source = r#"
final class Box
  value: Int = 5
end

def fee(value: Int): Int
  value + 1
end

def next_fee(value: Int): Int
  value + 10
end

def price(): Int
  Box().value + fee(0)
end

def run_price(
  image: Vm,
  function: FunctionBinding[(), Int]
): Result[Int, String] with Vm
  run = image.activate(function, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the price function faulted")
  end
end

def execute(): Result[(Int, Int), String] with Compiler.Compile, Compiler.Verify, Vm
  portable_box = codeof(Box)
  definition = portable_box.definition()
  definitions = List[(String, DefinitionSpec)]()
  definitions.push(("Box", definition))
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
    definition.identity.module_name,
    "box-batch-revision.lm",
    "final class Box\n  value: Int = 50\nend\n",
    env,
    options
  ).map_error() { |error: CompileErrors| error.message }?
  module = artifact.verify().map_error() { |error: CodeError| error.message }?
  replacement_code = module.class_code("Box").map_error() {
    |error: CodeError| error.message
  }?

  image = sys.vm.Vm()
  price_binding = image.install(price).map_error() {
    |error: CodeError| error.message
  }?
  box_binding = image.install(portable_box).map_error() {
    |error: CodeError| error.message
  }?
  fee_binding = image.install(fee).map_error() {
    |error: CodeError| error.message
  }?
  next_fee_binding = image.install(next_fee).map_error() {
    |error: CodeError| error.message
  }?
  replacement_box = image.install(replacement_code).map_error() {
    |error: CodeError| error.message
  }?
  before = run_price(image, price_binding)?
  changes = List[SlotChange]()
  changes.push(image.change(box_binding, replacement_box).map_error() {
    |error: CodeError| error.message
  }?)
  changes.push(image.change(fee_binding, next_fee_binding).map_error() {
    |error: CodeError| error.message
  }?)
  image.replace_all(changes).map_error() { |error: CodeError| error.message }?
  Ok((before, run_price(image, price_binding)?))
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok((6, 60)))");
}

#[test]
fn an_enum_case_keeps_its_shared_definition_source() {
    let source = r#"
def execute(): Bool with Compiler.Compile, Compiler.Verify, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile(
    "choice.lm",
    "choice.lm",
    "enum Choice\n  First(value: Int)\n  Second(value: Int)\nend\n",
    env,
    options
  )
  in Ok(value) then value
  in Err(_) then return false
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_) then return false
  end
  code = case module.class_code("Choice.Second")
  in Ok(value) then value
  in Err(_) then return false
  end
  case code.source()
  in Some(source)
    source.syntax.kind() == 5 and source.syntax.text().contains("Second(value: Int)")
  in None then false
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn codeof_rejects_a_local_function_value() {
    let source = r#"
value = { |x: Int| x + 1 }
codeof(value)
"#;
    let error = compile_to_bytes("local-codeof.lm", source)
        .expect_err("a local function value is not portable code");
    assert!(error.contains("error[E1026]"));
    assert!(error.contains("cannot reify a local function value"));
}

#[test]
fn codeof_rejects_an_unapplied_generic_function() {
    let source = r#"
def identity[T](value: T): T
  value
end

codeof(identity)
"#;
    let error = compile_to_bytes("generic-codeof.lm", source)
        .expect_err("an unapplied generic function is not portable code");
    assert!(error.contains("error[E1026]"));
    assert!(error.contains("needs a monomorphic function"));
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
    let mut module =
        lm_testkit::compile_module_text("native-code-forgery.lm", "sys.vm.artifact(Bytes())\n")
            .expect("the seed program compiles");
    let class = module.core_roles[lm_bytecode::corepin::ROLE_ARTIFACT];
    let entry = module.entry as usize;
    module.funcs[entry].blocks = vec![vec![
        lm_bytecode::Instr::New(class),
        lm_bytecode::Instr::Return,
    ]];
    let error = lm_verify::verify_module(&module).expect_err("the verifier rejects the forgery");
    assert!(
        error.message.contains("native core class"),
        "{}",
        error.message
    );
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
def execute(): Int with Compiler.CompileSyntax, Compiler.Verify, Vm
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
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile_syntax("syntax", "syntax.lm", syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return -1
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -2
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -3
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -4
  end
  case image.activate(entry, args: ())
  in Err(_) then -5
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -5
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
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  case sys.compiler.compile_syntax("syntax", "syntax.lm", syntax, env, options)
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
def execute(): Int with Compiler.CompileSyntax, Compiler.Verify, Reflect.ParseSyntax, Vm
  parsed = sys.reflect.parse_syntax("40 + 2\n")
  syntax = case parsed.status
  in ParseComplete then parsed.tree.root()
  in ParseIncomplete
    return -2
  in ParseInvalid
    return -3
  end
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile_syntax("syntax", "syntax.lm", syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return -4
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -5
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -6
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -7
  end
  case image.activate(entry, args: ())
  in Err(_) then -8
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -8
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
def execute(): Bool with Compiler.CompileSyntax, Compiler.Verify, Reflect.ParseSyntax, Vm
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
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: true,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile_syntax("syntax", "syntax.lm", syntax, env, options)
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
  entry = case instance.dynamic_entry()
  in Ok(value) then value
  in Err(_)
    return false
  end
  case image.activate(entry, args: ())
  in Err(_) then false
  in Ok(run)
    case run.run()
    in Ok(value) then value.render() == "[1, 2, 3]"
    in Err(_) then false
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
def execute(): Int with Compiler.CompileSyntax, Compiler.Verify, Reflect.ParseSyntax, Vm
  parsed = sys.reflect.parse_syntax(
    "def add(value: Int): Int\n  value + 2\nend\n"
  )
  syntax = case parsed.status
  in ParseComplete then parsed.tree.root()
  in ParseIncomplete
    return -2
  in ParseInvalid
    return -3
  end
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile_syntax("syntax", "syntax.lm", syntax, env, options)
  in Ok(value) then value
  in Err(_)
    return -4
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -5
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -6
  end
  function = case instance.function[(Int,), Int]("add")
  in Ok(value) then value
  in Err(_)
    return -7
  end
  case image.activate(function, args: (40,))
  in Err(_) then -8
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -8
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
def execute(): Int with Compiler.Compile, Compiler.Verify, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = case sys.compiler.compile("runtime", "runtime.lm", "40 + 2\n", env, options)
  in Ok(value) then value
  in Err(_)
    return -1
  end
  module = case artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -2
  end
  image = sys.vm.Vm()
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -3
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -4
  end
  case image.activate(entry, args: ())
  in Err(_) then -5
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -5
    end
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(42)");
}

#[test]
fn stable_slot_specs_survive_declaration_reordering() {
    let source = r#"
def compile_revision(
  source: String,
  functions: List[String]
): Result[VerifiedModule, String] with Compiler.Compile, Compiler.Verify
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: functions,
    late_classes: List[String]()
  )
  artifact = sys.compiler.compile("revision", "revision.lm", source, env, options).map_error() {
    |error: CompileErrors| error.message
  }?
  artifact.verify().map_error() { |error: CodeError| error.message }
end

def run_entry(image: Vm, entry: FunctionDef[(), Int]): Result[Int, String] with Vm
  run = image.activate(entry, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the run faulted")
  end
end

def execute(): Result[(Int, Int), String] with Compiler.Compile, Compiler.Verify, Vm
  first_functions = List[String]()
  first_functions.push("add")
  first_module = compile_revision(
    "def add(value: Int): Int\n  value + 100\nend\nadd(0)\n",
    first_functions
  )?

  second_functions = List[String]()
  second_functions.push("step")
  second_functions.push("add")
  second_module = compile_revision(
    "def step(value: Int): Int\n  value + 2\nend\ndef add(value: Int): Int\n  value + 1\nend\nstep(add(0))\n",
    second_functions
  )?

  third_functions = List[String]()
  third_functions.push("add")
  third_module = compile_revision(
    "def add(value: String): String\n  value\nend\nadd(\"x\")\n",
    third_functions
  )?

  image = sys.vm.Vm()
  second = image.install(second_module).map_error() {
    |error: CodeError| error.message
  }?
  first = image.install(first_module).map_error() {
    |error: CodeError| error.message
  }?
  third = image.install(third_module).map_error() {
    |error: CodeError| error.message
  }?
  entry = second.entry[(), Int]().map_error() {
    |error: CodeError| error.message
  }?
  before = run_entry(image, entry)?

  spec = first.slot_spec("add").map_error() {
    |error: CodeError| error.message
  }?
  case first.slot_spec("missing")
  in Ok(_) then return Err("an unknown name found a slot")
  in Err(_) then ()
  end
  incompatible = third.slot_spec("add").map_error() {
    |error: CodeError| error.message
  }?
  case second.slot_for(incompatible)
  in Ok(_) then return Err("an incompatible slot matched")
  in Err(_) then ()
  end
  slot = second.slot_for(spec).map_error() {
    |error: CodeError| error.message
  }?
  target = first.function[(Int,), Int]("add").map_error() {
    |error: CodeError| error.message
  }?
  image.replace(slot, target).map_error() {
    |error: CodeError| error.message
  }?

  after = run_entry(image, entry)?
  Ok((before, after))
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(Ok((3, 102)))");
}

#[test]
fn runtime_compilation_links_an_explicit_provider_instance() {
    let source = r#"
def execute(): Int with Compiler.Compile, Compiler.Verify, Vm
  empty_env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  library_options = CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  library_artifact = case sys.compiler.compile(
    "dep",
    "dep.lm",
    "def add(value: Int): Int\n  value + 2\nend\n",
    empty_env,
    library_options
  )
  in Ok(value) then value
  in Err(_)
    return -1
  end
  library_module = case library_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -2
  end

  image = sys.vm.Vm()
  library_instance = case image.install(library_module)
  in Ok(value) then value
  in Err(_)
    return -3
  end

  program_env = CompileEnv(
    [library_module],
    [("dep", "dep")],
  List[(String, DefinitionSpec)]()
  )
  program_options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  program_artifact = case sys.compiler.compile(
    "app",
    "app.lm",
    "use dep\ndep.add(40)\n",
    program_env,
    program_options
  )
  in Ok(value) then value
  in Err(_)
    return -4
  end
  program_module = case program_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -5
  end
  links = LinkEnv([library_instance])
  program_instance = case image.install(program_module, links)
  in Ok(value) then value
  in Err(_)
    return -6
  end
  entry = case program_instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -7
  end
  case image.activate(entry, args: ())
  in Err(_) then -8
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -8
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
def execute(): Int with Compiler.Compile, Compiler.Verify, Vm
  empty = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  library_options = CompileOptions(
    is_main: false,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  library_artifact = case sys.compiler.compile(
    "dep",
    "dep.lm",
    "def add(value: Int): Int\n  value + 2\nend\n",
    empty,
    library_options
  )
  in Ok(value) then value
  in Err(_)
    return -1
  end
  library_module = case library_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -2
  end

  image = sys.vm.Vm()
  library_instance = case image.install(library_module)
  in Ok(value) then value
  in Err(_)
    return -3
  end
  program_env = CompileEnv(
    [library_module],
    [("dep", "dep")],
    List[(String, DefinitionSpec)]()
  )
  program_options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  program_artifact = case sys.compiler.compile(
    "app",
    "app.lm",
    "use dep\ndep.add(40)\n",
    program_env,
    program_options
  )
  in Ok(value) then value
  in Err(_)
    return -4
  end
  program_module = case program_artifact.verify()
  in Ok(value) then value
  in Err(_)
    return -5
  end
  program_instance = case image.install(program_module, LinkEnv([library_instance]))
  in Ok(value) then value
  in Err(_)
    return -6
  end
  entry = case program_instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -7
  end
  count = 0
  for _ in Range(0, 1000)
    count = count + 1
  end
  case image.activate(entry, args: ())
  in Err(_) then -8
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -8
    end
  end
end

execute()
"#;
    let bytes =
        compile_to_bytes("snapshot-runtime-links.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
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
            Ok(image)
                if image
                    .world()
                    .vm_images
                    .iter()
                    .any(|record| record.instances.len() == 2) =>
            {
                captured = Some(image);
                break;
            }
            Ok(_) | Err(SnapshotFail::ResourceActive { .. }) => {}
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows both installations");
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the linked snapshot admits");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut restored = World::new(
        arena,
        namespace,
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
    List[(String, String)](),
  List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  case sys.compiler.compile("broken", "broken.lm", "def", env, options)
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
  List[(String, String)](),
List[(String, DefinitionSpec)]()
)
options = CompileOptions(
  is_main: true,
  dynamic_result: false,
  late_definitions: false,
  late_functions: List[String](),
  late_classes: List[String]()
)
sys.compiler.compile("blocked", "blocked.lm", "1\n", env, options)
"#;
    let bytes = compile_to_bytes("blocked-compiler.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(format!("{outcome:?}"), "Fault(PolicyDenied)");
}

#[test]
fn loom_verifies_installs_and_activates_an_artifact() {
    let artifact = compile_to_bytes("installed.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open(Path("installed.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  artifact = sys.vm.artifact(artifact_bytes())
  case artifact.verify()
  in Err(_) then -1
  in Ok(module)
    image = sys.vm.Vm()
    case image.install(module)
    in Err(_) then -2
    in Ok(instance)
      case instance.dynamic_entry()
      in Ok(_) then return -3
      in Err(_) then ()
      end
      case instance.entry[(), Int]()
      in Err(_) then -3
      in Ok(entry)
        case image.activate(entry, args: ())
        in Err(_) then -4
        in Ok(run)
          case run.run()
          in Ok(value) then value
          in Err(_) then -4
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
    lm_testkit::encode_compiled_artifact(compiled).expect("the revision artifact encodes")
}

#[test]
fn a_replaced_function_fault_uses_the_new_source_revision() {
    let first = revision_artifact("value + 1");
    let second = revision_artifact("10 / (value - 1)");
    let source = r#"
def read_module(path: String): Result[VerifiedModule, String] with Fs.Open, Fs.Read, Fs.Close, Vm.Artifact, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes).verify().map_error() { |error: CodeError| error.message }
end

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Compiler.Verify, Vm
  first_module = case read_module("first.lmbc")
  in Ok(module) then module
  in Err(_) then return false
  end
  second_module = case read_module("second.lmbc")
  in Ok(module) then module
  in Err(_) then return false
  end
  first_source = case first_module.function_code[(Int,), Int]("step")
  in Err(_) then return false
  in Ok(code)
    case code.source()
    in None then return false
    in Some(source) then source
    end
  end
  second_source = case second_module.function_code[(Int,), Int]("step")
  in Err(_) then return false
  in Ok(code)
    case code.source()
    in None then return false
    in Some(source) then source
    end
  end
  image = sys.vm.Vm()
  first = case image.install(first_module)
  in Ok(instance) then instance
  in Err(_) then return false
  end
  second = case image.install(second_module)
  in Ok(instance) then instance
  in Err(_) then return false
  end
  entry = case first.entry[(), Int]()
  in Ok(value) then value
  in Err(_) then return false
  end
  spec = case first.slot_spec("step")
  in Ok(value) then value
  in Err(_) then return false
  end
  slot = case first.slot_for(spec)
  in Ok(value) then value
  in Err(_) then return false
  end
  target = case second.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_) then return false
  end
  case image.replace(slot, target)
  in Err(_) then return false
  in Ok(_) then ()
  end
  run = case image.activate(entry, args: ())
  in Ok(value) then value
  in Err(_) then return false
  end
  case run.run()
  in Ok(_) then false
  in Err(fault)
    case fault.site()
    in None then false
    in Some(site)
      site.function == second_source.definition.identity.implementation_hash and
      site.function != first_source.definition.identity.implementation_hash
    end
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("first.lmbc", first), ("second.lmbc", second)],),
        "Done(true)"
    );
}

fn class_revision_artifact(default: i64, increment: i64) -> Vec<u8> {
    let source = format!(
        "final class Box\n  value: Int = {default}\n  def init(mut self)\n    \
         self.value = self.value + {increment}\n  end\nend\nBox().value\n"
    );
    let compiled = compile_module_with_options(
        "class-revision",
        &SourceFile::new("class-revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_class("Box"),
    )
    .expect("the class revision compiles");
    lm_testkit::encode_compiled_artifact(compiled).expect("the class artifact encodes")
}

fn proc_class_revision_artifact(default: i64, increment: i64) -> Vec<u8> {
    let source = format!(
        r#"class Worker < Proc
  value: Int = {default}

  def init(mut self)
    self.value = self.value + {increment}
  end

  def on_spawn(self): Int
    self.value
  end
end

worker = Worker.spawn()
worker.pause()
worker
"#
    );
    let compiled = compile_module_with_options(
        "proc-class-revision",
        &SourceFile::new("proc-class-revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_class("Worker"),
    )
    .expect("the proc class revision compiles");
    lm_testkit::encode_compiled_artifact(compiled).expect("the proc class artifact encodes")
}

fn proc_revision_artifact(body: &str) -> Vec<u8> {
    let source = format!(
        r#"class Worker < Proc
  def on_spawn(self): Int
    step(21)
  end
end

def step(value: Int): Int
  {body}
end

worker = Worker.spawn()
worker.pause()
worker
"#
    );
    let compiled = compile_module_with_options(
        "proc-revision",
        &SourceFile::new("proc-revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_function("step"),
    )
    .expect("the proc revision compiles");
    lm_testkit::encode_compiled_artifact(compiled).expect("the proc artifact encodes")
}

fn live_proc_revision_artifact(body: &str) -> Vec<u8> {
    let source = format!(
        r#"class Worker < Proc[(Int, Handle[Int, Int])]
  def on_spawn(self): Int with Proc
    loop do
      case self.receive()
      in Msg((value, reply))
        reply.send(step(value))
      in Closed
        return 0
      end
    end
  end
end

def step(value: Int): Int
  {body}
end

worker = Worker.spawn()
worker.pause()
worker
"#
    );
    let compiled = compile_module_with_options(
        "live-proc-revision",
        &SourceFile::new("live-proc-revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_function("step"),
    )
    .expect("the live proc revision compiles");
    lm_testkit::encode_compiled_artifact(compiled).expect("the live proc artifact encodes")
}

fn complete_slot_artifact() -> Vec<u8> {
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
        .expect("the class is exported");
    assert!(module
        .slots
        .iter()
        .any(|slot| slot.initial == Some(lm_bytecode::SlotTarget::Function(step))));
    assert!(module.slots.iter().any(|slot| slot.initial
        == Some(lm_bytecode::SlotTarget::Class {
            class: class.def,
            constructor: class.ctor,
        })));
    let int = module
        .types
        .iter()
        .position(|ty| *ty == lm_bytecode::BcType::Int)
        .expect("the Int type exists") as u32;
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::ad_hoc_slot_key("slot-kinds.value"),
        binding: "slot-kinds.value".to_string(),
        late: true,
        contract_hash: [0; 32],
        contract: lm_bytecode::SlotContract::Value { ty: int },
        initial: None,
    });
    module.slots.push(lm_bytecode::SlotSpec {
        key: lm_bytecode::ad_hoc_slot_key("slot-kinds.process"),
        binding: "slot-kinds.process".to_string(),
        late: true,
        contract_hash: [0; 32],
        contract: lm_bytecode::SlotContract::Process {
            message: int,
            result: int,
        },
        initial: None,
    });
    lm_verify::verify_module(&module).expect("the complete slot artifact verifies");
    lm_testkit::encode_artifact_with_core_from_module("slot-kinds", module)
        .expect("the complete slot artifact encodes")
}

#[test]
fn a_slot_replacement_changes_later_calls_only() {
    let first = revision_artifact("value + 1");
    let second = revision_artifact("value + 10");
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
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

def execute(): (Int, Int) with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  image = sys.vm.Vm()
  first_module = case read_artifact("first.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (-1, -1)
  end
  second_module = case read_artifact("second.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (-2, -2)
  end
  first = case image.install(first_module)
  in Ok(instance) then instance
  in Err(_)
    return (-3, -3)
  end
  second = case image.install(second_module)
  in Ok(instance) then instance
  in Err(_)
    return (-4, -4)
  end
  entry = case first.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return (-5, -5)
  end
  before_run = case image.activate(entry, args: ())
  in Ok(run) then run
  in Err(_)
    return (-6, -6)
  end
  before = case before_run.run()
  in Ok(value) then value
  in Err(_)
    return (-6, -6)
  end
  spec = case first.slot_spec("step")
  in Ok(value) then value
  in Err(_)
    return (-7, -7)
  end
  slot = case first.slot_for(spec)
  in Ok(value) then value
  in Err(_)
    return (-7, -7)
  end
  target = case second.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return (-8, -8)
  end
  case image.replace(slot, target)
  in Err(_)
    return (-9, -9)
  in Ok(_)
    after = case image.activate(entry, args: ())
    in Err(_) then -10
    in Ok(run)
      case run.run()
      in Ok(value) then value
      in Err(_) then -10
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
fn a_class_replacement_changes_future_construction() {
    let first = class_revision_artifact(5, 1);
    let second = class_revision_artifact(50, 2);
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
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

def run_entry(image: Vm, entry: FunctionDef[(), Int]): Int with Vm
  case image.activate(entry, args: ())
  in Err(_) then -20
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -21
    end
  end
end

def execute(): (Int, Int, Int, Int) with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  image = sys.vm.Vm()
  first_module = case read_artifact("first-class.lmbc").verify()
  in Ok(module) then module
  in Err(_) then return (-1, -1, -1, -1)
  end
  second_module = case read_artifact("second-class.lmbc").verify()
  in Ok(module) then module
  in Err(_) then return (-2, -2, -2, -2)
  end
  first_instance = case image.install(first_module)
  in Ok(instance) then instance
  in Err(_) then return (-3, -3, -3, -3)
  end
  second_instance = case image.install(second_module)
  in Ok(instance) then instance
  in Err(_) then return (-4, -4, -4, -4)
  end
  first_entry = case first_instance.entry[(), Int]()
  in Ok(entry) then entry
  in Err(_) then return (-5, -5, -5, -5)
  end
  second_entry = case second_instance.entry[(), Int]()
  in Ok(entry) then entry
  in Err(_) then return (-6, -6, -6, -6)
  end
  before = run_entry(image, first_entry)
  second_own = run_entry(image, second_entry)
  spec = case first_instance.slot_spec("Box")
  in Ok(value) then value
  in Err(_) then return (before, second_own, -7, -7)
  end
  slot = case first_instance.slot_for(spec)
  in Ok(value) then value
  in Err(_) then return (before, second_own, -8, -8)
  end
  target = case second_instance.class_def("Box")
  in Ok(value) then value
  in Err(_) then return (before, second_own, -9, -9)
  end
  case image.replace_class(slot, target)
  in Err(_) then (before, second_own, -10, -10)
  in Ok(_)
    after = run_entry(image, first_entry)
    pending = case image.activate(first_entry, args: ())
    in Ok(run) then run
    in Err(_) then return (before, second_own, after, -11)
    end
    snapshot = case pending.snapshot()
    in Ok(value) then value
    in Err(_) then return (before, second_own, after, -12)
    end
    restored = case sys.vm.Vm().restore(snapshot)
    in Ok(run) then run
    in Err(_) then return (before, second_own, after, -13)
    end
    restored_value = case restored.run()
    in Ok(value) then value
    in Err(_) then -14
    end
    (before, second_own, after, restored_value)
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(
            source,
            &[("first-class.lmbc", first), ("second-class.lmbc", second),],
        ),
        "Done((6, 6, 52, 52))"
    );
}

#[test]
fn a_class_replacement_changes_future_proc_construction() {
    let first = proc_class_revision_artifact(5, 1);
    let second = proc_class_revision_artifact(50, 2);
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
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

def start_worker(
  image: Vm,
  instance: Instance
): Result[Handle[Never, Int], String] with Vm, Proc
  entry = instance.entry[(), Handle[Never, Int]]().map_error() {
    |error: CodeError| error.message
  }?
  run = image.activate(entry, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  case run.run()
  in Ok(worker) then Ok(worker)
  in Err(_) then Err("the worker entry faulted")
  end
end

def finish_worker(worker: Handle[Never, Int]): Result[Int, String] with Proc
  case worker.resume()
  in Ok(_) then ()
  in Err(_) then return Err("the worker did not resume")
  end
  case worker.done()
  in Ok(result) then Ok(result)
  in Err(_) then Err("the worker faulted")
  end
end

def execute(): Result[(Int, Int), String] with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
  first_module = read_artifact("first-proc-class.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  second_module = read_artifact("second-proc-class.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  first_instance = image.install(first_module).map_error() {
    |error: CodeError| error.message
  }?
  second_instance = image.install(second_module).map_error() {
    |error: CodeError| error.message
  }?

  before = finish_worker(start_worker(image, first_instance)?)?
  spec = first_instance.slot_spec("Worker").map_error() {
    |error: CodeError| error.message
  }?
  slot = first_instance.slot_for(spec).map_error() {
    |error: CodeError| error.message
  }?
  target = second_instance.class_def("Worker").map_error() {
    |error: CodeError| error.message
  }?
  image.replace_class(slot, target).map_error() {
    |error: CodeError| error.message
  }?
  after = finish_worker(start_worker(image, first_instance)?)?
  Ok((before, after))
end

execute()
"#;
    assert_eq!(
        run_with_files_and_grants(
            source,
            &[
                ("first-proc-class.lmbc", first),
                ("second-proc-class.lmbc", second),
            ],
            &["Proc"],
        ),
        "Done(Ok((6, 52)))"
    );
}

#[test]
fn an_image_proc_uses_initial_and_replaced_slot_targets() {
    let first = proc_revision_artifact("value + 1");
    let second = proc_revision_artifact("value + 100");
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
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

def start_worker(
  image: Vm,
  instance: Instance
): Result[Handle[Never, Int], String] with Vm, Proc
  entry = instance.entry[(), Handle[Never, Int]]().map_error() {
    |error: CodeError| error.message
  }?
  run = image.activate(entry, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  case run.run()
  in Ok(worker) then Ok(worker)
  in Err(_) then Err("the worker entry faulted")
  end
end

def finish_worker(worker: Handle[Never, Int]): Result[Int, String] with Proc
  case worker.resume()
  in Ok(_) then ()
  in Err(_) then return Err("the worker did not resume")
  end
  case worker.done()
  in Ok(result) then Ok(result)
  in Err(_) then Err("the worker faulted")
  end
end

def execute(): Result[(Int, Int), String] with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
  first_module = read_artifact("first-proc.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  second_module = read_artifact("second-proc.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  first_instance = image.install(first_module).map_error() {
    |error: CodeError| error.message
  }?
  second_instance = image.install(second_module).map_error() {
    |error: CodeError| error.message
  }?

  initial_worker = start_worker(image, first_instance)?
  initial = finish_worker(initial_worker)?

  upgraded_worker = start_worker(image, first_instance)?
  snapshot = case image.snapshot()
  in Ok(value) then value
  in Err(_) then return Err("the VM did not capture")
  end
  restored = case sys.vm.restore_vm(snapshot)
  in Ok(value) then value
  in Err(_) then return Err("the VM did not restore")
  end
  case restored.snapshot()
  in Ok(_) then ()
  in Err(_) then return Err("the restored VM did not capture")
  end

  spec = first_instance.slot_spec("step").map_error() {
    |error: CodeError| error.message
  }?
  slot = first_instance.slot_for(spec).map_error() {
    |error: CodeError| error.message
  }?
  target = second_instance.function[(Int,), Int]("step").map_error() {
    |error: CodeError| error.message
  }?
  image.replace_function(slot, target).map_error() {
    |error: CodeError| error.message
  }?

  upgraded = finish_worker(upgraded_worker)?
  Ok((initial, upgraded))
end

execute()
"#;
    let bytes = compile_to_bytes("image-proc-host.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("first-proc.lmbc", first.clone());
    host.borrow_mut()
        .set_file("second-proc.lmbc", second.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Proc", "Compiler.Verify"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Ok((22, 121)))");

    let image = world.last_snapshot().expect("the restored VM captures");
    let full_vm = image.world().full_vm.expect("the snapshot names one VM");
    let procs: Vec<_> = image
        .world()
        .machines
        .iter()
        .filter(|machine| machine.is_proc)
        .collect();
    assert!(!procs.is_empty());
    assert!(procs.iter().all(|machine| machine.image == Some(full_vm)));
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        image.bytes().expect("the proc snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the proc snapshot admits");
    assert!(admitted
        .world()
        .machines
        .iter()
        .filter(|machine| machine.is_proc)
        .all(|machine| machine.image == Some(full_vm)));
}

#[test]
fn a_paused_live_proc_uses_replaced_function_code() {
    let first = live_proc_revision_artifact("value + 1");
    let second = live_proc_revision_artifact("value + 100");
    let source = r#"
class Collector < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then -1
    end
  end
end

def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path(path, PathStyle.Posix), ReadOnly)
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

def start_worker(
  image: Vm,
  instance: Instance
): Result[Handle[(Int, Handle[Int, Int]), Int], String] with Vm, Proc
  entry = instance.entry[(), Handle[(Int, Handle[Int, Int]), Int]]().map_error() {
    |error: CodeError| error.message
  }?
  run = image.activate(entry, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  case run.run()
  in Ok(worker) then Ok(worker)
  in Err(_) then Err("the worker entry faulted")
  end
end

def ask(
  worker: Handle[(Int, Handle[Int, Int]), Int],
  value: Int
): Result[Int, String] with Proc
  collector = Collector.spawn()
  case worker.send((value, collector))
  in Sent then ()
  in Closed then return Err("the worker mailbox closed")
  in Fault(_) then return Err("the worker send faulted")
  end
  case collector.done()
  in Ok(answer) then Ok(answer)
  in Err(_) then Err("the collector faulted")
  end
end

def execute(): Result[(Int, Int), String] with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
  first_module = read_artifact("first-live-proc.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  second_module = read_artifact("second-live-proc.lmbc").verify().map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  first_instance = image.install(first_module).map_error() {
    |error: CodeError| error.message
  }?
  second_instance = image.install(second_module).map_error() {
    |error: CodeError| error.message
  }?
  worker = start_worker(image, first_instance)?
  worker.resume().map_error() { |_: ProcError| "the worker did not resume" }?
  before = ask(worker, 10)?
  case worker.pause()
  in Ok(_) then ()
  in Err(_) then return Err("the worker did not pause after receive")
  end
  spec = first_instance.slot_spec("step").map_error() {
    |error: CodeError| error.message
  }?
  slot = first_instance.slot_for(spec).map_error() {
    |error: CodeError| error.message
  }?
  target = second_instance.function[(Int,), Int]("step").map_error() {
    |error: CodeError| error.message
  }?
  image.replace(slot, target).map_error() { |error: CodeError| error.message }?
  worker.resume().map_error() { |_: ProcError| "the upgraded worker did not resume" }?
  after = ask(worker, 10)?
  worker.close()
  worker.done()
  Ok((before, after))
end

execute()
"#;
    assert_eq!(
        run_with_files_and_grants(
            source,
            &[
                ("first-live-proc.lmbc", first),
                ("second-live-proc.lmbc", second),
            ],
            &["Proc"],
        ),
        "Done(Ok((11, 110)))"
    );
}

#[test]
fn a_source_defined_worker_uses_replaced_named_code() {
    let source = r#"
class Collector < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then -1
    end
  end
end

def rate(value: Int): Int
  value * 2
end

def with_fee(value: Int): Int
  value * 20
end

class Worker < Proc[(Int, Handle[Int, Int])]
  def on_spawn(self): Int with Proc
    loop do
      case self.receive()
      in Msg((value, reply))
        reply.send(rate(value))
      in Closed
        return 0
      end
    end
  end
end

def launch(): Handle[(Int, Handle[Int, Int]), Int] with Proc
  worker = Worker.spawn()
  worker.pause()
  worker
end

def ask(
  worker: Handle[(Int, Handle[Int, Int]), Int],
  value: Int
): Result[Int, String] with Proc
  collector = Collector.spawn()
  case worker.send((value, collector))
  in Sent then ()
  in Closed then return Err("the worker mailbox closed")
  in Fault(_) then return Err("the worker send faulted")
  end
  case collector.done()
  in Ok(answer) then Ok(answer)
  in Err(_) then Err("the collector faulted")
  end
end

def execute(): Result[(Int, Int), String] with Vm, Proc
  image = sys.vm.Vm()
  worker_class = image.install(codeof(Worker)).map_error() {
    |error: CodeError| error.message
  }?
  worker_class.slot().map_error() { |error: CodeError| error.message }?
  service = worker_class.instance().map_error() {
    |error: CodeError| error.message
  }?
  launcher = image.install(launch).map_error() {
    |error: CodeError| error.message
  }?
  original = service.function_binding[(Int,), Int]("rate").map_error() {
    |error: CodeError| error.message
  }?
  replacement = image.install(with_fee).map_error() {
    |error: CodeError| error.message
  }?

  run = image.activate(launcher, args: ()).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  worker = case run.run()
  in Ok(value) then value
  in Err(_) then return Err("the launcher faulted")
  end
  worker.resume().map_error() { |_: ProcError| "the worker did not resume" }?
  before = ask(worker, 10)?
  worker.pause().map_error() { |_: ProcError| "the worker did not pause" }?
  image.replace(original, replacement).map_error() {
    |error: CodeError| error.message
  }?
  worker.resume().map_error() { |_: ProcError| "the worker did not resume again" }?
  after = ask(worker, 10)?
  worker.close()
  worker.done()
  Ok((before, after))
end

execute()
"#;
    assert_eq!(
        run_with_files_and_grants(source, &[], &["Proc"]),
        "Done(Ok((20, 200)))"
    );
}

#[test]
fn cross_vm_definition_activation_returns_a_code_error() {
    let artifact = compile_to_bytes("cross-vm.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path("cross-vm.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
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
fn closure_activation_returns_a_code_error() {
    let source = r#"
def execute(): Bool with Vm
  captured = sys.vm.Vm()
  image = sys.vm.Vm()
  case image.activate(do ||: Int with Vm
    captured.snapshot()
    1
  end, args: ())
  in Ok(_) then false
  in Err(error) then error.message.len() > 0
  end
end

execute()
"#;
    assert_eq!(run_with_compiler(source), "Done(true)");
}

#[test]
fn loom_replaces_every_slot_target_kind() {
    let artifact = complete_slot_artifact();
    let source = r#"
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    7
  end
end

def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path("slot-kinds.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
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
  function_spec = case instance.slot_spec("step")
  in Ok(value) then value
  in Err(_)
    return false
  end
  function_slot = case instance.slot_for(function_spec)
  in Ok(value) then value
  in Err(_)
    return false
  end
  class_spec = case instance.slot_spec("Box")
  in Ok(value) then value
  in Err(_)
    return false
  end
  class_slot = case instance.slot_for(class_spec)
  in Ok(value) then value
  in Err(_)
    return false
  end
  value_spec = case instance.slot_spec("slot-kinds.value")
  in Ok(value) then value
  in Err(_)
    return false
  end
  value_slot = case instance.slot_for(value_spec)
  in Ok(value) then value
  in Err(_)
    return false
  end
  process_spec = case instance.slot_spec("slot-kinds.process")
  in Ok(value) then value
  in Err(_)
    return false
  end
  process_slot = case instance.slot_for(process_spec)
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
"#;
    let bytes = compile_to_bytes("slot-kinds-host.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("slot-kinds.lmbc", artifact.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Proc", "Compiler.Verify"] {
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
fn a_value_change_commits_and_a_stale_change_publishes_nothing() {
    let artifact = complete_slot_artifact();
    let source = r#"
def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path("slot-kinds.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  image = sys.vm.Vm()
  module = case read_artifact().verify()
  in Ok(value) then value
  in Err(_) then return false
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_) then return false
  end
  spec = case instance.slot_spec("slot-kinds.value")
  in Ok(value) then value
  in Err(_) then return false
  end
  slot = case instance.slot_for(spec)
  in Ok(value) then value
  in Err(_) then return false
  end

  first = case image.change_value(slot, 41)
  in Ok(value) then value
  in Err(_) then return false
  end
  first_batch = List[SlotChange]()
  first_batch.push(first)
  case image.replace_all(first_batch)
  in Ok(_) then ()
  in Err(_) then return false
  end

  stale = case image.change_value(slot, 42)
  in Ok(value) then value
  in Err(_) then return false
  end
  case image.replace_value(slot, 43)
  in Ok(_) then ()
  in Err(_) then return false
  end
  stale_batch = List[SlotChange]()
  stale_batch.push(stale)
  stale_rejected = case image.replace_all(stale_batch)
  in Ok(_) then false
  in Err(_) then true
  end
  case image.snapshot()
  in Ok(_) then stale_rejected
  in Err(_) then false
  end
end

execute()
"#;
    let bytes = compile_to_bytes("value-change.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("slot-kinds.lmbc", artifact.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Compiler.Verify"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    let image = world.last_snapshot().expect("the full snapshot exists");
    assert!(image.world().vm_images[0].slots.iter().any(|slot| {
        matches!(
            slot,
            lm_vm::snapshot::ImageSlotTarget::Value(lm_value::Value::Int(43))
        )
    }));
}

#[test]
fn a_process_change_keeps_its_target_and_rejects_a_stale_change() {
    let artifact = complete_slot_artifact();
    let source = r#"
class Worker < Proc[Int]
  answer: Int

  def init(mut self, answer: Int)
    self.answer = answer
  end

  def on_spawn(self): Int with Proc
    self.answer
  end
end

def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path("slot-kinds.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Proc, Compiler.Verify
  image = sys.vm.Vm()
  module = case read_artifact().verify()
  in Ok(value) then value
  in Err(_) then return false
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_) then return false
  end
  spec = case instance.slot_spec("slot-kinds.process")
  in Ok(value) then value
  in Err(_) then return false
  end
  slot = case instance.slot_for(spec)
  in Ok(value) then value
  in Err(_) then return false
  end
  first = Worker.spawn(7)
  second = Worker.spawn(9)
  case first.done()
  in Ok(7) then ()
  in _ then return false
  end
  case second.done()
  in Ok(9) then ()
  in _ then return false
  end

  prepared = case image.change_process(slot, first)
  in Ok(value) then value
  in Err(_) then return false
  end
  first_batch = List[SlotChange]()
  first_batch.push(prepared)
  case image.replace_all(first_batch)
  in Ok(_) then ()
  in Err(_) then return false
  end

  stale = case image.change_process(slot, second)
  in Ok(value) then value
  in Err(_) then return false
  end
  case image.replace_process(slot, first)
  in Ok(_) then ()
  in Err(_) then return false
  end
  stale_batch = List[SlotChange]()
  stale_batch.push(stale)
  stale_rejected = case image.replace_all(stale_batch)
  in Ok(_) then false
  in Err(_) then true
  end
  case image.snapshot()
  in Ok(_) then stale_rejected
  in Err(_) then false
  end
end

execute()
"#;
    let bytes = compile_to_bytes("process-change.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("slot-kinds.lmbc", artifact.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Proc", "Compiler.Verify"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    let image = world.last_snapshot().expect("the full snapshot exists");
    let process = image.world().vm_images[0]
        .slots
        .iter()
        .find_map(|slot| match slot {
            lm_vm::snapshot::ImageSlotTarget::Process { proc, .. } => Some(*proc),
            _ => None,
        })
        .expect("the process slot has a target");
    let terminal = &image.world().machines[process as usize].terminal;
    assert!(
        matches!(
            terminal,
            Some(lm_vm::snapshot::ImageTerminal::Done(lm_value::Value::Int(
                7
            )))
        ),
        "the staged process target changed: {terminal:?}"
    );
}

#[test]
fn loom_captures_and_restores_a_complete_vm() {
    let artifact = compile_to_bytes("full-vm.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def read_artifact(): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  bytes = case sys.fs.open(Path("full-vm.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Bool with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
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
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("full-vm.lmbc", artifact.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Compiler.Verify"] {
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
    assert_eq!(image.world().vm_images[0].instances.len(), 1);
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        image.bytes().expect("the full snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the external full snapshot admits");
    assert_eq!(admitted.world().distinguished, None);
    assert_eq!(admitted.world().full_vm, Some(0));
    assert_eq!(admitted.world().vm_images[0].instances.len(), 1);
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
  case sys.fs.open(Path("seed.lms", PathStyle.Posix), ReadOnly)
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
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut seed = World::new(
        arena,
        namespace,
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
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Compiler.Verify"] {
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
  case sys.fs.open(Path("installed.lmbc", PathStyle.Posix), ReadOnly)
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

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm, Compiler.Verify
  image = sys.vm.Vm()
  module = case sys.vm.artifact(artifact_bytes()).verify()
  in Ok(value) then value
  in Err(_)
    return -1
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return -2
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return -3
  end
  case image.activate(entry, args: ())
  in Err(_) then -4
  in Ok(run)
    case run.run()
    in Ok(value) then value
    in Err(_) then -4
    end
  end
end

execute()
"#;
    let bytes = compile_to_bytes("snapshot-code.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("installed.lmbc", artifact.clone());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm", "Compiler.Verify"] {
        world.allow(grant).expect("the grant exists");
    }
    let initial_gate = world.next_gate();
    let initial = world
        .capture_snapshot(initial_gate, 0, false)
        .expect("the initial program captures");

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
            Ok(image)
                if image
                    .world()
                    .vm_images
                    .iter()
                    .any(|record| !record.instances.is_empty()) =>
            {
                captured = Some(image);
                break;
            }
            Ok(_) | Err(SnapshotFail::ResourceActive { .. }) => {}
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows installation");
    // The root image rides along with the installed image.
    assert_eq!(captured.world().vm_images.len(), 2);
    assert_eq!(
        captured
            .world()
            .vm_images
            .iter()
            .filter(|vm| vm.instances.len() == 1)
            .count(),
        1
    );

    let debugger = compile_to_bytes("debugger.lm", "0\n").expect("the debugger compiles");
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &debugger,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the external snapshot admits");
    let initial = lm_testkit::load_snapshot_for_artifact_bytes(
        &debugger,
        initial.bytes().expect("the initial snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the initial external snapshot admits");
    let (arena, namespace) = publish_artifact_bytes(&debugger).expect("the debugger loads");
    let mut restored = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    restored.allow("Vm").expect("the grant exists");
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the code image restores");
    let initial_target = restored
        .new_child(0)
        .expect("the initial restore target exists");
    restored
        .restore_image(0, initial_target, &initial)
        .expect("the initial image restores after the wider image");
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

const INSTALLED_CHILD_APP: &str = r#"
def execute(): Result[Int, String] with Compiler.Compile, Compiler.Verify, Vm
  env = CompileEnv(
    List[VerifiedModule](),
    List[(String, String)](),
    List[(String, DefinitionSpec)]()
  )
  options = CompileOptions(
    is_main: true,
    dynamic_result: false,
    late_definitions: false,
    late_functions: List[String](),
    late_classes: List[String]()
  )
  artifact = sys.compiler.compile(
    "plugin.lm",
    "plugin.lm",
    "def slow(value: Int): Int\n  i = 0\n  while i < value\n    i = i + 1\n  end\n  i\nend\n0\n",
    env,
    options
  ).map_error() { |error: CompileErrors| error.message }?
  module = artifact.verify().map_error() { |error: CodeError| error.message }?
  code = module.function_code[(Int,), Int]("slow").map_error() {
    |error: CodeError| error.message
  }?
  image = sys.vm.Vm()
  definition = image.install(code).map_error() { |error: CodeError| error.message }?
  run = image.activate(definition, args: (300,)).map_error() {
    |error: CodeError| error.message
  }?
  case run.run()
  in Ok(value) then Ok(value)
  in Err(_) then Err("the installed function faulted")
  end
end

execute()
"#;

fn capture_installed_child_snapshot() -> Vec<u8> {
    let artifact = compile_text("installed-child.lm", INSTALLED_CHILD_APP)
        .expect("the installed-child program compiles");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let base_funcs = arena
        .namespace(namespace)
        .expect("the program namespace exists")
        .table_store()
        .funcs
        .len();
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
    for grant in ["Compiler", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    for _ in 0..200_000 {
        for child in world.machine_ids().into_iter().filter(|id| *id != 0) {
            let gate = world.next_gate();
            if let Ok(image) = world.capture_snapshot(gate, child, false) {
                let stored = image.world();
                let top = stored.machines[0].frames.last().map(|frame| frame.func);
                if stored.namespaces.len() == 1
                    && stored.namespaces[0].artifacts.len() == 2
                    && top.is_some_and(|func| (func as usize) >= base_funcs)
                {
                    return image.bytes().expect("the child snapshot encodes").to_vec();
                }
            }
        }
        match world.step_root() {
            RootEvent::Ran => {}
            RootEvent::Blocked | RootEvent::Waiting => {
                if world.poll_blocked() == 0 && world.wait_host_completion(|_| true).is_none() {
                    panic!("the root stalled before the child capture");
                }
            }
            event => panic!("the root stopped before the child capture: {event:?}"),
        }
    }
    panic!("no installed child reached a snapshot boundary");
}

fn capture_initial_program_snapshot() -> Vec<u8> {
    let artifact = compile_text("installed-child.lm", INSTALLED_CHILD_APP)
        .expect("the installed-child program compiles");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the initial program captures");
    assert_eq!(image.world().namespaces.len(), 1);
    assert_eq!(image.world().namespaces[0].artifacts.len(), 1);
    image
        .bytes()
        .expect("the initial snapshot encodes")
        .to_vec()
}

#[test]
fn a_prefix_restore_keeps_a_registered_child_namespace() {
    let child = capture_installed_child_snapshot();
    let initial = capture_initial_program_snapshot();
    let debugger =
        compile_text("namespace-debugger.lm", "0\n").expect("the debugger program compiles");
    let (arena, namespace) = publish_artifact(&debugger).expect("the debugger publishes");
    let mut config = VmConfig::default();
    config.max_children += 4;
    let mut world = World::new(arena, namespace, config, Box::new(RecordingHost::new(1)));

    let child_image = world
        .load_snapshot_bytes(&child)
        .expect("the child snapshot admits");
    let child_target = world.new_child(0).expect("the child target exists");
    let child_root = world
        .restore_image(0, child_target, &child_image)
        .expect("the child snapshot restores");

    let initial_image = world
        .load_snapshot_bytes(&initial)
        .expect("the initial snapshot admits");
    let initial_target = world.new_child(0).expect("the initial target exists");
    world
        .restore_image(0, initial_target, &initial_image)
        .expect("the initial snapshot restores");

    match world.run_machine(child_root) {
        RootEvent::Done(value) => {
            assert_eq!(world.show_result_of(child_root, value), "300");
        }
        event => panic!("the restored child stopped: {event:?}"),
    }
}

#[test]
fn installed_bindings_survive_an_external_snapshot() {
    let source = r#"
final class Box
end

def add(value: Int): Int
  value + 1
end

def double(value: Int): Int
  value * 2
end

def execute(): Int with Vm
  image = sys.vm.Vm()
  function = case image.install(add)
  in Ok(value) then value
  in Err(_) then return -1
  end
  class_binding = case image.install(codeof(Box))
  in Ok(value) then value
  in Err(_) then return -2
  end
  replacement = case image.install(double)
  in Ok(value) then value
  in Err(_) then return -3
  end
  case image.replace(function, function)
  in Ok(_) then ()
  in Err(_) then return -4
  end
  pending = case image.change(function, replacement)
  in Ok(value) then value
  in Err(_) then return -5
  end
  changes = List[SlotChange]()
  changes.push(pending)
  count = 0
  for _ in Range(0, 1000)
    count = count + 1
  end
  case function.slot()
  in Ok(_) then ()
  in Err(_) then return -6
  end
  case class_binding.slot()
  in Ok(_) then ()
  in Err(_) then return -7
  end
  case image.replace_all(changes)
  in Ok(_) then ()
  in Err(_) then return -8
  end
  run = case image.activate(function, args: (41,))
  in Ok(value) then value
  in Err(_) then return -9
  end
  case run.run()
  in Ok(value) then value
  in Err(_) then -10
  end
end

execute()
"#;
    let bytes = compile_to_bytes("snapshot-bindings.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant exists");

    let mut captured = None;
    for _ in 0..4000 {
        match world.step_root() {
            RootEvent::Ran => {}
            event => panic!("the source stopped before capture: {event:?}"),
        }
        let gate = world.next_gate();
        match world.capture_snapshot(gate, 0, false) {
            Ok(image) => {
                let has_change = image
                    .world()
                    .machines
                    .iter()
                    .flat_map(|machine| &machine.objects)
                    .any(|entry| matches!(&entry.object, lm_heap::Object::NativeSlotChange { .. }));
                let has_version = image
                    .world()
                    .vm_images
                    .iter()
                    .any(|record| record.slot_versions.iter().any(|version| *version > 0));
                if has_change && has_version {
                    captured = Some(image);
                    break;
                }
            }
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows the binding replacement");
    // Each selected definition carries one exact artifact.
    assert_eq!(
        captured
            .world()
            .vm_images
            .iter()
            .map(|vm| vm.instances.len())
            .max(),
        Some(3)
    );
    let kinds: Vec<_> = captured
        .world()
        .machines
        .iter()
        .flat_map(|machine| &machine.objects)
        .filter_map(|entry| match &entry.object {
            lm_heap::Object::NativeCodeHandle { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&lm_heap::CodeHandleKind::FunctionBinding));
    assert!(kinds.contains(&lm_heap::CodeHandleKind::ClassBinding));
    assert!(captured
        .world()
        .machines
        .iter()
        .flat_map(|machine| &machine.objects)
        .any(|entry| matches!(&entry.object, lm_heap::Object::NativeSlotChange { .. })));

    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the external snapshot admits");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut restored = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    restored.allow("Vm").expect("the grant exists");
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the binding image restores");
    restored.allow_on(root, "Vm").expect("the grant exists");
    loop {
        match restored.run_machine(root) {
            RootEvent::Done(value) => {
                assert_eq!(restored.show_result_of(root, value), "82");
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
    let path = compiled.path.clone();
    let mut links = core_link_env().expect("the core link environment builds");
    lm_testkit::bind_compiled_unit(&mut links, compiled).expect("the dynamic module binds");
    let artifact = links
        .freeze()
        .artifact(&path)
        .expect("the dynamic artifact builds");
    let (arena, namespace) =
        lm_testkit::publish_artifact(&artifact).expect("the dynamic program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(DynValue([1, 2, 3]))");

    let gate = world.next_gate();
    let captured = world
        .capture_snapshot(gate, 0, false)
        .expect("the dynamic result captures");
    let admitted = lm_testkit::load_snapshot_for_artifact(
        &artifact,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the dynamic snapshot admits");
    let (arena, namespace) =
        lm_testkit::publish_artifact(&artifact).expect("the dynamic program loads");
    let mut restored = World::new(
        arena,
        namespace,
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
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
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
    let admitted = lm_testkit::load_snapshot_for_artifact_bytes(
        &bytes,
        captured.bytes().expect("the snapshot encodes"),
        LoadLimits::default(),
    )
    .expect("the syntax snapshot admits");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut restored = World::new(
        arena,
        namespace,
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
