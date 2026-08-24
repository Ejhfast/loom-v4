//! Parallel scheduler correctness tests.

use lm_proc::{Scheduler, SchedulerStats};
use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, TraceEvent, VmConfig, World, WorldLimits};

fn run_parallel_with(
    source: &str,
    workers: usize,
    grants: &[&str],
) -> Result<(String, SchedulerStats), String> {
    let bytes = compile_to_bytes("parallel.lm", source)?;
    let loaded = load_bytes(&bytes).map_err(|error| error.to_string())?;
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in grants {
        world.allow(grant)?;
    }
    let mut scheduler = Scheduler::default();
    let outcome = scheduler
        .run_parallel(&mut world, workers)
        .map_err(|error| error.to_string())?;
    Ok((world.show_outcome(&outcome), scheduler.stats()))
}

fn run_parallel(source: &str, workers: usize) -> Result<(String, SchedulerStats), String> {
    run_parallel_with(source, workers, &["Proc"])
}

fn compare_modes(source: &str, grants: &[&str]) -> Result<(String, String), String> {
    let bytes = compile_to_bytes("parallel-compare.lm", source)?;
    let loaded = load_bytes(&bytes).map_err(|error| error.to_string())?;
    let make_world = || {
        World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        )
    };
    let mut deterministic = make_world();
    let mut parallel = make_world();
    for grant in grants {
        deterministic.allow(grant)?;
        parallel.allow(grant)?;
    }
    let expected = lm_proc::run_world(&mut deterministic);
    let actual = Scheduler::default()
        .run_parallel(&mut parallel, 3)
        .map_err(|error| error.to_string())?;
    Ok((
        deterministic.show_outcome(&expected),
        parallel.show_outcome(&actual),
    ))
}

#[test]
fn one_runnable_task_stays_on_the_inline_path() {
    let source = "i = 0\nwhile i < 10000\n  i = i + 1\nend\ni\n";
    let (outcome, stats) = run_parallel(source, 4).expect("the inline world runs");
    assert_eq!(outcome, "Done(10000)");
    assert_eq!(stats.max_active_leases, 0);
}

#[test]
fn parallel_procs_preserve_mailbox_results() {
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
                  a = Adder.spawn()\n\
                  b = Adder.spawn()\n\
                  a.send(1)\n\
                  b.send(10)\n\
                  a.send(2)\n\
                  b.send(20)\n\
                  a.close()\n\
                  b.close()\n\
                  (a.done(), b.done())\n";
    assert_eq!(
        run_parallel(source, 3).expect("the parallel world runs").0,
        "Done((Done(3), Done(30)))"
    );
}

#[test]
fn a_worker_can_spawn_and_join_a_child_while_another_worker_runs() {
    let source = r#"
class Leaf < Proc
  def on_spawn(self): Int
    7
  end
end

def run_leaf(): Int with Proc
  child = Leaf.spawn()
  case child.done()
  in Done(value) then value
  in Fault(_) then -1
  end
end

class Parent < Proc
  def on_spawn(self): Int with Proc
    left = run_leaf()
    right = run_leaf()
    left + right
  end
end

class Spinner < Proc
  def on_spawn(self): Int
    i = 0
    while i < 300000
      i = i + 1
    end
    i
  end
end

parent = Parent.spawn()
spinner = Spinner.spawn()
(parent.done(), spinner.done())
"#;
    let bytes = compile_to_bytes("parallel-spawn.lm", source).expect("the source compiles");
    let loaded = load_bytes(&bytes).expect("the artifact loads");
    let limits = WorldLimits {
        max_machines: 4,
        ..WorldLimits::default()
    };
    let mut world = World::new_with_limits(
        &loaded,
        VmConfig::default(),
        limits,
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    let expected = lm_proc::run_world(&mut world);
    assert_eq!(
        world.show_outcome(&expected),
        "Done((Done(14), Done(300000)))"
    );

    let mut world = World::new_with_limits(
        &loaded,
        VmConfig::default(),
        limits,
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    let mut scheduler = Scheduler::default();
    let outcome = scheduler
        .run_parallel(&mut world, 3)
        .expect("the nested spawn world runs");
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((Done(14), Done(300000)))"
    );
    assert!(scheduler.stats().max_active_leases >= 2);
}

#[test]
fn a_child_keeps_effect_routing_after_its_parent_stops() {
    let source = r#"
class Inner < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
  end
end

class Maker < Proc
  def on_spawn(self): Handle[Int, Int] with Proc
    Inner.spawn()
  end
end

case Maker.spawn().done()
in Done(child)
  child.send(3)
  child.close()
  case child.done()
  in Done(value) then value
  in Fault(_) then -1
  end
in Fault(_)
  -2
end
"#;
    let (expected, actual) =
        compare_modes(source, &["Proc"]).expect("both parent lifetime programs run");
    assert_eq!(expected, "Done(3)");
    assert_eq!(actual, expected);
}

#[test]
fn independent_cpu_procs_hold_two_worker_leases() {
    let source = "class Spinner < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   i = 0\n\
                  \x20   while i < 200000\n\
                  \x20     i = i + 1\n\
                  \x20   end\n\
                  \x20   i\n\
                  \x20 end\n\
                  end\n\
                  a = Spinner.spawn()\n\
                  b = Spinner.spawn()\n\
                  (a.done(), b.done())\n";
    let (outcome, stats) = run_parallel(source, 2).expect("the parallel world runs");
    assert_eq!(outcome, "Done((Done(200000), Done(200000)))");
    assert_eq!(stats.max_active_leases, 2);
}

#[test]
fn allocation_refills_do_not_become_guest_heap_faults() {
    let source = r#"
class Builder < Proc
  def on_spawn(self): Int
    values: [Int] = []
    i = 0
    while i < 2000
      values.push(i)
      i = i + 1
    end
    values.len()
  end
end

left = Builder.spawn()
right = Builder.spawn()
(left.done(), right.done())
"#;
    let (outcome, stats) = run_parallel(source, 2).expect("the allocation world runs");
    assert_eq!(outcome, "Done((Done(2000), Done(2000)))");
    assert_eq!(stats.max_active_leases, 2);
}

#[test]
fn parallel_workers_share_one_world_fuel_reservation() {
    let source = r#"
class Spinner < Proc
  def on_spawn(self): Int
    i = 0
    while i < 100000
      i = i + 1
    end
    i
  end
end

left = Spinner.spawn()
right = Spinner.spawn()
(left.done(), right.done())
"#;
    let bytes = compile_to_bytes("parallel-fuel.lm", source).expect("the source compiles");
    let loaded = load_bytes(&bytes).expect("the artifact loads");
    let limits = WorldLimits {
        fuel: 10_000,
        ..WorldLimits::default()
    };
    let mut world = World::new_with_limits(
        &loaded,
        VmConfig::default(),
        limits,
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    let mut scheduler = Scheduler::default();
    let outcome = scheduler
        .run_parallel(&mut world, 2)
        .expect("world fuel exhaustion remains a guest fault");
    assert_eq!(world.show_outcome(&outcome), "Fault(OutOfFuel)");
    assert_eq!(world.world_fuel(), 0);
    assert_eq!(scheduler.stats().max_active_leases, 2);
}

#[test]
fn a_pause_stops_an_active_worker_at_one_quantum_boundary() {
    let source = "class Spinner < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   i = 0\n\
                  \x20   while i < 2000000\n\
                  \x20     i = i + 1\n\
                  \x20   end\n\
                  \x20   i\n\
                  \x20 end\n\
                  end\n\
                  class Pauser < Proc\n\
                  \x20 target: Handle[Never, Int]\n\
                  \x20 def init(mut self, target: Handle[Never, Int])\n\
                  \x20   self.target = target\n\
                  \x20 end\n\
                  \x20 def on_spawn(self): Bool with Proc\n\
                  \x20   case self.target.pause()\n\
                  in Ok(_) then true\n\
                  in Err(_) then false\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  spinner = Spinner.spawn()\n\
                  pauser = Pauser.spawn(spinner)\n\
                  pauser.done()\n";
    let (outcome, stats) = run_parallel(source, 2).expect("the active proc pauses");
    assert_eq!(outcome, "Done(Done(true))");
    assert_eq!(stats.max_active_leases, 2);
}

#[test]
fn different_senders_keep_each_sender_order() {
    let source = r#"
class Collector < Proc[Int]
  values: [Int] = []

  def on_spawn(mut self): [Int] with Proc
    loop do
      case self.receive()
      in Msg(value)
        self.values.push(value)
      in Closed
        return self.values.freeze()
      end
    end
  end
end

class Sender < Proc
  target: Handle[Int, [Int]]
  first: Int
  second: Int

  def init(
    mut self,
    target: Handle[Int, [Int]],
    first: Int,
    second: Int
  )
    self.target = target
    self.first = first
    self.second = second
  end

  def on_spawn(self): Int with Proc
    self.target.send(self.first)
    self.target.send(self.second)
    0
  end
end

target = Collector.spawn()
first = Sender.spawn(target, 1, 2)
second = Sender.spawn(target, 10, 20)
first.done()
second.done()
target.close()
target.done()
"#;
    let (outcome, _) = run_parallel(source, 4).expect("the concurrent senders finish");
    let accepted = [
        "Done(Done([1, 2, 10, 20]))",
        "Done(Done([1, 10, 2, 20]))",
        "Done(Done([1, 10, 20, 2]))",
        "Done(Done([10, 20, 1, 2]))",
        "Done(Done([10, 1, 20, 2]))",
        "Done(Done([10, 1, 2, 20]))",
    ];
    assert!(accepted.contains(&outcome.as_str()), "{outcome}");
}

#[test]
fn a_send_and_close_race_uses_coordinator_commit_order() {
    let source = r#"
class Target < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed
        return total
      end
    end
  end
end

class Sender < Proc
  target: Handle[Int, Int]
  def init(mut self, target: Handle[Int, Int]) self.target = target end
  def on_spawn(self): Bool with Proc self.target.send(7).is_sent() end
end

class Closer < Proc
  target: Handle[Int, Int]
  def init(mut self, target: Handle[Int, Int]) self.target = target end
  def on_spawn(self): Bool with Proc self.target.close().is_sent() end
end

target = Target.spawn()
sender = Sender.spawn(target)
closer = Closer.spawn(target)
sender.done()
closer.done()
target.done()
"#;
    let bytes = compile_to_bytes("parallel-race.lm", source).expect("the race source compiles");
    let loaded = load_bytes(&bytes).expect("the race artifact loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    world.trace_procs();
    let outcome = Scheduler::default()
        .run_parallel(&mut world, 4)
        .expect("the race world runs");
    let shown = world.show_outcome(&outcome);
    assert!(matches!(shown.as_str(), "Done(Done(0))" | "Done(Done(7))"));
    let send = world
        .trace()
        .iter()
        .position(|event| matches!(event, TraceEvent::Send { from: 2, to: 1, .. }));
    let close = world
        .trace()
        .iter()
        .position(|event| matches!(event, TraceEvent::Close { proc: 1, .. }));
    let (send, close) = send.zip(close).expect("the trace contains both actions");
    let accepted = matches!(world.trace()[send], TraceEvent::Send { accepted: true, .. });
    assert_eq!(accepted, send < close);
}

#[test]
fn root_termination_returns_every_worker_to_a_snapshot_boundary() {
    let source = r#"
class Spinner < Proc
  def on_spawn(self): Never
    loop do
      ()
    end
  end
end

class Quick < Proc
  def on_spawn(self): Int
    1
  end
end

spinner = Spinner.spawn()
quick = Quick.spawn()
quick.done()
0
"#;
    let bytes = compile_to_bytes("parallel-stop.lm", source).expect("the stop source compiles");
    let loaded = load_bytes(&bytes).expect("the stop artifact loads");
    let mut saw_two_leases = false;
    for _ in 0..16 {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        world.allow("Proc").expect("the Proc grant exists");
        let mut scheduler = Scheduler::default();
        let outcome = scheduler
            .run_parallel(&mut world, 2)
            .expect("the root world runs");
        assert_eq!(world.show_outcome(&outcome), "Done(0)");
        saw_two_leases |= scheduler.stats().max_active_leases == 2;
        assert!(world.all_machines_resident());
        world
            .snapshot_wait(world.root(), 0)
            .expect("the stopped world is at a snapshot boundary");
    }
    assert!(saw_two_leases);
}

#[test]
fn host_completions_and_worker_reports_share_one_wake_path() {
    let source = r#"
class Spinner < Proc
  def on_spawn(self): Int
    i = 0
    while i < 300000
      i = i + 1
    end
    i
  end
end

class Timer < Proc
  def on_spawn(self): Int with Clock.Sleep
    sys.clock.sleep(1)
    2
  end
end

spinner = Spinner.spawn()
timer = Timer.spawn()
(spinner.done(), timer.done())
"#;
    let bytes = compile_to_bytes("parallel-wake.lm", source).expect("the source compiles");
    let loaded = load_bytes(&bytes).expect("the artifact loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(lm_host::CliHost::new(1)),
    );
    world.allow("Proc").expect("the Proc grant exists");
    world.allow("Clock").expect("the Clock grant exists");
    let outcome = Scheduler::default()
        .run_parallel(&mut world, 2)
        .expect("the mixed wake world runs");
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((Done(300000), Done(2)))"
    );
}

#[test]
fn mixed_waits_keep_one_selected_result() {
    let source = r#"
enum Command
  Stop
end

def spin(): Never
  loop do
    ()
  end
end

class Supervisor < Proc[Command]
  def on_spawn(self): String with Proc, Vm, Wait, Clock
    child = sys.vm.Vm().activate_or_fault(spin, args: ())
    select
    in child.drive_wait() -> _
      "drive"
    in sys.clock.sleep.wait(0) -> _
      "clock"
    in self.receive_wait() -> _
      "mailbox"
    end
  end
end

supervisor = Supervisor.spawn()
supervisor.send(Stop)
case supervisor.done()
in Done(value) then value
in Fault(fault) then fault.code()
end
"#;
    let (outcome, _) = run_parallel_with(source, 3, &["Proc", "Vm", "Wait", "Clock"])
        .expect("the parallel mixed wait runs");
    assert_eq!(outcome, "Done(\"mailbox\")");
}

#[test]
fn snapshot_control_uses_the_global_quiescence_fallback() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root().join("examples/08-snapshots/machine-world.lm"),
    )
    .expect("the snapshot example reads");
    let (deterministic, parallel) =
        compare_modes(&source, &["Proc", "Vm"]).expect("both scheduler modes run");
    assert_eq!(parallel, deterministic);
}

#[test]
fn replacement_under_parallel_execution_matches_deterministic_execution() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root()
            .join("examples/15-compiler-and-hot-code-reloading/06-upgrade-a-running-proc.lm"),
    )
    .expect("the replacement example reads");
    let (deterministic, parallel) =
        compare_modes(&source, &["Proc", "Vm"]).expect("both scheduler modes run");
    assert_eq!(parallel, deterministic);
    assert_eq!(parallel, "Done(Ok((20, 30, 2)))");
}
