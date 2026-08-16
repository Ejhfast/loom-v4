//! Week-7 closure suites: the brace spelling, trailing closure
//! arguments, and the brace/pipe disambiguation.

use lm_testkit::{compile_text, compile_to_bytes, run_text};
use lm_vm::VmConfig;

fn runs(source: &str) -> String {
    run_text("t.lm", source, VmConfig::default()).unwrap()
}

fn code_of(source: &str) -> String {
    let rendered = compile_text("t.lm", source).unwrap_err();
    rendered[6..11].to_string()
}

// ---------------------------------------------------------------
// One node and one bytecode form for both spellings.
// ---------------------------------------------------------------

/// The two spellings differ only in the source text. The typed HIR
/// dump and the encoded bytecode must be identical.
#[test]
fn both_closure_spellings_produce_one_bytecode_form() {
    let with_do = "\
increment = do |x: Int|: Int
  x + 1
end

increment(41)
";
    let with_brace = "\
increment = { |x: Int|: Int
  x + 1
}

increment(41)
";
    assert_eq!(runs(with_do), "Done(42)");
    assert_eq!(runs(with_brace), "Done(42)");
    let a = compile_to_bytes("t.lm", with_do).expect("the do form compiles");
    let b = compile_to_bytes("t.lm", with_brace).expect("the brace form compiles");
    assert_eq!(a, b, "the two spellings must encode identically");
}

/// A trailing closure is the final call argument in either spelling,
/// and the two forms encode identically.
#[test]
fn both_trailing_spellings_produce_one_bytecode_form() {
    let with_do = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

with_value(41) do |x: Int|
  x + 1
end
";
    let with_brace = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

with_value(41) { |x: Int|
  x + 1
}
";
    let plain = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

with_value(41, do |x: Int|
  x + 1
end)
";
    assert_eq!(runs(with_do), "Done(42)");
    assert_eq!(runs(with_brace), "Done(42)");
    assert_eq!(runs(plain), "Done(42)");
    let a = compile_to_bytes("t.lm", with_do).expect("the do form compiles");
    let b = compile_to_bytes("t.lm", with_brace).expect("the brace form compiles");
    let c = compile_to_bytes("t.lm", plain).expect("the plain form compiles");
    assert_eq!(a, b);
    assert_eq!(a, c, "a trailing closure is the final ordinary argument");
}

/// A brace closure carries a header-line body, a multi-expression
/// body, an empty parameter list, a result type, and a row.
#[test]
fn brace_closures_accept_every_closure_part() {
    assert_eq!(runs("t = { || 42 }\nt()\n"), "Done(42)");
    assert_eq!(runs("f = { |x: Int|: Int x + 1 }\nf(41)\n"), "Done(42)");
    let multi = "\
f = { |x: Int|: Int
  y = x * 2
  y + 2
}

f(20)
";
    assert_eq!(runs(multi), "Done(42)");
    let row = "\
def go(): Int with Io.Print
  printer = { |text: String| with Io.Print
    sys.io.print(text)
  }
  printer(\"hello\\n\")
  42
end

go()
";
    assert_eq!(
        lm_testkit::run_allowed("t.lm", row, &["Io.Print"]).unwrap(),
        "Done(42)"
    );
}

/// A trailing closure works on a method call and on a call through a
/// closure value.
#[test]
fn a_trailing_closure_attaches_to_every_call_form() {
    let method = "\
class Box
  n: Int

  def init(mut self, n: Int)
    self.n = n
  end

  def map(self, f: (Int) -> Int): Int
    f(self.n)
  end
end

Box(21).map() { |x: Int| x * 2 }
";
    assert_eq!(runs(method), "Done(42)");
    let value = "\
apply = do |value: Int, body: (Int) -> Int|: Int
  body(value)
end

apply(41) { |x: Int| x + 1 }
";
    assert_eq!(runs(value), "Done(42)");
}

// ---------------------------------------------------------------
// Brace and pipe disambiguation.
// ---------------------------------------------------------------

/// A left brace followed by a pipe starts a brace closure. Every
/// other left brace stays a map literal, and `{}` stays the empty map.
#[test]
fn braces_still_start_map_literals() {
    assert_eq!(
        runs("counts = {\"a\": 1, \"b\": 2}\ncounts.len()\n"),
        "Done(2)"
    );
    assert_eq!(runs("empty: {String: Int} = {}\nempty.len()\n"), "Done(0)");
    // A map value inside a brace closure body still parses.
    let mixed = "\
f = { |k: String|: Int
  m = {\"a\": 1, \"b\": 2}
  m.at(k)
}

f(\"b\")
";
    assert_eq!(runs(mixed), "Done(2)");
    // A map type in a closure signature still parses.
    let typed = "\
f = { |m: {String: Int}|: Int m.len() }
f({\"a\": 1})
";
    assert_eq!(runs(typed), "Done(1)");
}

// ---------------------------------------------------------------
// The new rules reject.
// ---------------------------------------------------------------

/// A call accepts at most one trailing closure.
#[test]
fn a_second_trailing_closure_rejects() {
    let source = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

with_value(41) { |x: Int| x + 1 } { |y: Int| y }
";
    assert_eq!(code_of(source), "E1054");
}

/// No postfix suffix may follow a trailing closure.
#[test]
fn a_suffix_after_a_trailing_closure_rejects() {
    let base = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

";
    for suffix in [".to_text()", "(1)", "[0]"] {
        let source = format!("{base}with_value(41) {{ |x: Int| x + 1 }}{suffix}\n");
        assert_eq!(code_of(&source), "E1055", "{suffix}");
    }
}

/// A closure after a name without a call suffix is not a trailing
/// closure, so the statement does not end and the parser rejects.
#[test]
fn a_closure_without_a_call_suffix_rejects() {
    assert_eq!(code_of("x = 1\nx { |v: Int| v }\n"), "E1001");
    assert_eq!(code_of("x = [1]\nx[0] { |v: Int| v }\n"), "E1001");
}

/// A closure on the next line is a separate statement, so the call
/// keeps its written arity.
#[test]
fn a_closure_on_the_next_line_does_not_attach() {
    let source = "\
def with_value(value: Int, body: (Int) -> Int): Int
  body(value)
end

with_value(41)
  { |x: Int| x + 1 }
";
    assert_eq!(code_of(source), "E1006");
}

/// An unterminated brace closure names the closing brace.
#[test]
fn an_unterminated_brace_closure_rejects() {
    let rendered = compile_text("t.lm", "f = { |x: Int|: Int x + 1\n").unwrap_err();
    assert!(rendered.starts_with("error[E1003]"), "{rendered}");
    assert!(rendered.contains("`}`"), "{rendered}");
}

/// A brace closure without the closing pipe names the pipe.
#[test]
fn a_brace_closure_needs_its_parameter_list() {
    let rendered = compile_text("t.lm", "f = { |x: Int 1 }\nf(1)\n").unwrap_err();
    assert!(rendered.starts_with("error[E1003]"), "{rendered}");
}

/// The scanner opens a statement block for a brace closure and keeps
/// a map literal a delimiter. The two nest in every combination.
#[test]
fn brace_nesting_survives_every_combination() {
    // A multi-line brace closure inside a call argument list.
    let inside_parens = "\
def apply_twice(f: (Int) -> Int, value: Int): Int
  f(f(value))
end

apply_twice({ |x: Int|: Int
  y = x + 1
  y
}, 40)
";
    assert_eq!(runs(inside_parens), "Done(42)");
    // A multi-line map literal inside a brace closure body.
    let map_inside = "\
f = { |k: String|: Int
  m = {
    \"a\": 1,
    \"b\": 2
  }
  m.at(k)
}

f(\"b\")
";
    assert_eq!(runs(map_inside), "Done(2)");
    // A brace closure inside a brace closure.
    let nested = "\
outer = { |x: Int|: Int
  inner = { |y: Int|: Int
    y * 2
  }
  inner(x) + 2
}

outer(20)
";
    assert_eq!(runs(nested), "Done(42)");
    // Each spelling inside the other.
    let mixed = "\
a = { |x: Int|: Int
  g = do |y: Int|: Int
    y + 1
  end
  g(x)
}

b = do |x: Int|: Int
  h = { |y: Int|: Int y * 2 }
  h(x)
end

a(20) + b(11)
";
    assert_eq!(runs(mixed), "Done(43)");
    // An interpolated string inside a brace closure body. The
    // interpolation scanner rejects a brace of its own, and its
    // tokens carry spans into the enclosing source.
    let interp = "\
f = { |n: Int|: String
  \"n is {n}\"
}

f(42)
";
    assert_eq!(runs(interp), "Done(\"n is 42\")");
}

/// The scanner makes the brace decision once and reports it through
/// its own token, so the parser never repeats the test.
///
/// A closure brace may open on its own line or after a comment. The
/// body then keeps its statement separators, whatever the header
/// layout is. An earlier version tested the brace twice, in bytes and
/// in tokens, and the two disagreed across a line end.
#[test]
fn the_brace_decision_does_not_depend_on_the_header_layout() {
    let bodies = [
        "f = { |x: Int|: Int\n  y = x + 1\n  y + 100\n}\n",
        "f = {\n  |x: Int|: Int\n  y = x + 1\n  y + 100\n}\n",
        "f = { # a note\n  |x: Int|: Int\n  y = x + 1\n  y + 100\n}\n",
        "f = {\n  # a note\n  |x: Int|: Int\n  y = x + 1\n  y + 100\n}\n",
    ];
    for body in bodies {
        let source = format!("{body}f(41)\n");
        assert_eq!(runs(&source), "Done(142)", "{body}");
    }
    // The same layouts as a trailing closure.
    let trailing = [
        "with_value(41) { |x: Int|\n  y = x + 1\n  y + 100\n}\n",
        "with_value(41) {\n  |x: Int|\n  y = x + 1\n  y + 100\n}\n",
    ];
    for tail in trailing {
        let source = format!(
            "def with_value(value: Int, body: (Int) -> Int): Int\n  \
             body(value)\nend\n\n{tail}"
        );
        assert_eq!(runs(&source), "Done(142)", "{tail}");
    }
    // A brace that no pipe follows is still a map, whatever the
    // layout, and `{}` is still the empty map.
    assert_eq!(
        runs("m = {\n  \"a\": 1,\n  \"b\": 2\n}\nm.len()\n"),
        "Done(2)"
    );
    assert_eq!(runs("m: {String: Int} = {\n}\nm.len()\n"), "Done(0)");
    // A pipe inside a comment does not open a closure.
    assert_eq!(
        runs("m = { # a pipe | in a comment\n  \"a\": 1\n}\nm.len()\n"),
        "Done(1)"
    );
}
