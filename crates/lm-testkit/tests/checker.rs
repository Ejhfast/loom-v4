//! Focused checker tests: one negative case for each new rule, plus
//! positive coverage for the lowered CFG dump.

use lm_testkit::{compile_text, run_text};
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
    assert_eq!(code_of("\"hi {x}\"\n"), "E0006");
    assert_eq!(code_of("0x\n"), "E0007");
    assert_eq!(code_of("'c'\n"), "E0008");
    assert_eq!(code_of("b\"x\"\n"), "E0009");
    assert_eq!(code_of("\"\"\"x\"\"\"\n"), "E0010");
    // Parser rules.
    assert_eq!(code_of("x = 1 y = 2\n"), "E1001");
    assert_eq!(code_of("enum Color\nend\n"), "E1002");
    assert_eq!(code_of("[1, 2]\n"), "E1002");
    assert_eq!(code_of("{1: 2}\n"), "E1002");
    assert_eq!(code_of("(1, 2)\n"), "E1002");
    assert_eq!(code_of("a.b\n"), "E1002");
    assert_eq!(code_of("do || 1 end\n"), "E1002");
    assert_eq!(code_of("if true\n1\n"), "E1003");
    // Checker rules.
    assert_eq!(code_of("1 + \"a\"\n"), "E1004");
    assert_eq!(code_of("not 1\n"), "E1004");
    assert_eq!(code_of("def f(): Int\n  true\nend\nf()\n"), "E1004");
    assert_eq!(code_of("def f(): Int\n  return\nend\nf()\n"), "E1004");
    assert_eq!(code_of("def f()\n  return 1\nend\nf()\n"), "E1004");
    assert_eq!(code_of("missing()\n"), "E1005");
    assert_eq!(code_of("nowhere\n"), "E1005");
    assert_eq!(code_of("def f(a: Int): Int\n  a\nend\nf()\n"), "E1006");
    assert_eq!(code_of("x = 1\nx(2)\n"), "E1007");
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
    assert_eq!(code_of("def f(): Int\n  1\nend\nf\n"), "E1018");
    assert_eq!(code_of("def f(): Int\n  1\nend\nf = 3\n"), "E1019");
    assert_eq!(code_of("x = 1\nx: Int = 2\n"), "E1020");
    assert_eq!(code_of("while true\n  break\n  x = 1\nend\n1\n"), "E1021");
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
fn cfg_dump_shows_signatures_blocks_and_jumps() {
    let module = compile_text(
        "t.lm",
        "def half(n: Int): Int\n  n / 2\nend\n\nx = 0\nwhile x < 4\n  x = x + 1\nend\nhalf(x)\n",
    )
    .unwrap();
    let dump = lm_hir::dump_cfg(&module);
    assert!(dump.contains("fn0 half(Int) -> Int"), "{dump}");
    assert!(dump.contains("fn1 <entry>() -> Int"), "{dump}");
    assert!(dump.contains("b1:"), "{dump}");
    assert!(dump.contains("JumpIfFalse -> b"), "{dump}");
    assert!(dump.contains("Call fn0"), "{dump}");
    assert!(dump.contains("pop 2 push 1"), "{dump}");
    // The dump is deterministic.
    assert_eq!(dump, lm_hir::dump_cfg(&module));
}

#[test]
fn printable_ast_is_available() {
    let ast = lm_source::parse::parse("x = 1\nx + 2\n").unwrap();
    let dump = lm_source::ast::dump_module(&ast);
    assert!(dump.contains("assign x"), "{dump}");
    assert!(dump.contains("binary +"), "{dump}");
}
