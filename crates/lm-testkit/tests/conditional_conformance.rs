//! Conditional conformance tests.

use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
use lm_source::SourceFile;
use lm_testkit::{compile_text, run_allowed};
use lm_vm::{Vm, VmConfig};

fn run(source: &str) -> Result<String, String> {
    run_allowed("conditional.lm", source, &[])
}

fn verify_rejection(module: &lm_bytecode::Module, needle: &str) {
    let bytes = lm_bytecode::encode(module);
    let decoded = lm_bytecode::decode(&bytes).expect("the forged module decodes");
    let error = lm_verify::verify_module(&decoded).expect_err("the verifier rejects the forgery");
    assert!(error.message.contains(needle), "{error:?}");
}

#[test]
fn a_conditional_conformance_uses_its_premise() {
    let source = r##"
interface Labeled
  def label(self): String
end

final class Word implements Labeled
  def label(self): String
    "word"
  end
end

final class Box[T] implements Labeled when T: Labeled
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def label(self): String when T: Labeled
    "box #{self.value.label()}"
  end
end

def label[T: Labeled](value: T): String
  value.label()
end

label(Box(Word()))
"##;
    assert_eq!(run(source).unwrap(), "Done(\"box word\")");
}

#[test]
fn an_unmet_premise_rejects_a_generic_bound() {
    let source = r#"
interface Labeled
  def label(self): String
end

final class Plain
end

final class Box[T] implements Labeled when T: Labeled
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def label(self): String when T: Labeled
    self.value.label()
  end
end

def label[T: Labeled](value: T): String
  value.label()
end

label(Box(Plain()))
"#;
    let error = run(source).expect_err("the premise must reject");
    assert!(error.contains("E1053"), "{error}");
    assert!(error.contains("does not conform to `Labeled`"), "{error}");
    assert!(
        error.contains("because `Plain` does not conform to `Labeled`"),
        "{error}"
    );
}

#[test]
fn an_unmet_method_premise_rejects_a_direct_call() {
    let source = r#"
interface Labeled
  def label(self): String
end

final class Plain
end

final class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def label(self): String when T: Labeled
    self.value.label()
  end
end

Box(Plain()).label()
"#;
    let error = run(source).expect_err("the method premise must reject");
    assert!(error.contains("E1053"), "{error}");
    assert!(error.contains("does not conform to `Labeled`"), "{error}");
}

#[test]
fn a_mutable_interface_receiver_mismatch_has_a_precise_diagnostic() {
    let error = run(r#"
interface Cursor
  def next(mut self): Int
end

final class Fixed implements Cursor
  def next(self): Int
    1
  end
end

Fixed()
"#)
    .expect_err("the receiver mismatch must reject");
    assert!(
        error.contains(
            "error[E1053]: the method `next` does not satisfy interface `Cursor`: the contract requires `mut self`"
        ),
        "{error}"
    );
}

#[test]
fn a_premise_must_name_a_class_type_parameter() {
    let source = r#"
interface Labeled
  def label(self): String
end

final class Box[T] implements Labeled when U: Labeled
  def label(self): String
    "box"
  end
end

Box[Int]()
"#;
    let error = run(source).expect_err("the unknown parameter must reject");
    assert!(error.contains("E1053"), "{error}");
    assert!(error.contains("unknown type parameter `U`"), "{error}");
}

#[test]
fn interface_inheritance_keeps_the_weakest_premise() {
    let source = r#"
interface Named
  def name(self): String
end

interface Labeled: Named
  def label(self): String
end

final class Name implements Named
  def name(self): String
    "name"
  end
end

final class Box[T] implements Labeled when T: Labeled, Named when T: Named
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def name(self): String when T: Named
    self.value.name()
  end

  def label(self): String when T: Labeled
    self.value.label()
  end
end

def name[T: Named](value: T): String
  value.name()
end

name(Box(Name()))
"#;
    assert_eq!(run(source).unwrap(), "Done(\"name\")");
}

#[test]
fn a_conditional_enum_conformance_uses_its_premise() {
    let declarations = r#"
interface Labeled
  def label(self): String
end

final class Word implements Labeled
  def label(self): String
    "word"
  end
end

enum Maybe[T] implements Labeled when T: Labeled
  SomeValue(value: T)
  NoValue

  def label(self): String when T: Labeled
    case self
    in SomeValue(value) then value.label()
    in NoValue then "none"
    end
  end
end

def label[T: Labeled](value: T): String
  value.label()
end
"#;
    let direct = format!("{declarations}\nSomeValue(Word()).label()\n");
    assert_eq!(run(&direct).unwrap(), "Done(\"word\")");
    let generic = format!("{declarations}\nlabel(SomeValue(Word()))\n");
    assert_eq!(run(&generic).unwrap(), "Done(\"word\")");
    let source = format!(
        "{declarations}\nnone: Maybe[Word] = NoValue\n\
         (label(SomeValue(Word())), label(none))\n"
    );
    assert_eq!(run(&source).unwrap(), "Done((\"word\", \"none\"))");
}

#[test]
fn a_conditional_associated_type_resolves_only_after_its_premise() {
    let source = r#"
interface Labeled
  def label(self): String
end

interface Source
  type Item: Labeled
  def item(self): Self.Item
end

final class Word implements Labeled
  def label(self): String
    "word"
  end
end

final class Box[T] implements Source when T: Labeled
  type Item = T
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def item(self): T
    self.value
  end
end

def label_item[S: Source](source: S): String
  source.item().label()
end

label_item(Box(Word()))
"#;
    assert_eq!(run(source).unwrap(), "Done(\"word\")");
}

#[test]
fn an_inherited_conditional_method_never_panics() {
    let source = r#"
interface Labeled
  def label(self): String
end

final class Word implements Labeled
  def label(self): String
    "word"
  end
end

final class Plain
end

class Gate[T]
  def open(self): String when T: Labeled
    "open"
  end
end

final class WordGate < Gate[Word]
end

final class PlainGate < Gate[Plain]
end

WordGate().open()
"#;
    assert_eq!(run(source).unwrap(), "Done(\"open\")");

    let bad = source.replace("WordGate().open()", "PlainGate().open()");
    let error = run(&bad).expect_err("the inherited premise must reject");
    assert!(error.contains("E1026"), "{error}");
}

#[test]
fn artifacts_preserve_and_verify_conformance_premises() {
    let source = r#"
interface Labeled
  def label(self): String
end

final class Box[T] implements Labeled when T: Labeled
  def label(self): String when T: Labeled
    "box"
  end
end

1
"#;
    let module = compile_text("conditional.lm", source).expect("the source compiles");
    let box_class = module
        .classes
        .iter()
        .position(|class| class.name == "Box")
        .expect("Box exists");
    let conformance = module
        .conformances
        .iter()
        .find(|item| item.class as usize == box_class && !item.premises.is_empty())
        .expect("the conditional conformance exists");
    assert_eq!(conformance.premises.len(), 1);
    assert_eq!(conformance.premises[0].param, 0);

    let bytes = lm_bytecode::encode(&module);
    let decoded = lm_bytecode::decode(&bytes).expect("the module decodes");
    assert_eq!(decoded.conformances, module.conformances);
    lm_verify::verify_module(&decoded).expect("the module verifies");

    let original_identity =
        lm_bytecode::identity::module_identity(&module).expect("the module hashes");
    let mut unconditional = module.clone();
    unconditional
        .conformances
        .iter_mut()
        .find(|item| item.class as usize == box_class && !item.premises.is_empty())
        .expect("the conditional conformance exists")
        .premises
        .clear();
    let changed_identity =
        lm_bytecode::identity::module_identity(&unconditional).expect("the module hashes");
    assert_ne!(
        original_identity.class_hashes[box_class],
        changed_identity.class_hashes[box_class]
    );
    let error = lm_verify::verify_module(&unconditional)
        .expect_err("the missing witness premise must reject");
    assert!(error.message.contains("undeclared premise"), "{error:?}");

    let mut malformed = module;
    let premise = malformed
        .conformances
        .iter_mut()
        .find(|item| item.class as usize == box_class && !item.premises.is_empty())
        .expect("the conditional conformance exists");
    premise.premises[0].param = u32::MAX;
    let error = lm_verify::verify_module(&malformed).expect_err("the premise index must reject");
    assert!(error.message.contains("premise parameter"), "{error:?}");
}

#[test]
fn the_verifier_checks_interface_default_witnesses() {
    let mut short = compile_text(
        "defaults.lm",
        "interface Named\n  def name(self): String\n    \"default\"\n  end\nend\n\
         final class Item implements Named\nend\nItem().name()\n",
    )
    .expect("the default program compiles");
    let item = short
        .classes
        .iter()
        .position(|class| class.name == "Item")
        .expect("Item exists") as u32;
    short
        .conformances
        .iter_mut()
        .find(|item_conformance| item_conformance.class == item)
        .expect("Item conforms")
        .method_overrides
        .clear();
    verify_rejection(&short, "method witness table does not match");

    let mut missing = compile_text(
        "required.lm",
        "interface Named\n  def name(self): String\nend\n\
         final class Item implements Named\n\
         \x20 def name(self): String\n    \"item\"\n  end\nend\nItem().name()\n",
    )
    .expect("the required method program compiles");
    let item = missing
        .classes
        .iter()
        .position(|class| class.name == "Item")
        .expect("Item exists") as u32;
    missing
        .conformances
        .iter_mut()
        .find(|item_conformance| item_conformance.class == item)
        .expect("Item conforms")
        .method_overrides[0] = false;
    verify_rejection(&missing, "selects a missing default");

    let mut diamond = compile_text(
        "diamond.lm",
        "interface Left\n  def name(self): String\n    \"left\"\n  end\nend\n\
         interface Right\n  def name(self): String\n    \"right\"\n  end\nend\n\
         final class Item implements Left, Right\n\
         \x20 def name(self): String\n    \"item\"\n  end\nend\nItem().name()\n",
    )
    .expect("the explicit diamond override compiles");
    let item = diamond
        .classes
        .iter()
        .position(|class| class.name == "Item")
        .expect("Item exists") as u32;
    for conformance in diamond
        .conformances
        .iter_mut()
        .filter(|conformance| conformance.class == item)
    {
        conformance.method_overrides[0] = false;
    }
    verify_rejection(
        &diamond,
        "two interface defaults need one explicit class override",
    );
}

#[test]
fn conditional_conformances_cross_module_boundaries() {
    let library_source = r##"
interface Labeled
  def label(self): String
end

final class Word implements Labeled
  def label(self): String
    "word"
  end
end

final class Box[T] implements Labeled when T: Labeled
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def label(self): String when T: Labeled
    "box #{self.value.label()}"
  end
end
"##;
    let library = compile_module(
        "lib.labels",
        &SourceFile::new("labels.lm", library_source),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let exported = library.interface.find("Box").expect("Box is exported");
    let lm_bytecode::interface::IfaceItem::Class(class) = &exported.item else {
        panic!("Box is not a class");
    };
    assert_eq!(class.conformances[0].premises.len(), 1);

    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_interface(library.interface.clone())
        .expect("the interface binds");
    compile_env
        .bind_root("labels", "lib.labels")
        .expect("the root binds");
    let main = compile_module(
        "app.main",
        &SourceFile::new(
            "main.lm",
            "use labels\n\
             def label[T: labels.Labeled](value: T): String\n\
               value.label()\n\
             end\n\
             label(labels.Box(labels.Word()))\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");
    let mut link_env = LinkEnv::new();
    for module in [&library, &main] {
        link_env
            .bind(
                LinkUnit::new(
                    module.path.clone(),
                    module.module.clone(),
                    module.interface.clone(),
                    Vec::new(),
                )
                .expect("the link unit is valid"),
            )
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("the program links");
    let loaded = lm_vm::load(linked.module).expect("the program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(\"box word\")");
}

#[test]
fn interface_defaults_cross_module_boundaries() {
    let library = compile_module(
        "lib.defaults",
        &SourceFile::new(
            "defaults.lm",
            "interface Named\n  def name(self): String\n    \"default\"\n  end\nend\n\
             final class Box implements Named\nend\n",
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the default library compiles");
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_interface(library.interface.clone())
        .expect("the interface binds");
    compile_env
        .bind_root("defaults", "lib.defaults")
        .expect("the root binds");
    let main = compile_module(
        "app.main",
        &SourceFile::new(
            "main.lm",
            "use defaults\n\
             def name[T: defaults.Named](value: T): String\n  value.name()\nend\n\
             \"#{defaults.Box().name()}:#{name(defaults.Box())}\"\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the default caller compiles");
    let mut link_env = LinkEnv::new();
    for module in [&library, &main] {
        link_env
            .bind(
                LinkUnit::new(
                    module.path.clone(),
                    module.module.clone(),
                    module.interface.clone(),
                    Vec::new(),
                )
                .expect("the link unit is valid"),
            )
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("the program links");
    let loaded = lm_vm::load(linked.module).expect("the program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(\"default:default\")");
}

#[test]
fn sparse_interface_witnesses_follow_class_inheritance() {
    let module = compile_text(
        "defaults.lm",
        "interface Named\n  def name(self): String\n    \"default\"\n  end\nend\n\
         class DefaultParent implements Named\nend\n\
         final class DefaultChild < DefaultParent\nend\n\
         class OverrideParent implements Named\n\
         \x20 def name(self): String\n    \"parent\"\n  end\nend\n\
         final class OverrideChild < OverrideParent\nend\n\
         def name[T: Named](value: T): String\n  value.name()\nend\n\
         \"#{name(DefaultChild())}:#{name(OverrideChild())}\"\n",
    )
    .expect("the inherited default program compiles");
    let loaded = lm_vm::load(module).expect("the inherited default program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(\"default:parent\")");
}

#[test]
fn sparse_interface_witnesses_select_multiple_defaults() {
    let module = compile_text(
        "defaults.lm",
        "interface First\n  def first(self): Int\n    1\n  end\nend\n\
         interface Second\n  def second(self): Int\n    2\n  end\nend\n\
         final class Both implements First, Second\nend\n\
         def first[T: First](value: T): Int\n  value.first()\nend\n\
         def second[T: Second](value: T): Int\n  value.second()\nend\n\
         (first(Both()), second(Both()))\n",
    )
    .expect("the multiple default program compiles");
    let loaded = lm_vm::load(module).expect("the multiple default program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done((1, 2))");
}

#[test]
fn a_mutable_interface_method_crosses_a_module_boundary() {
    let library = compile_module(
        "lib.domain",
        &SourceFile::new(
            "domain.lm",
            r#"
interface Aggregate
  def apply(mut self, event: Int)
end

final class Task implements Aggregate
  total: Int = 0

  def init(mut self, total: Int)
    self.total = total
  end

  def apply(mut self, event: Int)
    self.total = self.total + event
  end

  def value(self): Int
    self.total
  end
end
"#,
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_interface(library.interface.clone())
        .expect("the interface binds");
    compile_env
        .bind_root("domain", "lib.domain")
        .expect("the root binds");
    let main = compile_module(
        "app.main",
        &SourceFile::new(
            "main.lm",
            "use domain.Task\n\
             task = Task(7)\n\
             task.apply(9)\n\
             task.value()\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");
    let mut link_env = LinkEnv::new();
    for module in [&library, &main] {
        link_env
            .bind(
                LinkUnit::new(
                    module.path.clone(),
                    module.module.clone(),
                    module.interface.clone(),
                    Vec::new(),
                )
                .expect("the link unit is valid"),
            )
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("the program links");
    let loaded = lm_vm::load(linked.module).expect("the program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(16)");
}

#[test]
fn a_receiver_mismatch_names_the_method_class_and_interface() {
    let mut module = compile_text(
        "receiver.lm",
        r#"
interface Cursor
  def next(mut self): Int
end

final class Counter implements Cursor
  value: Int = 0

  def next(mut self): Int
    self.value = self.value + 1
    self.value
  end
end

Counter()
"#,
    )
    .expect("the source compiles");
    let class = module
        .classes
        .iter()
        .find(|class| class.name == "Counter")
        .expect("Counter exists");
    let (_, function) = class
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == "next")
        .copied()
        .expect("Counter.next exists");
    module.funcs[function as usize].param_muts[0] = false;

    let error = lm_verify::verify_module(&module).expect_err("the mismatch rejects");
    assert!(error.message.contains("the method `next`"), "{error:?}");
    assert!(error.message.contains("of `Counter`"), "{error:?}");
    assert!(error.message.contains("satisfy `Cursor`"), "{error:?}");
    assert!(
        error.message.contains("the contract requires `mut self`"),
        "{error:?}"
    );
}

#[test]
fn frozen_generic_classes_cross_module_boundaries() {
    let library = compile_module(
        "lib.frozen",
        &SourceFile::new(
            "frozen.lm",
            r#"
frozen class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end
end
"#,
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let exported = library.interface.find("Box").expect("Box is exported");
    let lm_bytecode::interface::IfaceItem::Class(class) = &exported.item else {
        panic!("Box is not a class");
    };
    assert!(class.is_frozen);

    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_interface(library.interface.clone())
        .expect("the interface binds");
    compile_env
        .bind_root("frozenlib", "lib.frozen")
        .expect("the root binds");
    let compile_env = compile_env.freeze();
    let valid = compile_module(
        "app.valid",
        &SourceFile::new(
            "valid.lm",
            "use frozenlib\nfrozenlib.Box[String](\"ready\")\n",
        ),
        &compile_env,
        true,
    );
    valid.expect("an always-frozen argument compiles");

    let invalid = compile_module(
        "app.invalid",
        &SourceFile::new(
            "invalid.lm",
            "use frozenlib\nfrozenlib.Box[List[Int]]([])\n",
        ),
        &compile_env,
        true,
    )
    .expect_err("a mutable argument rejects");
    assert!(
        invalid.contains("always-frozen type arguments"),
        "{invalid}"
    );
}
