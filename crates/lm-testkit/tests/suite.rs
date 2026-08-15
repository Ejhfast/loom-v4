//! Directory-driven UI, run-pass, and run-fault suites, plus the
//! checked example outputs.

use lm_testkit::{repo_root, run_case, run_suite, run_text, ui_case};
use lm_vm::VmConfig;

#[test]
fn ui_suite() {
    run_suite(&repo_root().join("tests/ui"), ui_case);
}

#[test]
fn run_pass_suite() {
    run_suite(&repo_root().join("tests/run-pass"), |path| {
        run_case(path, VmConfig::default())
    });
}

#[test]
fn run_fault_suite() {
    run_suite(&repo_root().join("tests/run-fault"), |path| {
        run_case(path, VmConfig::default())
    });
}

fn run_example(name: &str) -> String {
    let path = repo_root().join(name);
    let text = std::fs::read_to_string(&path).expect("example exists");
    run_text(name, &text, VmConfig::default()).expect("example compiles")
}

#[test]
fn factorial_example_prints_3628800() {
    assert_eq!(
        run_example("examples/01-basics/factorial.lm"),
        "Done(3628800)"
    );
}

#[test]
fn control_example_prints_4950() {
    assert_eq!(run_example("examples/01-basics/control.lm"), "Done(4950)");
}
