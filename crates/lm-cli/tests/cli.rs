//! End-to-end tests for the `lm` binary.

use std::path::Path;
use std::process::{Command, Output};

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn lm(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lm"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("the lm binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

#[test]
fn run_factorial_prints_done_3628800() {
    let out = lm(&["run", "--show-result", "examples/01-basics/factorial.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(3628800)\n");
}

#[test]
fn run_control_prints_done_4950() {
    let out = lm(&["run", "--show-result", "examples/01-basics/control.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(4950)\n");
}

#[test]
fn check_type_mismatch_prints_the_e1004_diagnostic() {
    let out = lm(&["check", "tests/ui/type-mismatch.lm"]);
    assert!(!out.status.success());
    assert_eq!(
        stderr(&out),
        "error[E1004]: expected Int, found String\n  --> tests/ui/type-mismatch.lm:2:5\n"
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn check_passes_on_a_valid_file() {
    let out = lm(&["check", "examples/01-basics/factorial.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
    assert_eq!(stderr(&out), "");
}

#[test]
fn disasm_prints_signatures_blocks_and_jump_targets() {
    let out = lm(&["disasm", "examples/01-basics/factorial.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("fn0 factorial(Int) -> Int"), "{text}");
    assert!(text.contains("entry fn1"), "{text}");
    assert!(text.contains("b0:"), "{text}");
    assert!(text.contains("JumpIfFalse -> b"), "{text}");
    assert!(text.contains("; pop 2 push 1"), "{text}");
}

#[test]
fn run_reports_a_fault_with_a_stable_code() {
    let out = lm(&["run", "--show-result", "tests/run-fault/divide-by-zero.lm"]);
    assert!(!out.status.success());
    assert_eq!(stdout(&out), "Fault(DivideByZero)\n");
}

#[test]
fn run_with_a_small_fuel_budget_faults_with_out_of_fuel() {
    let out = lm(&[
        "run",
        "--show-result",
        "--fuel",
        "3",
        "examples/01-basics/control.lm",
    ]);
    assert!(!out.status.success());
    assert_eq!(stdout(&out), "Fault(OutOfFuel)\n");
}

#[test]
fn check_output_is_byte_identical_between_runs() {
    let a = lm(&["check", "tests/ui/type-mismatch.lm"]);
    let b = lm(&["check", "tests/ui/type-mismatch.lm"]);
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(a.stderr, b.stderr);
}

#[test]
fn unknown_command_prints_usage() {
    let out = lm(&["frobnicate"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("usage:"), "{}", stderr(&out));
}

#[test]
fn missing_file_is_an_ordinary_error() {
    let out = lm(&["check", "does-not-exist.lm"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("cannot read"), "{}", stderr(&out));
}
