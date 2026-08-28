//! The `examples/15-compiler-and-hot-code-reloading` programs.
//!
//! Each checked output pins the behaviour its example claims. The
//! reload examples state an exact price list, so a change in slot
//! semantics shows up here as a changed list rather than as prose
//! that quietly stops being true.

use lm_host::CliHost;
use lm_testkit::publish_artifact_bytes;
use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::{RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

/// Run one example on the command-line host, which is the host that
/// answers `Compiler` and `Reflect`. The recording host does not
/// carry the compiler service.
fn run_example(path: &str, allow: &[&str]) -> String {
    let source = std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    let bytes = compile_to_bytes(path, &source).expect("the example compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the example loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(CliHost::new(1)),
    );
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

/// Run one example on the recording host with named files.
///
/// The recording host answers `Fs` operations from its file table,
/// so an example can read a file that the test wrote.
fn run_example_with_files(path: &str, allow: &[&str], files: &[(&str, Vec<u8>)]) -> String {
    let source = std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    run_source_with_files(path, &source, allow, files)
}

/// Run one source text on the recording host with named files.
fn run_source_with_files(
    path: &str,
    source: &str,
    allow: &[&str],
    files: &[(&str, Vec<u8>)],
) -> String {
    let bytes = compile_to_bytes(path, source).expect("the example compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    for (name, bytes) in files {
        host.borrow_mut().set_file(*name, bytes.clone());
    }
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
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
            &["Vm", "Io.ReadBytes"],
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
            &["Vm", "Io.ReadBytes"],
        ),
        "Done(Ok((0, 18)))"
    );
}

#[test]
fn a_debugger_opens_another_program() {
    // The debuggee is an unrelated program. The test captures it before
    // it runs, writes the bytes as `program.lms`, and the debugger reads
    // them. The debugger knows no debuggee definition; it names the top
    // frame source, answers the one request, and renders the result.
    let snapshot = independent_program_snapshot();

    assert_eq!(
        run_example_with_files(
            "examples/15-compiler-and-hot-code-reloading/12-debug-another-program.lm",
            &["Fs.Open", "Fs.Read", "Fs.Close", "Vm"],
            &[("program.lms", snapshot)],
        ),
        "Done(Ok((\"independent-program.lm\", \"Clock.Now -> 42\")))"
    );
}

/// Capture one unrelated program before it runs.
fn independent_program_snapshot() -> Vec<u8> {
    let source = "def calculate(): Int with Clock.Now\n  sys.clock.now()\n  42\nend\ncalculate()\n";
    program_snapshot("independent-program.lm", source)
}

/// Capture one program before it runs.
fn program_snapshot(name: &str, source: &str) -> Vec<u8> {
    let bytes = compile_to_bytes(name, source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let gate = world.next_gate();
    let snapshot = world
        .capture_snapshot(gate, 0, false)
        .expect("the program captures");
    snapshot.bytes().expect("the snapshot encodes").to_vec()
}

#[test]
fn a_dynamic_restore_of_a_full_vm_snapshot_is_an_ordinary_error() {
    // A full VM snapshot selects no run. The dynamic restore reports
    // that as a value, because arbitrary bytes are ordinary input.
    let source = "def go(): Result[String, String] with Vm\n\
      image = sys.vm.Vm()\n\
      snapshot = case image.snapshot()\n\
      in Ok(value) then value\n\
      in Err(problem) then return Err(display(problem))\n\
      end\n\
      case sys.vm.Vm().restore_dynamic(snapshot)\n\
      in Ok(_) then Ok(\"restored\")\n\
      in Err(problem) then Err(display(problem))\n\
      end\n\
    end\n\
    go()\n";
    assert_eq!(
        run_source_with_files("full-vm.lm", source, &["Vm"], &[]),
        "Done(Err(\"the snapshot does not select a run to restore\"))"
    );
}

#[test]
fn a_dynamic_run_keeps_its_result_view_across_a_snapshot() {
    // A snapshot of a dynamic run restores as a dynamic run. The typed
    // restore delivers the result as a `DynValue`, because the flag
    // rides in the image.
    let source = "def read_snapshot(): Result[Bytes, String] with Fs.Open, Fs.Read, Fs.Close\n\
      file = sys.fs.open(\"program.lms\", ReadOnly).map_error() {\n\
        |problem: FsError| display(problem)\n\
      }?\n\
      bytes = file.read(1048576).map_error() {\n\
        |problem: FsError| display(problem)\n\
      }\n\
      file.close()\n\
      bytes\n\
    end\n\
    \n\
    def go(): Result[String, String] with Fs.Open, Fs.Read, Fs.Close, Vm\n\
      bytes = read_snapshot()?\n\
      snapshot = sys.vm.load_snapshot(bytes).map_error() {\n\
        |problem: SnapshotError| display(problem)\n\
      }?\n\
      run = sys.vm.Vm().restore_dynamic(snapshot).map_error() {\n\
        |problem: RestoreError| display(problem)\n\
      }?\n\
      again = case run.snapshot()\n\
      in Ok(value) then value\n\
      in Err(problem) then return Err(display(problem))\n\
      end\n\
      restored = sys.vm.Vm().restore(again).map_error() {\n\
        |problem: RestoreError| display(problem)\n\
      }?\n\
      loop do\n\
        case restored.drive()\n\
        in Asked(request)\n\
          case request\n\
          in Call(Clock.Now, call, ())\n\
            restored.answer(call, 100)\n\
          in _\n\
            restored.reject(request, Fault.denied(\"rejected\"))\n\
          end\n\
        in Done(value)\n\
          return Ok(value.render())\n\
        in Fault(problem)\n\
          return Err(problem.code())\n\
        end\n\
      end\n\
    end\n\
    go()\n";
    assert_eq!(
        run_source_with_files(
            "dynamic-round-trip.lm",
            source,
            &["Fs.Open", "Fs.Read", "Fs.Close", "Vm"],
            &[("program.lms", independent_program_snapshot())],
        ),
        "Done(Ok(\"42\"))"
    );
}

#[test]
fn a_debugger_renders_a_result_of_a_class_it_does_not_know() {
    // The debuggee returns an instance of its own class. The debugger
    // never saw that class. The `DynValue` carries the runtime type,
    // and the world resolves it, so the debugger renders the value.
    let source = "class Box\n  value: Int = 41\nend\n\
      def calculate(): Box with Clock.Now\n  sys.clock.now()\n  Box()\nend\ncalculate()\n";
    assert_eq!(
        run_example_with_files(
            "examples/15-compiler-and-hot-code-reloading/12-debug-another-program.lm",
            &["Fs.Open", "Fs.Read", "Fs.Close", "Vm"],
            &[("program.lms", program_snapshot("box.lm", source))],
        ),
        "Done(Ok((\"box.lm\", \"Clock.Now -> Box{value: 41}\")))"
    );
}

#[test]
fn a_debugger_that_holds_a_foreign_result_snapshots_and_restores() {
    fn finish_restored(world: &mut World, root: lm_vm::VmId) {
        loop {
            match world.run_machine(root) {
                lm_vm::RootEvent::Done(value) => {
                    assert_eq!(world.show_result_of(root, value), "Ok(Box{value: 41})");
                    break;
                }
                lm_vm::RootEvent::Fault(rec) => {
                    panic!("the restored debugger faulted: {rec:?}")
                }
                lm_vm::RootEvent::Blocked if world.poll_blocked() > 0 => {}
                other => panic!("the restored debugger stopped: {other:?}"),
            }
        }
    }

    // The debugger keeps the result of a class it never saw, then
    // captures itself. The capture closes over the debuggee machine,
    // like a run handle. A fresh world with only the runtime core
    // restores the capture and renders the value through the
    // debuggee's own code.
    let debuggee = "class Box\n  value: Int = 41\nend\n\
      def calculate(): Box with Clock.Now\n  sys.clock.now()\n  Box()\nend\ncalculate()\n";
    let debugger = "def read_snapshot(): Result[Bytes, String] with Fs.Open, Fs.Read, Fs.Close\n\
      file = sys.fs.open(\"program.lms\", ReadOnly).map_error() {\n\
        |problem: FsError| display(problem)\n\
      }?\n\
      bytes = file.read(1048576).map_error() {\n\
        |problem: FsError| display(problem)\n\
      }\n\
      file.close()\n\
      bytes\n\
    end\n\
    \n\
    def finish(run: Run[DynValue]): Result[DynValue, String] with Vm\n\
      loop do\n\
        case run.drive()\n\
        in Asked(request)\n\
          case request\n\
          in Call(Clock.Now, call, ()) then run.answer(call, 100)\n\
          in _ then run.reject(request, Fault.denied(\"rejected\"))\n\
          end\n\
        in Done(value) then return Ok(value)\n\
        in Fault(problem) then return Err(problem.code())\n\
        end\n\
      end\n\
    end\n\
    \n\
    def go(): Result[DynValue, String] with Fs.Open, Fs.Read, Fs.Close, Vm\n\
      bytes = read_snapshot()?\n\
      snapshot = sys.vm.load_snapshot(bytes).map_error() {\n\
        |problem: SnapshotError| display(problem)\n\
      }?\n\
      run = sys.vm.Vm().restore_dynamic(snapshot).map_error() {\n\
        |problem: RestoreError| display(problem)\n\
      }?\n\
      Ok(finish(run)?)\n\
    end\n\
    go()\n";
    let bytes = compile_to_bytes("holder.lm", debugger).expect("the debugger compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the debugger loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("program.lms", program_snapshot("box.lm", debuggee));
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for grant in ["Fs.Open", "Fs.Read", "Fs.Close", "Vm"] {
        world.allow(grant).expect("the grant names a target");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Ok(Box{value: 41}))");
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the debugger captures with its foreign result");
    let bytes = image.bytes().expect("the capture encodes").to_vec();

    // A world with the runtime core alone.
    let core = lm_compiler::core_link_unit().expect("the core builds");
    let mut fresh_arena = lm_link::CodeArena::new();
    let core_namespace = fresh_arena
        .publish_verified(
            lm_bytecode::artifact::Artifact::new_shared(core, Vec::new())
                .expect("the core artifact is valid"),
            None,
        )
        .expect("the core publishes");
    let mut fresh_config = VmConfig::default();
    fresh_config.max_children += 1;
    let mut fresh = World::new(
        fresh_arena,
        core_namespace,
        fresh_config,
        Box::new(RecordingHost::new(1)),
    );
    let admitted = fresh
        .load_snapshot_bytes(&bytes)
        .expect("the capture admits in a core-only world");
    let target = fresh.new_child(0).expect("the restore target exists");
    let root = fresh
        .restore_image(0, target, &admitted)
        .expect("the capture restores");
    let gate = fresh.next_gate();
    let round_trip = fresh
        .capture_snapshot(gate, root, false)
        .expect("the restored debugger captures again");
    let round_trip_bytes = round_trip.bytes().expect("the round-trip image has bytes");
    assert_eq!(round_trip_bytes.as_slice(), bytes.as_slice());
    finish_restored(&mut fresh, root);

    // The first restore published both foreign chains after the core.
    // Repeated admission must still use the container's table layout.
    let repeated = fresh
        .load_snapshot_bytes(&bytes)
        .expect("world history does not change admission");
    assert_eq!(
        repeated.bytes().expect("the repeated image has bytes"),
        admitted.bytes().expect("the first image has bytes")
    );
    let target = fresh.new_child(0).expect("the second target exists");
    let root = fresh
        .restore_image(0, target, &repeated)
        .expect("the repeated image restores");
    finish_restored(&mut fresh, root);
}
