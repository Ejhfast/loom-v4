//! Robustness tests for deep input and for strict `if` value rules.
//!
//! The parser must reject input that nests deeper than its limit.
//! It must return a diagnostic. It must not overflow the Rust stack.

use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

fn code_of(source: &str) -> String {
    let rendered = compile_text("t.lm", source).unwrap_err();
    rendered[6..11].to_string()
}

fn nested(open: &str, middle: &str, close: &str, depth: usize) -> String {
    let mut text = String::new();
    for _ in 0..depth {
        text.push_str(open);
    }
    text.push_str(middle);
    for _ in 0..depth {
        text.push_str(close);
    }
    text
}

/// Run the closure on a bounded Rust stack. The deep inputs below
/// need tens of megabytes without the depth guard, so a pass on this
/// stack proves the guard stops the recursion first.
fn on_small_stack<F: FnOnce() + Send + 'static>(f: F) {
    std::thread::Builder::new()
        .stack_size(4 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deep_parens_are_rejected_with_a_diagnostic() {
    on_small_stack(|| {
        let text = nested("(", "1", ")", 5_000) + "\n";
        assert_eq!(code_of(&text), "E1022");
    });
}

#[test]
fn deep_unary_chains_are_rejected_with_a_diagnostic() {
    on_small_stack(|| {
        let text = "not ".repeat(20_000) + "true\n";
        assert_eq!(code_of(&text), "E1022");
        let text = "-".repeat(20_000) + "1\n";
        assert_eq!(code_of(&text), "E1022");
    });
}

#[test]
fn deep_if_blocks_are_rejected_with_a_diagnostic() {
    on_small_stack(|| {
        let text = nested("if true\n", "x = 1\n", "end\n", 5_000) + "1\n";
        assert_eq!(code_of(&text), "E1022");
    });
}

#[test]
fn deep_while_blocks_are_rejected_with_a_diagnostic() {
    on_small_stack(|| {
        let text = nested("while false\n", "x = 1\n", "end\n", 5_000) + "1\n";
        assert_eq!(code_of(&text), "E1022");
    });
}

#[test]
fn moderate_nesting_compiles_and_runs() {
    let text = nested("(", "1", ")", 120) + "\n";
    let shown = run_text("t.lm", &text, VmConfig::default()).unwrap();
    assert_eq!(shown, "Done(1)");

    let text = nested("if true\n", "x = 1\n", "end\n", 80) + "7\n";
    let shown = run_text("t.lm", &text, VmConfig::default()).unwrap();
    assert_eq!(shown, "Done(7)");
}

#[test]
fn if_without_else_rejects_a_non_unit_branch_value() {
    // In a value position the `if` result is `()`. A branch that
    // produces `1` is an error, not a silent discard.
    assert_eq!(code_of("y = if true\n  1\nend\ny\n"), "E1004");
    // The same rule applies to the module entry expression.
    assert_eq!(code_of("if true\n  1\nend\n"), "E1004");
}

#[test]
fn if_without_else_accepts_unit_branches() {
    let text = "x = 0\nif true\n  x = 1\nend\nx\n";
    let shown = run_text("t.lm", text, VmConfig::default()).unwrap();
    assert_eq!(shown, "Done(1)");

    // The slice has no unit literal, so bind through a unit call.
    let text = "def f()\nend\ny = if true\n  f()\nend\n0\n";
    let shown = run_text("t.lm", text, VmConfig::default()).unwrap();
    assert_eq!(shown, "Done(0)");
}
