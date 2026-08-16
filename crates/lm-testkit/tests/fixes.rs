//! Regression tests for the post-week-4 fix set:
//! declared local-type tables, labeled arguments, sibling inference,
//! `mut` markers in function types, the constructor-collision note,
//! and nested exact-arm exhaustiveness.

use lm_testkit::{compile_text, run_text};
use lm_vm::{LoadError, VmConfig};

fn run(source: &str) -> String {
    run_text("fixes.lm", source, VmConfig::default()).unwrap()
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
// ---------------------------------------------------------------

#[test]
fn nested_exact_arm_scrutinee_is_exhaustive() {
    let source = "s = Some(Some(3))\ncase s\nin Some(Some(v)) then v\nend\n";
    assert_eq!(run(source), "Done(3)");
}

#[test]
fn deeper_exact_arm_scrutinee_is_exhaustive() {
    let source = "s = Some(Some(Some(4)))\ncase s\nin Some(Some(Some(v))) then v\nend\n";
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
