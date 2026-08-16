//! Week 8, part one: a module class may inherit a core class.
//!
//! Nothing before week 8 inherited a class of the pinned core image.
//! The proc classes need that path, so these cases prove it on an
//! ordinary core class first.

use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("inherit.lm", source, VmConfig::default()).expect("the program compiles")
}

fn reject(source: &str) -> String {
    run_text("inherit.lm", source, VmConfig::default()).expect_err("the program must reject")
}

/// The subclass keeps the parent fields and answers the parent
/// methods.
#[test]
fn a_module_class_inherits_a_core_class() {
    let source = "class Window < Range\n\
                  \x20 def init(mut self, start: Int, stop: Int)\n\
                  \x20   super.init(start, stop)\n\
                  \x20 end\n\
                  end\n\
                  w = Window(2, 6)\n\
                  (w.len(), w.has(3), w.start, w.stop)\n";
    assert_eq!(run(source), "Done((4, true, 2, 6))");
}

/// A subclass value is valid at the core parent type, and the call
/// dispatches on the runtime class.
#[test]
fn dispatch_through_the_core_parent_type_reaches_the_override() {
    let source = "class Empty < Range\n\
                  \x20 def init(mut self)\n\
                  \x20   super.init(0, 0)\n\
                  \x20 end\n\
                  \x20 def len(self): Int\n\
                  \x20   0 - 1\n\
                  \x20 end\n\
                  end\n\
                  r: Range = Empty()\n\
                  r.len()\n";
    assert_eq!(run(source), "Done(-1)");
}

/// A subclass may add fields after the inherited layout.
#[test]
fn a_subclass_adds_fields_after_the_core_layout() {
    let source = "class Named < Range\n\
                  \x20 label: String = \"r\"\n\
                  \x20 def init(mut self, start: Int, stop: Int)\n\
                  \x20   super.init(start, stop)\n\
                  \x20 end\n\
                  \x20 def show(self): String\n\
                  \x20   \"{self.label}:{self.start}-{self.stop}\"\n\
                  \x20 end\n\
                  end\n\
                  Named(1, 3).show()\n";
    assert_eq!(run(source), "Done(\"r:1-3\")");
}

/// The core classes take the first class indices, so a core parent
/// always precedes its subclass in the verified class table.
#[test]
fn the_core_classes_precede_every_module_class() {
    let module = compile_text(
        "inherit.lm",
        "class Window < Range\n\
         \x20 def init(mut self)\n\
         \x20   super.init(0, 1)\n\
         \x20 end\n\
         end\n\
         Window().len()\n",
    )
    .expect("the module compiles");
    let range = module
        .classes
        .iter()
        .position(|c| c.name == "Range")
        .expect("the core declares Range");
    let window = module
        .classes
        .iter()
        .position(|c| c.name == "Window")
        .expect("the module declares Window");
    assert!(range < window, "a parent precedes its subclass");
    assert_eq!(module.classes[window].parent as usize, range);
    // The whole layout of the parent survives into the subclass.
    assert_eq!(
        module.classes[window].fields[..module.classes[range].fields.len()],
        module.classes[range].fields[..]
    );
}

/// A core enum stays sealed. Naming one as a parent still rejects.
#[test]
fn a_core_enum_is_not_a_parent() {
    let error = reject("class Bad < Option\nend\n1\n");
    assert!(error.contains("E1040"), "{error}");
    assert!(error.contains("sealed enum"), "{error}");
}

/// An unknown parent name still rejects with the same code.
#[test]
fn an_unknown_parent_still_rejects() {
    let error = reject("class Bad < Nope\nend\n1\n");
    assert!(error.contains("E1038"), "{error}");
    assert!(error.contains("unknown parent class"), "{error}");
}

/// The override rules apply to a core parent method as well.
#[test]
fn an_override_of_a_core_method_keeps_the_signature() {
    let error = reject(
        "class Bad < Range\n\
         \x20 def init(mut self)\n\
         \x20   super.init(0, 1)\n\
         \x20 end\n\
         \x20 def len(self, extra: Int): Int\n\
         \x20   extra\n\
         \x20 end\n\
         end\n\
         1\n",
    );
    assert!(error.contains("E1031"), "{error}");
}
