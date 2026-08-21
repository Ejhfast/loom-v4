//! Regression tests for the post-week-4 fix set:
//! declared local-type tables, labeled arguments, sibling inference,
//! `mut` markers in function types, the constructor-collision note,
//! and nested exact-arm exhaustiveness.

use lm_testkit::{compile_text, run_allowed, run_text};
use lm_vm::{LoadError, VmConfig};

fn run(source: &str) -> String {
    run_text("fixes.lm", source, VmConfig::default()).unwrap()
}

fn allowed(source: &str, allow: &[&str]) -> String {
    run_allowed("fixes.lm", source, allow).unwrap()
}

fn expect_error(source: &str, needle: &str) {
    let rendered = compile_text("fixes.lm", source).unwrap_err();
    assert!(
        rendered.contains(needle),
        "expected `{needle}` in:\n{rendered}"
    );
}

const ANIMALS: &str = "class Animal
  name: String = \"a\"
end
class Dog < Animal
end
class Cat < Animal
end
";

// ---------------------------------------------------------------
// Finding 1: the declared local type survives into verification.
// ---------------------------------------------------------------

#[test]
fn widened_local_passes_a_sibling_type_test() {
    let source = format!("{ANIMALS}a: Animal = Dog()\nif a is Cat\n  1\nelse\n  2\nend\n");
    assert_eq!(run(&source), "Done(2)");
}

#[test]
fn widened_local_accepts_a_sibling_reassignment() {
    let source =
        format!("{ANIMALS}a: Animal = Dog()\na = Cat()\nif a is Cat\n  1\nelse\n  2\nend\n");
    assert_eq!(run(&source), "Done(1)");
}

#[test]
fn widened_locals_compare_by_reference() {
    // The checker accepts `==` at the declared type Animal. The
    // verifier must use the declared slot types, not Dog and Cat.
    let source =
        format!("{ANIMALS}a: Animal = Dog()\nb: Animal = Cat()\nif a == b\n  1\nelse\n  2\nend\n");
    assert_eq!(run(&source), "Done(2)");
}

#[test]
fn widened_enum_local_runs_a_full_case() {
    let source = "o: Option[Int] = Some(1)\ncase o\nin Some(v) then v\nin None then 0\nend\n";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn widened_tuple_local_keeps_its_declared_type() {
    let source = format!("{ANIMALS}t: (Animal, Int) = (Dog(), 3)\nt[1]\n");
    assert_eq!(run(&source), "Done(3)");
}

#[test]
fn function_local_holds_a_narrower_row_closure() {
    let source = "f: (Int) -> Int with Io.Print = do |x: Int|: Int x + 1 end\nf(1)\n";
    assert_eq!(run(source), "Done(2)");
}

#[test]
fn function_local_joins_branches_with_different_rows() {
    let source = "def go(): Int with Clock.Now, Io.Print
  flag = true
  f: (Int) -> Int with Clock.Now, Io.Print = do |x: Int|: Int x end
  if flag
    f = do |x: Int|: Int with Io.Print x end
  else
    f = do |x: Int|: Int with Clock.Now x end
  end
  f(1)
end

go()
";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn widened_local_reassigned_in_a_loop() {
    let source = format!(
        "{ANIMALS}a: Animal = Dog()\ni = 0\nwhile i < 3\n  a = Cat()\n  i = i + 1\nend\n\
         if a is Cat\n  1\nelse\n  2\nend\n"
    );
    assert_eq!(run(&source), "Done(1)");
}

// The verifier validates the declared table instead of trusting it.

fn widened_module() -> lm_bytecode::Module {
    let source = format!("{ANIMALS}a: Animal = Dog()\nif a is Cat\n  1\nelse\n  2\nend\n");
    compile_text("fixes.lm", &source).unwrap()
}

fn expect_load_reject(module: &lm_bytecode::Module, needle: &str) {
    let bytes = lm_bytecode::encode(module);
    match lm_vm::load_bytes(&bytes) {
        Err(LoadError::Verify(e)) => {
            assert!(e.message.contains(needle), "wrong rejection: {e}");
        }
        other => panic!("expected a verifier rejection with `{needle}`, got {other:?}"),
    }
}

#[test]
fn forged_narrow_local_type_entry_is_rejected() {
    // Narrow the declared Animal slot of the entry to Int. The store
    // of the Dog instance no longer fits the declared type.
    let mut module = widened_module();
    let animal = module
        .classes
        .iter()
        .position(|c| c.name == "Animal")
        .expect("the module declares Animal") as u32;
    let animal_ty = module
        .types
        .iter()
        .position(|t| matches!(t, lm_bytecode::BcType::Class(c) if *c == animal))
        .expect("the Animal class type exists") as u32;
    let entry = module.entry as usize;
    let mut hit = false;
    for slot in module.funcs[entry].local_types.iter_mut() {
        if *slot == animal_ty {
            *slot = lm_verify::TY_INT;
            hit = true;
        }
    }
    assert!(hit, "the entry declares an Animal local");
    expect_load_reject(&module, "declared type");
}

#[test]
fn out_of_range_local_type_entry_is_rejected() {
    let mut module = widened_module();
    let entry = module.entry as usize;
    let bad = module.types.len() as u32 + 7;
    *module.funcs[entry]
        .local_types
        .last_mut()
        .expect("the entry has locals") = bad;
    expect_load_reject(&module, "type index");
}

#[test]
fn local_type_table_shorter_than_parameters_is_rejected() {
    let source = "def double(n: Int): Int\n  n * 2\nend\ndouble(4)\n";
    let mut with_params = compile_text("fixes.lm", source).unwrap();
    let f = with_params
        .funcs
        .iter()
        .position(|f| f.name == "double")
        .expect("double exists");
    with_params.funcs[f].local_types.clear();
    expect_load_reject(&with_params, "more parameters than local slots");
    // A wrong parameter prefix is also rejected.
    let mut wrong_prefix = compile_text("fixes.lm", source).unwrap();
    let f = wrong_prefix
        .funcs
        .iter()
        .position(|f| f.name == "double")
        .expect("double exists");
    wrong_prefix.funcs[f].local_types[0] = lm_verify::TY_BOOL;
    expect_load_reject(&wrong_prefix, "prefix");
}

// ---------------------------------------------------------------
// Finding 2: labeled arguments.
// ---------------------------------------------------------------

const GREET: &str = "def greet(name: String, count: Int): Int
  if name == \"Ada\"
    count + 10
  else
    count
  end
end
";

#[test]
fn labeled_arguments_follow_positionals() {
    let source = format!("{GREET}greet(\"Ada\", count: 2)\n");
    assert_eq!(run(&source), "Done(12)");
}

#[test]
fn labeled_arguments_match_in_any_order() {
    let source = format!("{GREET}greet(count: 2, name: \"Ada\")\n");
    assert_eq!(run(&source), "Done(12)");
}

#[test]
fn labeled_arguments_work_on_methods_and_constructors() {
    let source = "class Point
  x: Int
  y: Int

  def init(mut self, x: Int, y: Int)
    self.x = x
    self.y = y
  end

  def shifted(self, dx: Int, dy: Int): Int
    self.x + dx + self.y + dy
  end
end

enum Wrap
  Two(a: Int, b: Int)
end

p = Point(x: 1, y: 2)
w = Wrap.Two(b: 4, a: 3)
case w
in Two(a, b) then p.shifted(dy: 10, dx: 100) + a + b
end
";
    assert_eq!(run(source), "Done(120)");
}

#[test]
fn unknown_label_is_rejected() {
    let source = format!("{GREET}greet(\"Ada\", total: 2)\n");
    expect_error(&source, "does not declare a parameter named `total`");
}

#[test]
fn duplicate_label_is_rejected() {
    let source = format!("{GREET}greet(name: \"Ada\", name: \"Bo\")\n");
    expect_error(&source, "appears more than one time");
}

#[test]
fn positional_after_label_is_rejected() {
    let source = format!("{GREET}greet(name: \"Ada\", 2)\n");
    expect_error(&source, "cannot follow a labeled argument");
}

#[test]
fn label_for_a_filled_parameter_is_rejected() {
    let source = format!("{GREET}greet(\"Ada\", name: \"Bo\")\n");
    expect_error(&source, "positional argument already fills");
}

#[test]
fn labels_on_a_closure_value_are_rejected() {
    let source = "f = do |x: Int|: Int x end\nf(x: 1)\n";
    expect_error(source, "does not declare a parameter named `x`");
}

/// A native method declares parameter names, so it takes labels
/// under the one rule of specification 6.1.
#[test]
fn labeled_arguments_work_on_native_methods() {
    let source = "xs: [Int] = []
xs.push(value: 5)
m: {String: Int} = {}
m.put(key: \"a\", value: 2)
b = StringBuilder()
b.append(text: \"z\")
extra = if b.build() == \"z\"
  1
else
  0
end
xs.at(index: 0) + m.at(key: \"a\") + extra
";
    assert_eq!(run(source), "Done(8)");
}

/// `args:` is one label on a declared name, not a special case. The
/// spelling of specification 6.1 line 704 keeps working, the other
/// name works, and both orders work.
#[test]
fn activate_labels_follow_the_general_rule() {
    let program = "def child(a: Int, b: Int): Int\n  a + b\nend\n";
    let tail = "case vm.run()\nin Done(v) then v\nin Fault(_) then 0 - 1\nend\n";
    for call in [
        "sys.vm.Vm().activate_or_fault(child, args: (3, 4))",
        "sys.vm.Vm().activate_or_fault(child, (3, 4))",
        "sys.vm.Vm().activate_or_fault(program: child, args: (3, 4))",
        "sys.vm.Vm().activate_or_fault(args: (3, 4), program: child)",
    ] {
        let source = format!("{program}vm = {call}\n{tail}");
        assert_eq!(allowed(&source, &["Vm"]), "Done(7)", "call: {call}");
    }
}

#[test]
fn an_unknown_activate_label_reports_the_general_diagnostic() {
    let source = "def child(): Int\n  1\nend\n\
        vm = sys.vm.Vm().activate_or_fault(child, tuple: ())\n";
    expect_error(
        source,
        "`activate_or_fault` does not declare a parameter named `tuple`",
    );
}

#[test]
fn vm_and_run_have_distinct_type_forms() {
    expect_error(
        "def bad(image: Vm[Int]): Int\n  1\nend\n1\n",
        "`Vm` takes no type arguments; use `Run[T]` for an active invocation",
    );
    let source = "def finish(run: Run[Int]): Int with Vm\n\
        \x20 case run.run()\n\
        \x20 in Done(value) then value\n\
        \x20 in Fault(_) then 0 - 1\n\
        \x20 end\n\
        end\n\
        image: Vm = sys.vm.Vm()\n\
        finish(image.activate_or_fault(do ||: Int 42 end, args: ()))\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(42)");
}

#[test]
fn a_repeated_native_label_is_rejected() {
    let source = "m: {String: Int} = {}\nm.put(key: \"a\", key: \"b\")\n";
    expect_error(source, "appears more than one time");
}

#[test]
fn a_positional_argument_cannot_follow_a_native_label() {
    let source = "m: {String: Int} = {}\nm.put(key: \"a\", 2)\n";
    expect_error(source, "cannot follow a labeled argument");
}

/// The continuation methods name their parameters too.
#[test]
fn labels_work_on_the_continuation_methods() {
    let source = "def child(): Int with Clock.Now\n  sys.clock.now()\nend\n\
        vm = sys.vm.Vm().activate_or_fault(child, args: ())\n\
        case vm.drive()\n\
        in Asked(request)\n  \
        case request\n  \
        in Call(Clock.Now, c, ())\n    \
        vm.answer(call: c, value: 7)\n  \
        in _\n    \
        vm.dispatch(request)\n  \
        end\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\n\
        in Done(_) then 0 - 2\n\
        in Fault(_) then 0 - 3\n\
        end\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(7)");
}

// ---------------------------------------------------------------
// Finding 3: sibling inference for arm-typed constructors.
// ---------------------------------------------------------------

#[test]
fn branch_join_binds_none_from_a_sibling() {
    let source = "flag = true
o = if flag
  Some(1)
else
  None
end
case o
in Some(v) then v
in None then 0
end
";
    assert_eq!(run(source), "Done(1)");
}

#[test]
fn elsif_chain_binds_none_from_a_sibling() {
    let source = "n = 5
o = if n < 3
  None
elsif n < 10
  Some(n)
else
  None
end
case o
in Some(v) then v
in None then 0
end
";
    assert_eq!(run(source), "Done(5)");
}

#[test]
fn case_join_binds_none_from_a_sibling() {
    let source = "n = 2
o = case n
in 1 then None
in other then Some(other)
end
case o
in Some(v) then v
in None then 0
end
";
    assert_eq!(run(source), "Done(2)");
}

#[test]
fn list_literal_binds_none_from_a_sibling() {
    assert_eq!(run("xs = [Some(1), None]\nxs.len()\n"), "Done(2)");
    assert_eq!(run("xs = [None, Some(1)]\nxs.len()\n"), "Done(2)");
}

#[test]
fn map_literal_binds_none_from_a_sibling() {
    assert_eq!(run("m = {1: Some(\"one\"), 2: None}\nm.len()\n"), "Done(2)");
}

#[test]
fn nested_constructor_binds_from_a_sibling() {
    let source = "xs = [Some(None), Some(Some(1))]\nxs.len()\n";
    assert_eq!(run(source), "Done(2)");
}

#[test]
fn user_enum_binds_from_a_sibling() {
    let source = "enum Maybe[T]
  Just(value: T)
  Nothing
end

xs = [Just(1), Nothing]
case xs.at(1)
in Just(v) then v
in Nothing then 9
end
";
    assert_eq!(run(source), "Done(9)");
}

#[test]
fn ambiguous_siblings_still_error() {
    expect_error("xs = [None, None]\nxs.len()\n", "E1045");
    expect_error(
        "flag = true\no = if flag\n  None\nelse\n  None\nend\n1\n",
        "E1045",
    );
}

// ---------------------------------------------------------------
// Finding 4: `mut` markers in function types.
// ---------------------------------------------------------------

#[test]
fn closure_mut_parameter_needs_a_mutable_argument() {
    let source = "def sneak(xs: [Int]): Int
  f = do |mut ys: [Int]|: () ys.push(1) end
  f(xs)
  xs.len()
end

sneak([9])
";
    expect_error(source, "a `mut` parameter needs a mutable value");
}

#[test]
fn mut_closure_calls_with_a_mutable_argument() {
    let source = "xs: [Int] = [9]
f = do |mut ys: [Int]|: () ys.push(1) end
f(xs)
xs.len()
";
    assert_eq!(run(source), "Done(2)");
}

#[test]
fn function_types_carry_mut_markers() {
    // A declared `mut` position accepts a read-only closure, and a
    // `mut`-requiring closure does not fit a read-only position.
    let ok = "f: (mut [Int]) -> Int = do |ys: [Int]|: Int ys.len() end\nf([1, 2])\n";
    assert_eq!(run(ok), "Done(2)");
    let bad = "f: ([Int]) -> () = do |mut ys: [Int]|: () ys.push(1) end\n1\n";
    expect_error(bad, "E1004");
}

#[test]
fn mut_marker_outside_a_function_type_is_rejected() {
    expect_error(
        "x: (mut Int) = 1\nx\n",
        "only valid before a parameter type",
    );
}

#[test]
fn calling_through_a_mut_function_type_needs_capability() {
    let source = "def call_it(f: (mut [Int]) -> (), xs: [Int])
  f(xs)
end

call_it(do |mut ys: [Int]|: () ys.push(1) end, [1])
";
    expect_error(source, "a `mut` parameter needs a mutable value");
}

#[test]
fn forged_fn_type_mut_flag_is_rejected() {
    // Flip the mut flag byte of the closure function type in the
    // encoded module. The stored closure type then differs from the
    // real one, so the verifier rejects the module.
    let source = "f = do |mut ys: [Int]|: () ys.push(1) end\nxs: [Int] = []\nf(xs)\nxs.len()\n";
    let mut module = compile_text("fixes.lm", source).unwrap();
    let mut hit = false;
    for ty in &mut module.types {
        if let lm_bytecode::BcType::Fn(_, muts, _, _) = ty {
            if muts.as_slice() == [true] {
                muts[0] = false;
                hit = true;
            }
        }
    }
    assert!(hit, "the closure type carries a mut marker");
    expect_load_reject(&module, "type table");
}

#[test]
fn invalid_mut_flag_byte_is_rejected_by_the_decoder() {
    let source = "f = do |mut ys: [Int]|: () ys.push(1) end\n1\n";
    let bytes = lm_bytecode::encode(&compile_text("fixes.lm", source).unwrap());
    // Flip every byte to 2 in turn; at least one position must be a
    // mut flag and fail with the flag error.
    let mut rejected = false;
    for pos in 0..bytes.len() {
        if bytes[pos] != 1 {
            continue;
        }
        let mut corrupt = bytes.clone();
        corrupt[pos] = 2;
        if lm_bytecode::decode(&corrupt) == Err(lm_bytecode::DecodeError::BadFlag(2)) {
            rejected = true;
            break;
        }
    }
    assert!(rejected, "no mut flag rejection was observed");
}

// ---------------------------------------------------------------
// Finding 5: the constructor-collision note.
// ---------------------------------------------------------------

#[test]
fn colliding_constructor_gets_a_qualified_note() {
    let source = "enum Pairing
  Pair(a: Int, b: Int)
  Solo(a: Int)
end

p: Pairing = Pair(1, 2)
1
";
    expect_error(
        source,
        "the enum `Pairing` has an arm named `Pair`; write `Pairing.Pair(...)`",
    );
}

#[test]
fn qualified_colliding_constructor_works() {
    let source = "enum Pairing
  Pair(a: Int, b: Int)
  Solo(a: Int)
end

p: Pairing = Pairing.Pair(1, 2)
case p
in Pair(a, b) then a + b
in Solo(a) then a
end
";
    assert_eq!(run(source), "Done(3)");
}

// ---------------------------------------------------------------
// Finding 6: nested exact-arm exhaustiveness.
//
// This finding is reversed. A constructor now builds a value of the
// enum and not of the arm it names, so no expression carries an arm
// type and the recursive injection has nothing to narrow. A nested
// case covers every arm or uses a wildcard, like every other case.
// `docs/notes/week4-fixes.md` records the reversal.
// ---------------------------------------------------------------

#[test]
fn nested_constructor_scrutinee_covers_every_arm() {
    let source = "s = Some(Some(3))\ncase s\nin Some(Some(v)) then v\nend\n";
    expect_error(source, "does not cover every value");
    let covered = "s = Some(Some(3))\ncase s\n\
                   in Some(Some(v)) then v\nin Some(None) then 0\nin None then 0 - 1\nend\n";
    assert_eq!(run(covered), "Done(3)");
}

#[test]
fn deeper_constructor_scrutinee_takes_a_wildcard() {
    let source =
        "s = Some(Some(Some(4)))\ncase s\nin Some(Some(Some(v))) then v\nin _ then 0\nend\n";
    assert_eq!(run(source), "Done(4)");
}

#[test]
fn family_typed_inner_position_still_needs_full_coverage() {
    let source = "o: Option[Option[Int]] = Some(Some(3))
case o
in Some(Some(v)) then v
in None then 0
end
";
    expect_error(source, "E1042");
}

// ---------------------------------------------------------------
// A loop with no exit never completes (specification 7.2).
// ---------------------------------------------------------------

/// The driver shape: the loop leaves only by `return`, and the
/// declared result type needs no tail expression.
#[test]
fn a_loop_with_no_break_ends_a_body_of_any_type() {
    let source = "def f(n: Int): Int\n  n + 1\nend\n\
        def y(): Int\n  loop do\n    if true\n      return f(0)\n    end\n    ()\n  end\nend\ny()\n";
    assert_eq!(run(source), "Done(1)");
}

/// The body decides nothing. This body never returns on any path, and
/// the loop still ends the function.
#[test]
fn a_loop_that_never_returns_still_ends_a_body() {
    let source = "def y(): Never\n  loop do\n    ()\n  end\nend\n1\n";
    assert_eq!(run(source), "Done(1)");
}

/// A `break` gives the loop a normal exit, so its type stays `()`.
#[test]
fn a_loop_with_a_break_keeps_the_unit_type() {
    let source = "def y(): Int\n  loop do\n    break\n  end\nend\n1\n";
    expect_error(source, "expected Int, found ()");
}

/// A `break` inside a nested loop belongs to that loop.
#[test]
fn a_break_of_a_nested_loop_does_not_exit_the_outer_loop() {
    let source = "def y(): Never\n  loop do\n    loop do\n      break\n    end\n  end\nend\n1\n";
    assert_eq!(run(source), "Done(1)");
}

/// A `break` inside a `case` arm still exits the loop.
#[test]
fn a_break_inside_a_case_arm_exits_the_loop() {
    let source = "def y(): Int\n  o: Option[Int] = Some(1)\n  loop do\n    \
        case o\n    in Some(_)\n      break\n    in None\n      ()\n    end\n  end\nend\n1\n";
    expect_error(source, "expected Int, found ()");
}

/// The declared result type needs a witness. Nothing in this body
/// produces an `Int`, so `Never` is the honest annotation.
#[test]
fn a_callable_that_never_returns_cannot_claim_another_type() {
    let source = "def y(): Int\n  loop do\n    ()\n  end\nend\n1\n";
    expect_error(source, "declare the result type `Never`");
}

/// A `return` witnesses the declared type, so a driver keeps its own
/// result type.
#[test]
fn a_return_witnesses_the_declared_type() {
    let source = "def y(): Int\n  loop do\n    return 3\n  end\nend\ny()\n";
    assert_eq!(run(source), "Done(3)");
}

/// A unit result claims no value, so it needs no `return`.
#[test]
fn a_unit_result_needs_no_return() {
    let source = "def y()\n  loop do\n    ()\n  end\nend\n1\n";
    assert_eq!(run(source), "Done(1)");
}

/// A tail after a loop with no exit cannot run.
#[test]
fn a_tail_after_an_endless_loop_is_unreachable() {
    let source = "def y(): Int\n  loop do\n    ()\n  end\n  0\nend\n1\n";
    expect_error(source, "unreachable");
}

/// A bounded loop still completes, so its tail stays reachable.
#[test]
fn a_bounded_loop_keeps_its_tail() {
    let source = "def y(): Int\n  i = 0\n  while i < 3\n    i = i + 1\n  end\n  i\nend\ny()\n";
    assert_eq!(run(source), "Done(3)");
}
