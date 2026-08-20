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
    // The entry follows every class method, so the test reads its
    // index from the dump instead of pinning a constant.
    let index = text
        .lines()
        .find_map(|line| line.strip_prefix("entry fn"))
        .expect("the dump names the entry function")
        .to_string();
    assert!(
        text.contains(&format!("\nfn{index} <entry>() -> Int")),
        "{text}"
    );
    assert!(text.contains("b0:"), "{text}");
    assert!(text.contains("JumpIfFalse -> b"), "{text}");
    assert!(text.contains("; pop 2 push 1"), "{text}");
}

#[test]
fn run_counter_prints_done_5() {
    let out = lm(&["run", "--show-result", "examples/02-objects/counter.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(5)\n");
}

#[test]
fn run_counts_prints_the_word_counts() {
    let out = lm(&["run", "--show-result", "examples/02-objects/counts.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Done({\"red\": 3, \"blue\": 2, \"green\": 1})\n"
    );
}

#[test]
fn run_closures_prints_done_42() {
    let out = lm(&["run", "--show-result", "examples/02-objects/closures.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(42)\n");
}

#[test]
fn run_expr_example_prints_done_42() {
    let out = lm(&["run", "--show-result", "examples/03-types/expr.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(42)\n");
}

#[test]
fn run_generics_example_prints_the_tuple() {
    let out = lm(&["run", "--show-result", "examples/03-types/generics.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done((\"yes\", \"no\"))\n");
}

#[test]
fn disasm_covers_week3_surfaces() {
    let out = lm(&["disasm", "examples/03-types/generics.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("class"), "{text}");
    assert!(text.contains("abstract"), "{text}");
    assert!(text.contains("case"), "{text}");
    assert!(text.contains("app app0"), "{text}");
    assert!(text.contains("CallG"), "{text}");
    assert!(text.contains("TupleNew"), "{text}");
}

#[test]
fn inspect_live_dumps_heap_objects_and_stats() {
    let out = lm(&["inspect", "--live", "examples/02-objects/counter.lm"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("outcome: Done(5)"), "{text}");
    assert!(text.contains("heap: live="), "{text}");
    assert!(text.contains("collections="), "{text}");
    assert!(text.contains("frames: 0 active"), "{text}");
    assert!(
        text.contains("Instance mutable Counter{value: 5}"),
        "{text}"
    );
    // The dump is deterministic.
    let again = lm(&["inspect", "--live", "examples/02-objects/counter.lm"]);
    assert_eq!(out.stdout, again.stdout);
}

#[test]
fn inspect_without_live_is_rejected() {
    let out = lm(&["inspect", "examples/02-objects/counter.lm"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--live"), "{}", stderr(&out));
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

/// **The single-file module path rule.** A single source file has no
/// module path, and `lm check`, `lm run <file>.lm`, and
/// `lm build <file>.lm` all apply it. One file therefore gives one
/// module, whichever command a user runs.
///
/// The proof is a byte comparison: the listing of the source and the
/// listing of the artifact `lm build` wrote must be equal.
#[test]
fn every_single_file_command_compiles_one_module() {
    let source = "examples/02-objects/counter.lm";
    for command in ["check", "build"] {
        let out = lm(&[command, source]);
        assert!(out.status.success(), "{command}: {}", stderr(&out));
    }
    let run = lm(&["run", "--show-result", source]);
    assert!(run.status.success(), "{}", stderr(&run));
    let from_source = lm(&["disasm", source]);
    let from_artifact = lm(&["disasm", "build/debug/counter.lma"]);
    assert!(from_source.status.success(), "{}", stderr(&from_source));
    assert!(from_artifact.status.success(), "{}", stderr(&from_artifact));
    assert_eq!(
        stdout(&from_source),
        stdout(&from_artifact),
        "`lm build` and `lm run` compile one file two ways"
    );
    // The file name never reaches a qualified name.
    let text = stdout(&from_source);
    assert!(text.contains("binding Counter.add"), "{text}");
    assert!(!text.contains("counter.Counter"), "{text}");
}

/// A file name is not a module name. A file named `core.lm` carries no
/// module path, so it never takes the path the core image reserves,
/// and every single-file command accepts it.
#[test]
fn a_file_named_core_compiles_through_every_single_file_command() {
    let dir = repo_root().join("build/single-file");
    std::fs::create_dir_all(&dir).expect("the directory is created");
    std::fs::write(
        dir.join("core.lm"),
        "class Point\n  x: Int = 2\nend\nPoint().x\n",
    )
    .expect("the source is written");
    let path = "build/single-file/core.lm";
    for command in ["check", "build"] {
        let out = lm(&[command, path]);
        assert!(out.status.success(), "{command}: {}", stderr(&out));
    }
    let run = lm(&["run", "--show-result", path]);
    assert!(run.status.success(), "{}", stderr(&run));
    assert_eq!(stdout(&run), "Done(2)\n");
}

#[test]
fn run_the_worker_example_prints_done_done_42() {
    let out = lm(&[
        "run",
        "--show-result",
        "examples/07-procs/worker.lm",
        "--allow",
        "Proc",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(Done(42))\n");
    // The scheduler reads no clock, so a second run agrees.
    let again = lm(&[
        "run",
        "--show-result",
        "examples/07-procs/worker.lm",
        "--allow",
        "Proc",
    ]);
    assert_eq!(out.stdout, again.stdout);
}

/// A proc program survives the artifact round trip: the linker
/// relocates the parent type arguments and the handle types, and the
/// loader admits the result.
#[test]
fn a_proc_program_runs_from_its_artifact() {
    let build = lm(&["build", "examples/07-procs/worker.lm"]);
    assert!(build.status.success(), "{}", stderr(&build));
    let out = lm(&[
        "run",
        "--show-result",
        "build/debug/worker.lma",
        "--allow",
        "Proc",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(Done(42))\n");
}

/// A proc program builds and runs through the package path as well.
#[test]
fn a_proc_package_builds_and_runs() {
    let root = repo_root().join("target/test-proc-package");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("the package directory");
    std::fs::write(
        root.join("lm.package"),
        "[package]\nname = \"procapp\"\nversion = \"0.1.0\"\n",
    )
    .expect("the manifest writes");
    std::fs::write(
        root.join("src/main.lm"),
        "class Worker < Proc[Int]\n  \
         def on_spawn(self): Int with Proc\n    \
         case self.receive()\n    \
         in Msg(n)\n      n * 2\n    \
         in Closed\n      0\n    \
         end\n  end\nend\n\n\
         h = Worker.spawn()\nh.send(21)\nh.close()\n\
         case h.done()\nin Done(v)  then v\nin Fault(_) then 0\nend\n",
    )
    .expect("the source writes");
    let path = root.display().to_string();
    let out = lm(&["run", "--show-result", &path, "--allow", "Proc"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).ends_with("Done(42)\n"), "{}", stdout(&out));
}

// ---------------------------------------------------------------
// Week 9: the snapshot tools.
// ---------------------------------------------------------------

#[test]
fn run_branch_prints_two_restored_results() {
    let out = lm(&[
        "run",
        "--show-result",
        "examples/08-snapshots/branch.lm",
        "--allow",
        "Vm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done((42, 42))\n");
}

#[test]
fn run_machine_world_prints_three_equal_results() {
    let out = lm(&[
        "run",
        "--show-result",
        "examples/08-snapshots/machine-world.lm",
        "--allow",
        "Proc,Vm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done((42, 42, 42))\n");
}

#[test]
fn run_http_codec_example_prints_the_parsed_response() {
    let out = lm(&[
        "run",
        "--show-result",
        "examples/12-network-effects/01-http-codec.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done((112, \"200 world\"))\n");
}

#[test]
fn run_tcp_loopback_example_moves_plaintext() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Tcp",
        "examples/12-network-effects/02-tcp-loopback.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(\"hello\")\n");
}

#[test]
fn run_tls_driver_example_exposes_lower_requests() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Vm",
        "examples/12-network-effects/03-drive-tls.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "Done(5)\n");
}

/// Run one `examples/13-collections-and-interfaces` program and
/// return its result line. None of these examples needs a grant.
fn collections_example(name: &str) -> String {
    let path = format!("examples/13-collections-and-interfaces/{name}");
    let out = lm(&["run", "--show-result", &path]);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out)
}

#[test]
fn run_report_example_folds_and_groups() {
    assert_eq!(
        collections_example("01-build-a-report.lm"),
        "Done((460, [\"north\", \"north\", \"east\"], 200, 200, \
         [\"north\", \"south\", \"east\"]))\n"
    );
}

#[test]
fn run_iteration_example_reads_every_source() {
    assert_eq!(
        collections_example("02-iterate-anything.lm"),
        "Done((10, \"bbb\", 5, 10, (\"7\", \"8\", \"end\")))\n"
    );
}

#[test]
fn run_interface_example_uses_one_and_two_bounds() {
    assert_eq!(
        collections_example("03-define-an-interface.lm"),
        "Done((\"book loom costs 12\", \"seat 14 costs 40\", 35, \
         \"book atlas costs 12\", 24))\n"
    );
}

#[test]
fn run_custom_iterator_example_drives_a_user_type() {
    assert_eq!(
        collections_example("04-your-own-iterator.lm"),
        "Done((10, 3, 2, 5, 1))\n"
    );
}

#[test]
fn run_views_example_reads_without_copying() {
    assert_eq!(
        collections_example("05-views-without-copies.lm"),
        "Done((90, 3, 20, [20, 30, 40], [\"ada\", \"bob\", \"cy\"], \
         [90, 72, 84], 90))\n"
    );
}

#[test]
fn run_mutation_example_changes_a_collection_safely() {
    assert_eq!(
        collections_example("06-change-while-you-read.lm"),
        "Done(([2, 4, 6], [\"loom\", \"atlas\"], [1, 4, 9], 2, [1, 2, 3]))\n"
    );
}

#[test]
fn run_closure_example_needs_no_annotation() {
    assert_eq!(
        collections_example("07-closures-that-cost-nothing.lm"),
        "Done((12, 81, [6, 7, 8], true, false))\n"
    );
}

#[test]
fn run_http_server_example_routes_three_requests() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Proc,Tcp",
        "examples/12-network-effects/04-http-server.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Done(\"200 ok | 200 Loom | 404 no route for GET /nowhere | \
         the server answered 3 requests\")\n"
    );
}

#[test]
fn run_fake_origin_example_needs_no_socket() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Vm",
        "examples/12-network-effects/07-test-without-a-network.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Done((\"200 text/plain ready\", \"503 text/plain down\"))\n"
    );
}

#[test]
fn run_egress_allowlist_example_refuses_two_hosts() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Vm",
        "examples/12-network-effects/08-egress-allowlist.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Done(\"api.internal: 200 ok | \
         data.example.com: data.example.com is not on the egress list | \
         metrics.vendor.net: metrics.vendor.net is not on the egress list\")\n"
    );
}

// The two cases below reach the public internet. They are `#[ignore]`
// so an offline build stays green. Run them with
// `cargo test -p lm-cli -- --ignored`.

#[test]
#[ignore]
fn run_fetch_https_example_reads_a_real_page() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Http.Client",
        "examples/12-network-effects/05-fetch-https.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    // The page length can change, so the check pins the stable parts.
    let text = stdout(&out);
    assert!(
        text.starts_with("Done((\"example.com/ -> 200 text/html "),
        "{text}"
    );
    assert!(text.ends_with(" bytes\", true))\n"), "{text}");
}

#[test]
#[ignore]
fn run_certificate_rules_example_refuses_four_hosts() {
    let out = lm(&[
        "run",
        "--show-result",
        "--allow",
        "Http.Client",
        "examples/12-network-effects/06-certificate-rules.lm",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let refused = "\"refused: the certificate failed the check\"";
    assert_eq!(
        stdout(&out),
        format!("Done(({refused}, {refused}, {refused}, {refused}, \"accepted, status 200\"))\n")
    );
}

#[test]
fn snapshot_verify_reports_the_checkpoint_world() {
    let out = lm(&["snapshot", "verify", "checkpoints/asked-tree.lms"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "valid: state=asked machines=3 mailboxes=2\n");
}

#[test]
fn snapshot_run_restores_the_checkpoint_world() {
    let out = lm(&[
        "snapshot",
        "run",
        "--allow",
        "Proc,Vm,Clock",
        "checkpoints/asked-tree.lms",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    // The restored root holds the request the capture preserved.
    assert_eq!(stdout(&out), "Asked(Clock.Now)\n");
}

#[test]
fn snapshot_save_rewrites_the_checkpoint_byte_for_byte() {
    let dir = std::env::temp_dir().join(format!("lm-snapshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory exists");
    let out_path = dir.join("asked-tree.lms");
    let _ = std::fs::remove_file(&out_path);
    let out = lm(&[
        "snapshot",
        "save",
        "--allow",
        "Proc,Vm,Clock",
        "checkpoints/asked-tree.lm",
        out_path.to_str().expect("a valid path"),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("valid: state=asked machines=3 mailboxes=2"));
    // The canonical form is reproducible: the checked-in checkpoint
    // and a fresh capture are the same byte string.
    let fresh = std::fs::read(&out_path).expect("the tool wrote the file");
    let stored = std::fs::read(repo_root().join("checkpoints/asked-tree.lms"))
        .expect("the checkpoint reads");
    assert_eq!(fresh, stored);
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn inspect_dumps_the_checkpoint_container() {
    let out = lm(&["inspect", "checkpoints/asked-tree.lms"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let dump = stdout(&out);
    assert!(dump.starts_with("container 901 bytes hash "), "{dump}");
    assert!(dump.contains("machine 0 state asked"), "{dump}");
    assert!(dump.contains("pending Clock.Now"), "{dump}");
    assert!(dump.contains("obj 1 Handle frozen proc 1.0"), "{dump}");
    // The dump repeats exactly.
    let again = lm(&["inspect", "checkpoints/asked-tree.lms"]);
    assert_eq!(stdout(&again), dump);
}

#[test]
fn inspect_shapes_lists_the_snapshot_shape() {
    let out = lm(&["inspect", "--shapes"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out)
        .contains("15 Snapshot refs=false born_frozen=true boundary=sendable digestible=false"));
}

#[test]
fn snapshot_verify_rejects_a_damaged_container() {
    let dir = std::env::temp_dir().join(format!("lm-damaged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the scratch directory exists");
    // The damaged copy sits beside a copy of its program, so the tool
    // finds the program the container names.
    std::fs::copy(
        repo_root().join("checkpoints/asked-tree.lm"),
        dir.join("asked-tree.lm"),
    )
    .expect("the program copies");
    let mut bytes = std::fs::read(repo_root().join("checkpoints/asked-tree.lms"))
        .expect("the checkpoint reads");
    let at = bytes.len() / 2;
    bytes[at] ^= 1;
    let path = dir.join("asked-tree.lms");
    std::fs::write(&path, &bytes).expect("the damaged copy writes");
    let out = lm(&["snapshot", "verify", path.to_str().expect("a valid path")]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("container hash"), "{}", stderr(&out));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(dir.join("asked-tree.lm"));
    let _ = std::fs::remove_dir(&dir);
}
