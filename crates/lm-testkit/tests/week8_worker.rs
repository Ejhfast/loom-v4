//! The thread-backed scheduler baseline of specification 22.12.
//!
//! The whole machine world is built, driven, and dropped inside one
//! worker thread with a bounded stack. Only the rendered outcome comes
//! back, so no guest reference ever crosses the thread boundary.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

/// The worker runs a proc program, and the host thread agrees.
#[test]
fn a_worker_thread_runs_the_whole_world() {
    let source = "class Adder < Proc[Int]\n\
                  \x20 total: Int = 0\n\
                  \x20 def on_spawn(mut self): Int with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.total = self.total + n\n\
                  \x20     in Closed\n\
                  \x20       return self.total\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Adder.spawn()\n\
                  h.send(20)\n\
                  h.send(22)\n\
                  h.close()\n\
                  h.done()\n";
    let bytes = compile_to_bytes("worker.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let outcome = lm_proc::run_on_worker(
        &loaded,
        VmConfig::default(),
        &["Proc"],
        Box::new(|| Box::new(RecordingHost::new(1))),
    )
    .expect("the worker runs");
    assert!(!outcome.faulted);
    assert_eq!(outcome.text, "Done(Done(42))");
    // The same program on the host thread agrees, so the mode changes
    // no semantics.
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    let inline = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&inline), outcome.text);
}

/// Deep guest recursion inside a proc stays off the Rust stack, so
/// the bounded worker stack is enough.
#[test]
fn a_worker_thread_carries_deep_guest_recursion() {
    let source = "def down(n: Int): Int\n\
                  \x20 if n <= 0\n\
                  \x20   0\n\
                  \x20 else\n\
                  \x20   down(n - 1)\n\
                  \x20 end\n\
                  end\n\
                  class Deep < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   down(50000)\n\
                  \x20 end\n\
                  end\n\
                  Deep.spawn().done()\n";
    let bytes = compile_to_bytes("worker.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let config = VmConfig {
        max_frames: 100_000,
        ..VmConfig::default()
    };
    let outcome = lm_proc::run_on_worker(
        &loaded,
        config,
        &["Proc"],
        Box::new(|| Box::new(RecordingHost::new(1))),
    )
    .expect("the worker runs");
    assert_eq!(outcome.text, "Done(Done(0))");
}

/// A bad grant reports through the worker instead of panicking.
#[test]
fn a_worker_thread_reports_a_bad_grant() {
    let bytes = compile_to_bytes("worker.lm", "1\n").expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let error = lm_proc::run_on_worker(
        &loaded,
        VmConfig::default(),
        &["Nope"],
        Box::new(|| Box::new(RecordingHost::new(1))),
    )
    .expect_err("the grant must reject");
    assert!(error.contains("Nope"), "{error}");
}
