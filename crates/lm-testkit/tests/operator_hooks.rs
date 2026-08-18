//! User-declared operator hooks (specification 6.4).
//!
//! An operator reads its hook from the class of the left operand and
//! then takes the ordinary method path. These cases pin the four
//! consequences: the parameter type, the result type, the effect row,
//! and the dispatch rule.

use lm_testkit::{compile_text, repo_root, run_allowed, run_text};
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("ops.lm", source, VmConfig::default()).unwrap()
}

fn allowed(source: &str, allow: &[&str]) -> String {
    run_allowed("ops.lm", source, allow).unwrap()
}

fn code_of(source: &str) -> String {
    let rendered = compile_text("ops.lm", source).unwrap_err();
    rendered[6..11].to_string()
}

const MONEY: &str = "final class Money
  cents: Int

  def init(mut self, cents: Int)
    self.cents = cents
  end

  def __add__(self, other: Money): Money
    Money(self.cents + other.cents)
  end

  def __mul__(self, count: Int): Money
    Money(self.cents * count)
  end

  def __neg__(self): Money
    Money(0 - self.cents)
  end

  def __lt__(self, other: Money): Bool
    self.cents < other.cents
  end
end
";

#[test]
fn a_class_hook_serves_a_binary_operator() {
    let source = format!("{MONEY}(Money(150) + Money(250)).cents\n");
    assert_eq!(run(&source), "Done(400)");
}

/// The right operand takes the declared parameter type, so it need
/// not match the receiver.
#[test]
fn the_parameter_type_comes_from_the_hook() {
    let source = format!("{MONEY}(Money(150) * 3).cents\n");
    assert_eq!(run(&source), "Done(450)");
    // An operand of the wrong type is an ordinary argument mismatch.
    let bad = format!("{MONEY}Money(1) * Money(2)\n");
    assert_eq!(code_of(&bad), "E1004");
}

#[test]
fn a_class_hook_serves_a_unary_operator() {
    let source = format!("{MONEY}(-Money(150)).cents\n");
    assert_eq!(run(&source), "Done(-150)");
}

#[test]
fn a_class_hook_serves_a_comparison() {
    let source = format!("{MONEY}if Money(1) < Money(2)\n  1\nelse\n  0\nend\n");
    assert_eq!(run(&source), "Done(1)");
}

/// A hook may return any type. This one leaves the receiver type.
#[test]
fn a_hook_result_type_is_free() {
    let source = "final class Tag
  def __add__(self, other: Int): String
    \"tag{other}\"
  end
end
Tag() + 7
";
    assert_eq!(run(source), "Done(\"tag7\")");
}

/// `==` reads `__eq__` when the class declares one, and keeps
/// reference identity when it does not.
#[test]
fn equality_uses_the_hook_only_when_declared() {
    let with_hook = "final class V
  x: Int
  def init(mut self, x: Int)
    self.x = x
  end
  def __eq__(self, other: V): Bool
    self.x == other.x
  end
end
if V(1) == V(1)\n  1\nelse\n  0\nend
";
    assert_eq!(run(with_hook), "Done(1)");
    let without_hook = "final class W
  x: Int
  def init(mut self, x: Int)
    self.x = x
  end
end
if W(1) == W(1)\n  1\nelse\n  0\nend
";
    assert_eq!(run(without_hook), "Done(0)");
}

/// The row of the hook reaches the caller, so an operator cannot
/// hide an effect.
#[test]
fn a_hook_row_is_charged_to_the_caller() {
    let source = "final class L
  def __mul__(self, other: Int): Int with Io.Print
    sys.io.print(\"x\")
    other * 2
  end
end
def go(): Int with Io.Print
  L() * 21
end
go()
";
    assert_eq!(allowed(source, &["Io.Print"]), "Done(42)");
    // The same program without the row is rejected at the operator.
    let bare = source.replace("def go(): Int with Io.Print", "def go(): Int");
    assert_eq!(code_of(&bare), "E1046");
}

/// A class that is not final dispatches on the runtime class.
#[test]
fn a_hook_of_an_open_class_dispatches_virtually() {
    let source = "class Base
  def __add__(self, other: Int): String
    \"base\"
  end
end
class Sub < Base
  def __add__(self, other: Int): String
    \"sub\"
  end
end
b: Base = Sub()
b + 1
";
    assert_eq!(run(source), "Done(\"sub\")");
}

/// The core types keep their canonical instructions. The sugar adds
/// a spelling and removes no rule.
#[test]
fn core_operators_keep_their_lowering() {
    let module = compile_text(
        "ops.lm",
        "def f(a: Int, b: Int): Int\n  a + b * 2\nend\nf(3, 4)\n",
    )
    .expect("the program compiles");
    let f = module
        .funcs
        .iter()
        .find(|f| f.name == "f")
        .expect("f exists");
    let body: Vec<&lm_bytecode::Instr> = f.blocks.iter().flatten().collect();
    assert!(
        body.iter().any(|i| matches!(i, lm_bytecode::Instr::Add))
            && body.iter().any(|i| matches!(i, lm_bytecode::Instr::Mul)),
        "arithmetic must stay one instruction each: {body:?}"
    );
    assert!(
        !body
            .iter()
            .any(|i| matches!(i, lm_bytecode::Instr::CallVirtual { .. })),
        "core arithmetic must not dispatch: {body:?}"
    );
}

/// A hook with the wrong operand count is a clear diagnostic, not a
/// silent fall back to the built-in rule.
#[test]
fn a_hook_of_the_wrong_arity_is_rejected() {
    let source = "final class Odd
  def __add__(self, a: Int, b: Int): Int
    a + b
  end
end
Odd() + 1
";
    assert_eq!(code_of(source), "E1006");
}

#[test]
fn the_operator_examples_run() {
    let read =
        |path: &str| std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    assert_eq!(
        run(&read("examples/10-operator-sugar/01-money.lm")),
        "Done(\"486c then -486c\")"
    );
    assert_eq!(
        run(&read("examples/10-operator-sugar/02-comparison.lm")),
        "Done(\"equal by value; 1.4 precedes 1.10; 2.0 is not older\")"
    );
    assert_eq!(
        allowed(
            &read("examples/10-operator-sugar/03-effects-and-dispatch.lm"),
            &["Io.Print"]
        ),
        "Done(\"doubling 30, total 5\")"
    );
}
