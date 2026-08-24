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

/// The public scheduler configuration enables parallel execution.
#[test]
fn a_worker_thread_accepts_parallel_scheduler_configuration() {
    let source = "class Value < Proc\n\
                  \x20 def on_spawn(self): Int\n\
                  \x20   21\n\
                  \x20 end\n\
                  end\n\
                  a = Value.spawn()\n\
                  b = Value.spawn()\n\
                  (a.done(), b.done())\n";
    let bytes = compile_to_bytes("parallel-worker.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let outcome = lm_proc::run_on_worker_with_scheduler(
        &loaded,
        VmConfig::default(),
        lm_proc::SchedulerConfig::parallel(2),
        &["Proc"],
        Box::new(|| Box::new(RecordingHost::new(1))),
    )
    .expect("the parallel scheduler runs");
    assert_eq!(outcome.text, "Done((Done(21), Done(21)))");
}

/// One host-owned pool can serve two worlds at the same time.
#[test]
fn a_shared_scheduler_pool_serves_two_worlds() {
    let source = "class Counter < Proc\n\
                  \x20 def on_spawn(self): Int\n\
                  \x20   i = 0\n\
                  \x20   while i < 10000\n\
                  \x20     i = i + 1\n\
                  \x20   end\n\
                  \x20   i\n\
                  \x20 end\n\
                  end\n\
                  a = Counter.spawn()\n\
                  b = Counter.spawn()\n\
                  (a.done(), b.done())\n";
    let bytes = compile_to_bytes("shared-pool.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let pool = lm_proc::SchedulerPool::new(2).expect("the shared pool starts");
    std::thread::scope(|scope| {
        let first_pool = pool.clone();
        let second_pool = pool.clone();
        let first = scope.spawn(|| run_shared_world(&loaded, first_pool, 1));
        let second = scope.spawn(|| run_shared_world(&loaded, second_pool, 2));
        let first = first.join().expect("the first host thread runs");
        let second = second.join().expect("the second host thread runs");
        assert_eq!(first.0, "Done((Done(10000), Done(10000)))");
        assert_eq!(second.0, "Done((Done(10000), Done(10000)))");
        assert!(first.1 > 0);
        assert!(second.1 > 0);
    });
}

fn run_shared_world(
    loaded: &lm_vm::LoadedModule,
    pool: lm_proc::SchedulerPool,
    seed: u64,
) -> (String, u32) {
    let mut world = World::new(
        loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(seed)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    let mut scheduler =
        lm_proc::Scheduler::from_config(lm_proc::SchedulerConfig::parallel_with_pool(pool));
    let outcome = scheduler.run(&mut world).expect("the shared world runs");
    (
        world.show_outcome(&outcome),
        scheduler.stats().max_active_leases,
    )
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

/// Invalid parallel worker counts reject before guest execution.
#[test]
fn a_worker_thread_rejects_an_invalid_parallel_worker_count() {
    let bytes = compile_to_bytes("worker.lm", "1\n").expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let error = lm_proc::run_on_worker_with_scheduler(
        &loaded,
        VmConfig::default(),
        lm_proc::SchedulerConfig::parallel(0),
        &[],
        Box::new(|| Box::new(RecordingHost::new(1))),
    )
    .expect_err("zero workers must reject");
    assert!(error.contains("between 1 and 256"), "{error}");
}
