//! Conditional conformance tests.

use lm_compiler::{compile_module, link, CompileEnv, LinkEnv, LinkUnit};
use lm_source::SourceFile;
use lm_testkit::{compile_text, run_allowed};
use lm_vm::{Vm, VmConfig};

fn run(source: &str) -> Result<String, String> {
    run_allowed("conditional.lm", source, &[])
}

#[test]
fn a_conditional_conformance_uses_its_premise() {
    let source = r#"
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
    "box {self.value.label()}"
  end
end

def label[T: Labeled](value: T): String
  value.label()
end

label(Box(Word()))
"#;
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
            "error[E1053]: the method `next` uses `self`, but interface `Cursor` requires `mut self`"
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
    assert!(
        error.message.contains("outside the conformance"),
        "{error:?}"
    );

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
fn conditional_conformances_cross_module_boundaries() {
    let library_source = r#"
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
    "box {self.value.label()}"
  end
end
"#;
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
            .bind(LinkUnit {
                path: module.path.clone(),
                module: module.module.clone(),
                interface: module.interface.clone(),
            })
            .expect("the module binds");
    }
    let linked = link("app.main", &link_env.freeze()).expect("the program links");
    let loaded = lm_vm::load(linked.module).expect("the program loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(\"box word\")");
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
