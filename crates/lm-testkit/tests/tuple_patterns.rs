//! Tuple patterns.
//!
//! A tuple type has one constructor, so a tuple pattern is irrefutable
//! and needs no runtime test. Specification 14.8 reads request
//! arguments through one.

use lm_testkit::run_allowed;

fn run(source: &str) -> String {
    run_allowed("tuple.lm", source, &[]).expect("the program compiles")
}

#[test]
fn a_tuple_pattern_binds_every_element() {
    assert_eq!(
        run("case (7, \"hi\")\nin (n, s) then \"{n}-{s}\"\nend\n"),
        "Done(\"7-hi\")"
    );
}

#[test]
fn tuple_carriers_use_the_native_tuple_value() {
    let source = "pair: Tuple2[Int, String] = Tuple2(7, \"hi\")\n\
                  unit: Unit = Unit()\n\
                  (pair.swap(), unit)\n";
    assert_eq!(run(source), "Done(((\"hi\", 7), ()))");
}

#[test]
fn a_one_tuple_pattern_needs_its_comma() {
    assert_eq!(run("case (3,)\nin (n,) then n\nend\n"), "Done(3)");
    // Without the comma the parentheses only group, so this binds the
    // whole tuple to `n`.
    assert_eq!(
        run("case (3,)\nin (n) then n\nend\n"),
        "Done((3,))",
        "a parenthesized name is a binding, not a one-tuple"
    );
}

#[test]
fn a_tuple_pattern_nests() {
    assert_eq!(
        run("case (1, (2, 3))\nin (x, (y, z)) then x + y + z\nend\n"),
        "Done(6)"
    );
}

#[test]
fn a_tuple_pattern_is_exhaustive_by_itself() {
    // One arm covers the type, so the checker needs no wildcard and
    // reports no unreachable arm.
    assert_eq!(
        run("def pair(): (Int, Int)\n  (2, 5)\nend\ncase pair()\nin (a, b) then a * b\nend\n"),
        "Done(10)"
    );
}

#[test]
fn a_tuple_pattern_checks_its_arity() {
    let error = run_allowed("tuple.lm", "case (1, 2)\nin (a, b, c) then a\nend\n", &[])
        .expect_err("the arity is wrong");
    assert!(error.contains("E1041"), "{error}");
}

#[test]
fn a_tuple_pattern_needs_a_tuple_scrutinee() {
    let error =
        run_allowed("tuple.lm", "case 1\nin (a, b) then a\nend\n", &[]).expect_err("not a tuple");
    assert!(error.contains("E1041"), "{error}");
    assert!(
        error.contains("a tuple pattern cannot match a scrutinee of type Int"),
        "{error}"
    );
}

#[test]
fn a_tuple_pattern_refines_inside_a_constructor() {
    let source = "enum Pair\n\
                  \x20 Both(value: (Int, Int))\n\
                  end\n\
                  case Both((4, 6))\n\
                  in Both((a, b)) then a + b\n\
                  end\n";
    assert_eq!(run(source), "Done(10)");
}
