//! The `examples/15-compiler-and-hot-code-reloading` programs.
//!
//! Each checked output pins the behaviour its example claims. The
//! reload examples state an exact price list, so a change in slot
//! semantics shows up here as a changed list rather than as prose
//! that quietly stops being true.

use lm_host::CliHost;
use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::{load_bytes, VmConfig, World};

/// Run one example on the command-line host, which is the host that
/// answers `Compiler` and `Reflect`. The recording host does not
/// carry the compiler service.
fn run_example(path: &str, allow: &[&str]) -> String {
    let source = std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    let bytes = compile_to_bytes(path, &source).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(CliHost::new(1)));
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn the_pipeline_example_names_each_step() {
    let out = run_example(
        "examples/15-compiler-and-hot-code-reloading/01-compile-at-runtime.lm",
        &["Compiler", "Vm"],
    );
    // Two programs run, one fails to compile, and one fails the
    // typed entry lookup before anything runs.
    assert!(out.starts_with("Done([Ok(42), Ok(42), Err("), "{out}");
    assert!(out.contains("E1001"), "{out}");
    assert!(
        out.contains("does not match the requested monomorphic contract"),
        "{out}"
    );
}

#[test]
fn the_evaluator_example_classifies_each_line() {
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/02-a-small-evaluator.lm",
            &["Reflect", "Compiler", "Vm"],
        ),
        "Done([\"42\", \"...\", \"...\", \"defined\", \"\\\"loom\\\"\", \"...\", \"syntax error\"])"
    );
}

#[test]
fn a_program_redefines_its_own_class() {
    // `Box().amount()` answered 5 + 1. The revision compiles under
    // the module of the original, so it lands in the same nominal
    // family. Its implementation identity changes.
    // Its contract identity remains equal.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/03-redefine-your-own-code.lm",
            &["Compiler", "Vm"],
        ),
        "Done(Ok((6, 51)))"
    );
}

#[test]
fn the_open_request_keeps_its_own_version() {
    // The open call answers 10 * 3. The next call reads the new slot
    // target and answers 10 * 3 + 1000.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/04-finish-the-open-request.lm",
            &["Vm", "Io.ReadLine"],
        ),
        "Done(Ok([30, 1030]))"
    );
}

#[test]
fn untrusted_code_gets_no_authority() {
    let out = run_example(
        "examples/15-compiler-and-hot-code-reloading/05-run-untrusted-code.lm",
        &["Compiler", "Vm"],
    );
    // The computing rule runs. The rule that names an outside module
    // never compiles. The rule that prints compiles and verifies, and
    // the run policy stops it.
    assert!(out.starts_with("Done([Ok(42), Err("), "{out}");
    assert!(out.contains("E1052"), "{out}");
    assert!(out.contains("PolicyDenied"), "{out}");
}

#[test]
fn generated_code_compiles_and_invalid_trees_reject() {
    // The builder wrote `10 + 20 + 12`, which runs to 42. A second
    // table runs to 3. The builder also made a tree the grammar
    // rejects, and the compiler refused it.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/07-generate-code-from-data.lm",
            &["Compiler", "Vm"],
        ),
        "Done((\"10 + 20 + 12\\n\", [Ok(42), Ok(3)], true))"
    );
}

#[test]
fn a_rewrite_keeps_every_other_byte() {
    // The edit landed on one token and every other byte survived,
    // the original definition answered 10, and the rewritten one
    // that replaced it answers 25.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/08-rewrite-source-safely.lm",
            &["Compiler", "Vm"],
        ),
        "Done(Ok((true, 10, 25)))"
    );
}

#[test]
fn the_proc_example_upgrades_a_live_process() {
    // Every definition is ordinary Loom. Installing them binds the
    // worker's call to `rate`, so moving that binding changes what
    // the running worker reaches next. The first order priced at
    // twice the amount, the second added the fee, and one worker
    // served both.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/06-upgrade-a-running-proc.lm",
            &["Vm", "Proc"],
        ),
        "Done(Ok((20, 30, 2)))"
    );
}

#[test]
fn a_batch_publishes_both_halves_or_neither() {
    // The shown price and the charged price move together, so the
    // pair never mixes releases. The stale batch that follows is
    // refused whole: the shown price carries the single replace that
    // disturbed it, and the charged price still carries the fee.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/09-change-definitions-together.lm",
            &["Vm"],
        ),
        "Done(Ok(((20, 20), (30, 30), \"a slot change is stale\", (10, 30))))"
    );
}

#[test]
fn identity_separates_shape_from_body() {
    // A renamed parameter and an added comment move neither half of
    // a definition identity. Different instructions for the same
    // answer move the body and keep the shape. A wider effect row is
    // a different shape, so the compiler refuses it outright. And
    // the same source in two modules holds one identity, with
    // `module_hash` recording where each copy came from.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/10-when-two-definitions-match.lm",
            &["Compiler", "Vm"],
        ),
        "Done([\"renamed parameter: same shape=true same body=true\", \
         \"added a comment  : same shape=true same body=true\", \
         \"n + n            : same shape=true same body=false\", \
         \"added an effect  : the compiler refused the revision\", \
         \"same identity=true same module=false keys=alpha.rate and beta.rate\"])"
    );
}

#[test]
fn a_snapshot_accepts_an_exporter_after_the_fact() {
    // The shipped world discarded its total and answered 0. The same
    // capture, restored and given an exporter that did not exist when
    // it was taken, answered the 5 + 6 + 7 it had been holding. The
    // first answer was already in flight at the capture.
    assert_eq!(
        run_example(
            "examples/15-compiler-and-hot-code-reloading/11-recover-a-snapshot-after-the-fact.lm",
            &["Vm", "Io.ReadLine"],
        ),
        "Done(Ok((0, 18)))"
    );
}
