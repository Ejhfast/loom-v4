//! Tests for expression-only bodies and value-producing loops.

use lm_bytecode::Instr;
use lm_testkit::{compile_module_text, run_text};
use lm_vm::VmConfig;

fn run(source: &str) -> String {
    run_text("expression-control.lm", source, VmConfig::default()).unwrap()
}

fn expect_error(source: &str, text: &str) {
    let error = compile_module_text("expression-control.lm", source).unwrap_err();
    assert!(error.contains(text), "expected `{text}` in:\n{error}");
}

#[test]
fn inline_if_and_elsif_produce_one_value() {
    let source = "def clamp(value: Int, low: Int, high: Int): Int\n\
  if value < low then low elsif value > high then high else value end\n\
end\n\
(clamp(-2, 0, 10), clamp(20, 0, 10), clamp(5, 0, 10))\n";
    assert_eq!(run(source), "Done((0, 10, 5))");
}

#[test]
fn then_can_precede_a_newline_body() {
    let source = "value = if true then\n\
  5\n\
else\n\
  9\n\
end\n\
value\n";
    assert_eq!(run(source), "Done(5)");
}

#[test]
fn assignment_is_a_unit_expression() {
    let source = "def consume(value: ()): Int\n\
  7\n\
end\n\
x = 1\n\
answer = consume(x = 4)\n\
(answer, x)\n";
    assert_eq!(run(source), "Done((7, 4))");
}

#[test]
fn a_declaring_assignment_is_valid_inside_an_expression() {
    let source = "def consume(value: ()): Int\n\
  3\n\
end\n\
answer = consume(created = 9)\n\
(answer, created)\n";
    assert_eq!(run(source), "Done((3, 9))");
}

#[test]
fn a_branch_local_does_not_escape_its_body() {
    let source = "if true then hidden = 9 else () end\nhidden\n";
    expect_error(source, "cannot find `hidden` in this scope");
}

#[test]
fn case_then_accepts_a_field_assignment() {
    let source = "final class Cell\n\
  n: Int = 0\n\
  def set(mut self, value: Int)\n\
    case value\n\
    in 0 then self.n = 10\n\
    in _ then self.n = value\n\
    end\n\
  end\n\
end\n\
cell = Cell()\n\
cell.set(0)\n\
cell.n\n";
    assert_eq!(run(source), "Done(10)");
}

#[test]
fn return_has_never_at_its_expression_position() {
    let source = "def positive(value: Int): Int\n\
  if value > 0 then value else return 1 end\n\
end\n\
positive(-4)\n";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn loop_joins_valued_breaks() {
    let source = "def choose_value(value: Int): Int\n\
  loop do\n\
    if value > 0 then break value else break 0 end\n\
  end\n\
end\n\
choose_value(8)\n";
    assert_eq!(run(source), "Done(8)");
}

#[test]
fn a_bare_break_gives_loop_the_unit_type() {
    let source = "value: () = loop do break end\nvalue\n";
    assert_eq!(run(source), "Done(())");
}

#[test]
fn a_discarded_loop_does_not_join_break_values() {
    let source = "loop do\n\
  if true then break 1 else break \"unused\" end\n\
end\n\
42\n";
    assert_eq!(run(source), "Done(42)");
}

#[test]
fn while_and_for_reject_valued_breaks() {
    expect_error("while true do; break 1 end\n0\n", "cannot carry a value");
    expect_error("for n in [1] do; break n end\n0\n", "cannot carry a value");
}

#[test]
fn loop_break_values_need_one_join_type() {
    let source = "value = loop do\n\
  if true then break 1 else break \"wrong\" end\n\
end\n\
value\n";
    expect_error(source, "expected");
}

#[test]
fn a_loop_join_widens_nested_enum_cases() {
    let source = "enum Choice\n\
  First\n\
  Second\n\
end\n\
values = loop do\n\
  if true then break [First] else break [Second] end\n\
end\n\
values.len()\n";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn a_loop_result_keeps_read_only_mutability() {
    let source = "def extend(items: List[Int]): Int\n\
  picked = loop do break items end\n\
  picked.push(2)\n\
  picked.len()\n\
end\n\
extend([1])\n";
    expect_error(source, "needs a mutable receiver");
}

#[test]
fn a_loop_uses_sibling_breaks_for_constructor_inference() {
    let source = "value = loop do\n\
  map = do |item: Int|: Int item + 1 end\n\
  if true then break Some(map(0)) else break None end\n\
end\n\
value.value_or(0)\n";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn while_true_has_a_normal_exit_in_the_type_system() {
    let source = "def choose(): Int\n\
  while true\n\
    return 3\n\
  end\n\
  7\n\
end\n\
choose()\n";
    assert_eq!(run(source), "Done(3)");
}

#[test]
fn never_propagates_through_assignment_and_call_arguments() {
    let assignment = "def choose(): Int\n\
  value = return 4\n\
  value\n\
end\n\
choose()\n";
    expect_error(assignment, "unreachable");

    let call = "def keep(value: Int): Int\n\
  value\n\
end\n\
def choose(): Int\n\
  keep(return 4)\n\
  9\n\
end\n\
choose()\n";
    expect_error(call, "unreachable");

    let branch = "def keep(value: Int): Int\n\
  value\n\
end\n\
def choose(): Int\n\
  value = if true then keep(return 4) else 9 end\n\
  value\n\
end\n\
choose()\n";
    assert_eq!(run(branch), "Done(4)");

    let break_operand = "def keep(value: Int): Int\n\
  value\n\
end\n\
def choose(): Int\n\
  loop do break keep(return 4) end\n\
  9\n\
end\n\
choose()\n";
    expect_error(break_operand, "unreachable");
}

#[test]
fn control_transfers_parse_before_expression_delimiters() {
    let source = "def stop()\n\
  (return)\n\
end\n\
def consume(value: Int): Int\n\
  value\n\
end\n\
loop do\n\
  consume(break)\n\
end\n\
loop do\n\
  values: List[Int] = [break]\n\
end\n\
42\n";
    assert_eq!(run(source), "Done(42)");
}

#[test]
fn a_later_if_condition_can_transfer_control() {
    let source = "def choose(first: Bool): Int\n\
  if first then 3 elsif return 7 then 4 else 5 end\n\
end\n\
(choose(true), choose(false))\n";
    assert_eq!(run(source), "Done((3, 7))");
}

#[test]
fn do_with_a_separator_opens_inline_loop_bodies() {
    let source = "def numbers(): List[Int]\n\
  [1, 2, 3]\n\
end\n\
sum = 0\n\
for number in numbers() do; sum = sum + number end\n\
while sum < 7 do; sum = sum + 1 end\n\
sum\n";
    assert_eq!(run(source), "Done(7)");
}

#[test]
fn a_loop_header_call_keeps_its_trailing_closure() {
    let source = "def make(producer: () -> List[Int]): List[Int]\n\
  producer()\n\
end\n\
sum = 0\n\
for number in make() do || [2, 3] end\n\
  sum = sum + number\n\
end\n\
sum\n";
    assert_eq!(run(source), "Done(5)");
}

#[test]
fn break_unwinds_a_pending_receiver() {
    let source = "items: List[Int] = []\n\
while true\n\
  items.push(if items.len() > 2 then break else 1 end)\n\
end\n\
items.len()\n";
    assert_eq!(run(source), "Done(3)");
}

#[test]
fn break_unwinds_a_pending_binary_operand() {
    let source = "total = 0\n\
index = 0\n\
while true\n\
  index = index + 1\n\
  total = total + (if index > 3 then break else index end)\n\
end\n\
total\n";
    assert_eq!(run(source), "Done(6)");
}

#[test]
fn continue_unwinds_a_pending_argument_receiver() {
    let source = "items: List[Int] = []\n\
index = 0\n\
while index < 4\n\
  index = index + 1\n\
  items.push(if index % 2 == 0 then continue else index end)\n\
end\n\
items.len()\n";
    assert_eq!(run(source), "Done(2)");
}

#[test]
fn a_valued_break_removes_a_pending_receiver_before_its_result() {
    let source = "items: List[Int] = [1, 2, 3]\n\
loop do\n\
  items.push(if true then break items.len() else 1 end)\n\
end\n";
    assert_eq!(run(source), "Done(3)");

    let module = compile_module_text("expression-control.lm", source).unwrap();
    let entry = &module.funcs[module.entry as usize];
    assert!(entry.blocks.iter().any(|block| {
        block.windows(3).any(|window| {
            matches!(window[0], Instr::StoreLocal(_))
                && matches!(window[1], Instr::Pop)
                && matches!(window[2], Instr::Jump(_))
        })
    }));
}

#[test]
fn an_inline_loop_do_needs_a_separator() {
    expect_error(
        "for item in [1] do item end\n0\n",
        "`do` opens a loop body only before a newline or `;`",
    );

    expect_error(
        "def items(): List[Int]\n  [1]\nend\nfor item in items() do item end\n0\n",
        "`do` opens a loop body only before a newline or `;`",
    );
}
