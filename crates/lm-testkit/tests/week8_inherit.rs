//! Week 8, part one: a module class may inherit a core class.
//!
//! Nothing before week 8 inherited a class of the pinned core image.
//! The proc classes need that path, so these cases prove it on an
//! ordinary core class first.

use lm_testkit::{compile_module_text, run_text};
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
                  \x20   -1\n\
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
                  \x20   \"#{self.label}:#{self.start}-#{self.stop}\"\n\
                  \x20 end\n\
                  end\n\
                  Named(1, 3).show()\n";
    assert_eq!(run(source), "Done(\"r:1-3\")");
}

/// The core classes take the first class indices, so a core parent
/// always precedes its subclass in the verified class table.
#[test]
fn the_core_classes_precede_every_module_class() {
    let module = compile_module_text(
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

// ---------------------------------------------------------------
// Generic-parent inheritance.
// ---------------------------------------------------------------

/// The parent type argument flows into every inherited signature.
#[test]
fn an_instantiated_generic_parent_binds_the_inherited_signature() {
    let source = "class Cell[T]\n\
                  \x20 value: T\n\
                  \x20 def init(mut self, value: T)\n\
                  \x20   self.value = value\n\
                  \x20 end\n\
                  \x20 def get(self): T\n\
                  \x20   self.value\n\
                  \x20 end\n\
                  end\n\
                  class IntCell < Cell[Int]\n\
                  \x20 def init(mut self, value: Int)\n\
                  \x20   super.init(value)\n\
                  \x20 end\n\
                  end\n\
                  IntCell(41).get() + 1\n";
    assert_eq!(run(source), "Done(42)");
}

/// A subclass value is valid at the instantiated parent type, and an
/// override of the inherited method dispatches.
#[test]
fn a_subclass_is_valid_at_the_instantiated_parent_type() {
    let source = "class Cell[T]\n\
                  \x20 value: T\n\
                  \x20 def init(mut self, value: T)\n\
                  \x20   self.value = value\n\
                  \x20 end\n\
                  \x20 def get(self): T\n\
                  \x20   self.value\n\
                  \x20 end\n\
                  end\n\
                  class Bumped < Cell[Int]\n\
                  \x20 def init(mut self, value: Int)\n\
                  \x20   super.init(value)\n\
                  \x20 end\n\
                  \x20 def get(self): Int\n\
                  \x20   super.get() + 1\n\
                  \x20 end\n\
                  end\n\
                  c: Cell[Int] = Bumped(41)\n\
                  c.get()\n";
    assert_eq!(run(source), "Done(42)");
}

/// A generic method of a generic parent keeps its own parameters
/// after the parent parameters.
#[test]
fn a_generic_method_of_a_generic_parent_still_infers() {
    let source = "class Maker[T]\n\
                  \x20 def pair[U](self, a: T, b: U): (T, U)\n\
                  \x20   (a, b)\n\
                  \x20 end\n\
                  end\n\
                  class IntMaker < Maker[Int]\n\
                  end\n\
                  p = IntMaker().pair(1, \"x\")\n\
                  (p[0], p[1])\n";
    assert_eq!(run(source), "Done((1, \"x\"))");
}

/// A wrong type-argument count rejects.
#[test]
fn a_wrong_parent_arity_rejects() {
    let error = reject("class Cell[T]\nend\nclass Bad < Cell[Int, Int]\nend\n1\n");
    assert!(error.contains("E1024"), "{error}");
    assert!(
        error.contains("takes 1 type argument(s), found 2"),
        "{error}"
    );
    let missing = reject("class Cell[T]\nend\nclass Bad < Cell\nend\n1\n");
    assert!(
        missing.contains("takes 1 type argument(s), found 0"),
        "{missing}"
    );
    let extra = reject("class Plain\nend\nclass Bad < Plain[Int]\nend\n1\n");
    assert!(
        extra.contains("takes 0 type argument(s), found 1"),
        "{extra}"
    );
}

/// An unbound name in a parent type argument rejects.
#[test]
fn an_unbound_parent_type_argument_rejects() {
    let error = reject("class Cell[T]\nend\nclass Bad < Cell[Nope]\nend\n1\n");
    assert!(error.contains("E1013"), "{error}");
}

/// An override of an inherited method still may not widen the row.
#[test]
fn an_override_of_a_generic_parent_method_may_not_widen_the_row() {
    let error = reject(
        "class Cell[T]\n\
         \x20 def show(self): String\n\
         \x20   \"cell\"\n\
         \x20 end\n\
         end\n\
         class Loud < Cell[Int]\n\
         \x20 def show(self): String with Io.Write\n\
         \x20   print(\"x\")\n\
         \x20   \"loud\"\n\
         \x20 end\n\
         end\n\
         1\n",
    );
    assert!(error.contains("E1046"), "{error}");
}

/// An override of an inherited method may not change the bound
/// parameter types.
#[test]
fn an_override_of_a_generic_parent_method_keeps_the_bound_types() {
    let error = reject(
        "class Cell[T]\n\
         \x20 def take(self, v: T): Int\n\
         \x20   0\n\
         \x20 end\n\
         end\n\
         class Bad < Cell[Int]\n\
         \x20 def take(self, v: String): Int\n\
         \x20   1\n\
         \x20 end\n\
         end\n\
         1\n",
    );
    assert!(error.contains("E1031"), "{error}");
}

/// A generic class still declares no parent.
#[test]
fn a_generic_class_still_declares_no_parent() {
    let error = reject("class Cell[T]\nend\nclass Bad[U] < Cell[Int]\nend\n1\n");
    assert!(error.contains("E1024"), "{error}");
    assert!(
        error.contains("generic class cannot declare a parent"),
        "{error}"
    );
}

/// A default of a generic parent field that names a class parameter
/// rejects with a diagnostic, not with a verifier rejection.
#[test]
fn a_generic_parent_default_that_names_a_parameter_rejects() {
    let error = reject("class Slot[T]\n  items: [T] = []\nend\nclass Bad < Slot[Int]\nend\n1\n");
    assert!(error.contains("E1024"), "{error}");
    assert!(error.contains("class type parameter"), "{error}");
}

/// The class entry records the parent type arguments, so the verifier
/// reads them from the class table and no call site can forge them.
#[test]
fn the_class_entry_records_the_parent_type_arguments() {
    let module = compile_module_text(
        "inherit.lm",
        "class Cell[T]\n\
         \x20 def get(self): T\n\
         \x20   self.miss()\n\
         \x20 end\n\
         \x20 def miss(self): T\n\
         \x20   self.miss()\n\
         \x20 end\n\
         end\n\
         class IntCell < Cell[Int]\n\
         end\n\
         IntCell()\n\
         1\n",
    )
    .expect("the module compiles");
    let int_cell = module
        .classes
        .iter()
        .position(|c| c.name == "IntCell")
        .expect("the module declares IntCell");
    assert_eq!(module.classes[int_cell].parent_args.len(), 1);
    let arg = module.classes[int_cell].parent_args[0];
    assert_eq!(module.types[arg as usize], lm_bytecode::BcType::Int);
}

/// The class listing shows the parent instantiation the class table
/// records, so the new byte format has a readable dump.
#[test]
fn the_class_listing_shows_the_parent_type_arguments() {
    let module = compile_module_text(
        "inherit.lm",
        "class Cell[T]\n\
         \x20 def get(self): T\n\
         \x20   self.get()\n\
         \x20 end\n\
         end\n\
         class IntCell < Cell[Int]\n\
         end\n\
         class Window < Range\n\
         \x20 def init(mut self)\n\
         \x20   super.init(0, 1)\n\
         \x20 end\n\
         end\n\
         IntCell()\n\
         Window()\n\
         1\n",
    )
    .expect("the module compiles");
    let dump = lm_hir::dump_cfg(&module);
    assert!(dump.contains("IntCell < Cell[Int]\n"), "{dump}");
    assert!(dump.contains("Window < Range\n"), "{dump}");
    // An enum case keeps the implicit identity arguments, so its line
    // is unchanged.
    assert!(
        dump.contains("Option.Some case params 1 < Option\n"),
        "{dump}"
    );
}
