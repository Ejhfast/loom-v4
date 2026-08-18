//! Focused checker tests: one negative case for each rule, plus
//! positive coverage for the lowered CFG dump.

use lm_testkit::{compile_text, run_allowed, run_text};
use lm_vm::VmConfig;

fn code_of(source: &str) -> String {
    let rendered = compile_text("t.lm", source).unwrap_err();
    // The rendered text starts with `error[CODE]:`.
    rendered[6..11].to_string()
}

#[test]
fn negative_cases_have_stable_codes() {
    // Scanner rules.
    assert_eq!(code_of("\u{1}\n"), "E0001");
    assert_eq!(code_of("\"open\n"), "E0002");
    assert_eq!(code_of("\"\\q\"\n"), "E0003");
    assert_eq!(code_of("99999999999999999999\n"), "E0004");
    assert_eq!(code_of("3.14\n"), "E0005");
    assert_eq!(code_of("\"hi {\"\n"), "E0006");
    assert_eq!(code_of("\"hi { }\"\n"), "E0006");
    assert_eq!(code_of("0x\n"), "E0007");
    assert_eq!(code_of("'c'\n"), "E0008");
    assert_eq!(code_of("b\"x\"\n"), "E0009");
    assert_eq!(code_of("\"\"\"x\"\"\"\n"), "E0010");
    // Parser rules.
    assert_eq!(code_of("x = 1 y = 2\n"), "E1001");
    // Week 3: enums and tuples moved from rejected to supported.
    // Week 4: `loop` moved from reserved to supported, so the
    // position rule stands in for the E1002 family.
    assert_eq!(code_of("while true\nclass C\nend\nend\n"), "E1002");
    assert_eq!(code_of("enum Color\nend\n"), "E1040");
    assert_eq!(code_of("m = {1: 2}\nm[1] = 3\n"), "E1002");
    assert_eq!(code_of("if true\n1\n"), "E1003");
    // Checker rules.
    assert_eq!(code_of("1 + \"a\"\n"), "E1004");
    assert_eq!(code_of("not 1\n"), "E1004");
    assert_eq!(code_of("def f(): Int\n  true\nend\nf()\n"), "E1004");
    assert_eq!(code_of("def f(): Int\n  return\nend\nf()\n"), "E1004");
    assert_eq!(code_of("def f()\n  return 1\nend\nf()\n"), "E1004");
    assert_eq!(code_of("missing()\n"), "E1005");
    assert_eq!(code_of("nowhere\n"), "E1005");
    assert_eq!(code_of("a.b\n"), "E1005");
    assert_eq!(code_of("def f(a: Int): Int\n  a\nend\nf()\n"), "E1006");
    assert_eq!(code_of("continue\n"), "E1008");
    assert_eq!(
        code_of("def f(): Int\n  1\nend\ndef f(): Int\n  2\nend\nf()\n"),
        "E1010"
    );
    assert_eq!(code_of("def f(a: Foo): Int\n  1\nend\nf(1)\n"), "E1013");
    assert_eq!(
        code_of("def f(a: Int, a: Int): Int\n  1\nend\nf(1, 1)\n"),
        "E1014"
    );
    assert_eq!(code_of("return\n"), "E1016");
    assert_eq!(code_of("def f()\nend\nf() == f()\n"), "E1017");
    assert_eq!(code_of("class A\nend\nA\n"), "E1018");
    assert_eq!(code_of("def f(): Int\n  1\nend\nf = 3\n"), "E1019");
    assert_eq!(code_of("x = 1\nx: Int = 2\n"), "E1020");
    assert_eq!(code_of("while true\n  break\n  x = 1\nend\n1\n"), "E1021");
}

#[test]
fn week_two_negative_cases_have_stable_codes() {
    // Methods need a `self` receiver.
    assert_eq!(
        code_of("class A\n  def f(n: Int): Int\n    n\n  end\nend\n1\n"),
        "E1023"
    );
    // Generic type misuse.
    assert_eq!(code_of("x: List[Int, Int] = []\nx\n"), "E1024");
    assert_eq!(code_of("x: List = []\nx\n"), "E1024");
    assert_eq!(
        code_of("class Box\n  x: Int = 0\nend\ny: Box[Int] = Box()\ny\n"),
        "E1024"
    );
    // Unknown field.
    assert_eq!(
        code_of("class A\n  x: Int = 0\nend\na = A()\na.y\n"),
        "E1025"
    );
    // Unknown method.
    assert_eq!(
        code_of("class A\n  x: Int = 0\nend\na = A()\na.grow()\n"),
        "E1026"
    );
    // Member access on a non-object type.
    assert_eq!(code_of("x = ()\nx.y\n"), "E1027");
    assert_eq!(code_of("x = 1\nx[0]\n"), "E1027");
    // A required field is not initialized on a path.
    assert_eq!(
        code_of(
            "class P\n  x: Int\n  def init(mut self, b: Bool)\n    if b\n      \
             self.x = 1\n    end\n  end\nend\nP(true)\n"
        ),
        "E1028"
    );
    // A field is read before its first assignment.
    assert_eq!(
        code_of(
            "class P\n  x: Int\n  def init(mut self)\n    y = self.x\n    \
             self.x = 1\n  end\nend\nP()\n"
        ),
        "E1028"
    );
    // `self` escapes before the constructor completes.
    assert_eq!(
        code_of(
            "def id(p: P): P\n  p\nend\nclass P\n  x: Int\n  def init(mut self)\n    \
             id(self)\n    self.x = 1\n  end\nend\nP()\n"
        ),
        "E1029"
    );
    // `self` cannot be captured before the constructor completes.
    assert_eq!(
        code_of(
            "class P\n  x: Int\n  def init(mut self)\n    f = do ||: Int 1 end\n    \
             g = do ||: P self end\n    self.x = 1\n  end\nend\nP()\n"
        ),
        "E1029"
    );
    // `super.init` must run on every path.
    assert_eq!(
        code_of(
            "class A\n  x: Int\n  def init(mut self)\n    self.x = 1\n  end\nend\n\
             class B < A\n  def init(mut self)\n  end\nend\nB()\n"
        ),
        "E1030"
    );
    // `super.init` cannot run twice.
    assert_eq!(
        code_of(
            "class A\n  x: Int\n  def init(mut self)\n    self.x = 1\n  end\nend\n\
             class B < A\n  def init(mut self)\n    super.init()\n    super.init()\n  \
             end\nend\nB()\n"
        ),
        "E1030"
    );
    // An override cannot change the parameter types.
    assert_eq!(
        code_of(
            "class A\n  def f(self, n: Int): Int\n    n\n  end\nend\n\
             class B < A\n  def f(self, n: Bool): Int\n    1\n  end\nend\n1\n"
        ),
        "E1031"
    );
    // An override cannot widen the result type.
    assert_eq!(
        code_of(
            "class A\nend\nclass B < A\nend\n\
             class C\n  def f(self): B\n    B()\n  end\nend\n\
             class D < C\n  def f(self): A\n    A()\n  end\nend\n1\n"
        ),
        "E1031"
    );
    // Calling a value that is not a closure.
    assert_eq!(code_of("x = 1\nx(2)\n"), "E1032");
    // A map key must be Bool, Int, or String.
    assert_eq!(code_of("m = {[1]: 2}\nm\n"), "E1033");
    assert_eq!(code_of("m: {[Int]: Int} = {}\nm\n"), "E1033");
    // Interpolation of an unsupported type.
    assert_eq!(code_of("xs = [1]\n\"{xs}\"\n"), "E1034");
    // A write through a read-only reference.
    assert_eq!(
        code_of("def f(xs: [Int])\n  xs.push(1)\nend\nf([1])\n"),
        "E1035"
    );
    assert_eq!(
        code_of("class A\n  x: Int = 0\nend\ndef f(a: A)\n  a.x = 1\nend\nf(A())\n"),
        "E1035"
    );
    assert_eq!(
        code_of(
            "class A\n  x: Int = 0\n  def bump(mut self)\n    self.x = 1\n  end\nend\n\
             def f(a: A)\n  a.bump()\nend\nf(A())\n"
        ),
        "E1035"
    );
    // A captured name cannot be rebound inside a closure.
    assert_eq!(code_of("y = 1\nf = do ||\n  y = 2\nend\nf()\n"), "E1036");
    // An empty literal needs an expected type.
    assert_eq!(code_of("x = []\nx\n"), "E1037");
    assert_eq!(code_of("x = {}\nx\n"), "E1037");
    // Class declaration rules.
    assert_eq!(code_of("class A\n  x: Int\nend\nA()\n"), "E1038");
    assert_eq!(code_of("class A < Missing\nend\n1\n"), "E1038");
    assert_eq!(code_of("class A < B\nend\nclass B\nend\n1\n"), "E1038");
    assert_eq!(
        code_of("class A\n  x: Int = 0\n  x: Int = 1\nend\n1\n"),
        "E1038"
    );
    assert_eq!(
        code_of("class A\n  def freeze(self): Int\n    1\n  end\nend\n1\n"),
        "E1038"
    );
    // `self` and `super` need a method context.
    assert_eq!(code_of("self\n"), "E1039");
    assert_eq!(
        code_of("class A\nend\nclass B < A\nend\nsuper.f()\n"),
        "E1039"
    );
    // Element and value type mismatches inside literals.
    assert_eq!(code_of("[1, \"a\"]\n"), "E1004");
    assert_eq!(code_of("{\"a\": 1, \"b\": true}\n"), "E1004");
    assert_eq!(code_of("m: {String: Int} = {1: 2}\n"), "E1004");
    assert_eq!(code_of("xs: [Int] = [true]\n"), "E1004");
    // Wrong init arity.
    assert_eq!(
        code_of(
            "class P\n  x: Int\n  def init(mut self, x: Int)\n    self.x = x\n  \
             end\nend\nP()\n"
        ),
        "E1006"
    );
    // A mailbox message type must not name a holder-local class.
    let mailbox = "class B < Proc[{}]\n  def on_spawn(self): Int with Proc\n    1\n  end\nend\n1\n";
    for named in [
        "Vm[Int]",
        "PolicyTable",
        "Request",
        "[Request]",
        "(Int, Vm[Int])",
    ] {
        assert_eq!(code_of(&mailbox.replace("{}", named)), "E1056", "{named}");
    }
    // The walk reads the declared fields of a class message type.
    let holder = "class Holder\n  buf: ByteBuffer\n\
                  \x20 def init(mut self, buf: ByteBuffer)\n    self.buf = buf\n  end\nend\n";
    assert_eq!(
        code_of(&format!("{holder}{}", mailbox.replace("{}", "Holder"))),
        "E1056"
    );
    // The walk reads every arm of an enum message type.
    let payload = "enum Payload\n  Num(value: Int)\n  Buf(bb: ByteBuffer)\nend\n";
    assert_eq!(
        code_of(&format!("{payload}{}", mailbox.replace("{}", "Payload"))),
        "E1056"
    );
}

/// A message type may hold a cycle. The walk visits each class once,
/// so a recursive class neither loops nor rejects.
#[test]
fn a_recursive_message_type_is_accepted() {
    let source = "class Node\n  value: Int\n  next: Option[Node] = None\n\
                  \x20 def init(mut self, value: Int)\n    self.value = value\n  end\nend\n\
                  class Sink < Proc[Node]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n) then n.value\n\
                  \x20   in Closed then 0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Sink.spawn()\n\
                  h.send(Node(4))\n\
                  h.done()\n";
    assert_eq!(
        run_allowed("t.lm", source, &["Proc"]).expect("the program compiles"),
        "Done(Done(4))"
    );
}

#[test]
fn branch_scopes_do_not_leak() {
    // `y` is declared inside the branch, so it is unknown after `end`.
    let source = "if true\n  y = 1\nend\ny\n";
    assert_eq!(code_of(source), "E1005");
}

#[test]
fn while_body_scope_does_not_leak() {
    let source = "while false\n  y = 1\nend\ny\n";
    assert_eq!(code_of(source), "E1005");
}

#[test]
fn annotated_declaration_checks_the_value() {
    assert_eq!(code_of("x: Int = \"two\"\n"), "E1004");
    assert_eq!(
        run_text("t.lm", "x: Int = 3\nx * 2\n", VmConfig::default()).unwrap(),
        "Done(6)"
    );
}

#[test]
fn if_needs_else_to_produce_a_value() {
    assert_eq!(
        code_of("def f(): Int\n  if true\n    1\n  end\nend\nf()\n"),
        "E1004"
    );
}

#[test]
fn subclass_values_join_at_the_common_ancestor() {
    let source = "class Animal\nend\nclass Dog < Animal\nend\nclass Cat < Animal\nend\n\
                  x = if true\n  Dog()\nelse\n  Cat()\nend\ny: Animal = Dog()\ny = Cat()\n0\n";
    assert_eq!(
        run_text("t.lm", source, VmConfig::default()).unwrap(),
        "Done(0)"
    );
}

#[test]
fn cfg_dump_shows_signatures_blocks_and_jumps() {
    let module = compile_text(
        "t.lm",
        "def half(n: Int): Int\n  n / 2\nend\n\nx = 0\nwhile x < 4\n  x = x + 1\nend\nhalf(x)\n",
    )
    .unwrap();
    let dump = lm_hir::dump_cfg(&module);
    // The module functions take the first indices. The entry follows
    // every class method, so the test reads its index from the module.
    let entry = module.entry;
    assert!(dump.contains("fn0 half(Int) -> Int"), "{dump}");
    assert!(
        dump.contains(&format!("fn{entry} <entry>() -> Int")),
        "{dump}"
    );
    assert!(dump.contains("b1:"), "{dump}");
    assert!(dump.contains("JumpIfFalse -> b"), "{dump}");
    assert!(dump.contains("Div"), "{dump}");
    assert!(dump.contains("pop 2 push 1"), "{dump}");
    // The dump is deterministic.
    assert_eq!(dump, lm_hir::dump_cfg(&module));
}

#[test]
fn cfg_dump_covers_classes_selectors_and_closures() {
    let module = compile_text(
        "t.lm",
        "class Counter\n  value: Int = 0\n  def add(mut self, n: Int): Int\n    \
         self.value = self.value + n\n    self.value\n  end\nend\n\
         c = Counter()\nf = do |x: Int|: Int x + 1 end\nc.add(f(1))\n",
    )
    .unwrap();
    let dump = lm_hir::dump_cfg(&module);
    // The core classes register first, so a module class never takes
    // index zero. The test reads the index from the module.
    let counter = module
        .classes
        .iter()
        .position(|c| c.name == "Counter")
        .expect("the module declares Counter");
    assert!(counter > 0, "a module class follows the core classes");
    let sel_index = module
        .selectors
        .iter()
        .position(|s| s == "add")
        .expect("the module interns the add selector");
    assert!(
        dump.contains(&format!("selector sel{sel_index} = add")),
        "{dump}"
    );
    assert!(
        dump.contains(&format!("class class{counter} Counter")),
        "{dump}"
    );
    assert!(dump.contains("field 0 value: Int"), "{dump}");
    assert!(
        dump.contains(&format!("CallVirtual sel{sel_index} argc 1")),
        "{dump}"
    );
    assert!(dump.contains("MakeClosure"), "{dump}");
    assert!(dump.contains("CallValue argc 1"), "{dump}");
    assert!(dump.contains("<new Counter>"), "{dump}");
    assert!(dump.contains(&format!("New class{counter}")), "{dump}");
}

#[test]
fn printable_ast_is_available() {
    let ast = lm_source::parse::parse("x = 1\nx + 2\n").unwrap();
    let dump = lm_source::ast::dump_module(&ast);
    assert!(dump.contains("assign x"), "{dump}");
    assert!(dump.contains("binary +"), "{dump}");
}
