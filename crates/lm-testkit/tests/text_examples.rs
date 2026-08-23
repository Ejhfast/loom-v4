//! The `examples/11-text-and-bytes` programs.
//!
//! Each one shows a pattern a program writes over the text surface,
//! and each checked output pins the behaviour the example claims.

use lm_testkit::{repo_root, run_text};
use lm_vm::VmConfig;

fn run_example(path: &str) -> String {
    let source = std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    run_text(path, &source, VmConfig::default()).expect("the example runs")
}

#[test]
fn the_text_examples_run() {
    assert_eq!(
        run_example("examples/11-text-and-bytes/01-parse-config.lm"),
        "Done(\"loom on 8080 with 3 settings\")"
    );
    assert_eq!(
        run_example("examples/11-text-and-bytes/02-decode-untrusted.lm"),
        "Done(\"text[hi] not utf8, 2 bytes durable[hi]\")"
    );
    assert_eq!(
        run_example("examples/11-text-and-bytes/03-read-headers.lm"),
        "Done(\"/api/users declares 5 and carries 5\")"
    );
    assert_eq!(
        run_example("examples/11-text-and-bytes/04-build-a-report.lm"),
        "Done(\"name       count\\nALPHA      3\\nBETA       11\\nGAMMA      7\\ntotal      21\")"
    );
    assert_eq!(
        run_example("examples/11-text-and-bytes/05-slice-without-copy.lm"),
        "Done(\"view=loom durable=loom window=8 owned=8 source=2011\")"
    );
    assert_eq!(
        run_example("examples/11-text-and-bytes/06-binary-literals.lm"),
        "Done(\"frame {v1}: kind=144 raw=89504e470d0a1a0a masked=76504e470d0a1a0a ratio=1.50\")"
    );
}
