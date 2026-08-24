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

fn compare_modes(
    source: &str,
    grants: &[&str],
) -> Result<(String, String, SchedulerStats), String> {
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
    let mut scheduler = Scheduler::default();
    let actual = scheduler
        .run_parallel(&mut parallel, 3)
        .map_err(|error| error.to_string())?;
    Ok((
        deterministic.show_outcome(&expected),
        parallel.show_outcome(&actual),
        scheduler.stats(),
    ))
}

#[test]
fn par_map_matches_map_in_both_scheduler_modes() {
    let source = r#"
def work(value: Int): Int
  total = value
  i = 0
  while i < 1000
    total = total.wrapping_add(i)
    i = i + 1
  end
  total
end

def compare(): (Bool, Bool, Bool) with Proc
  values = Range(0, 64).to_list()
  sequential = values.map(work)
  from_list = values.par_map(work)
  from_range = Range(0, 64).par_map(work)
  empty = List[Int]().par_map(work)
  (from_list == sequential, from_range == sequential, empty.is_empty())
end

compare()
"#;
    let (deterministic, parallel, stats) =
        compare_modes(source, &["Proc"]).expect("both scheduler modes run");
    assert_eq!(deterministic, "Done((true, true, true))");
    assert_eq!(parallel, deterministic);
    assert!(stats.max_active_leases > 1);

    let faulting = r#"
def fail(): List[Int] with Proc
  Range(0, 32).par_map(do |value: Int|: Int
    if value == 17
      panic("parallel failure")
    end
    value
  end)
end
fail()
"#;
    let (deterministic, parallel, _) =
        compare_modes(faulting, &["Proc"]).expect("both fault paths run");
    assert_eq!(deterministic, "Fault(UserPanic)");
    assert_eq!(parallel, deterministic);
}

#[test]
fn a_running_closure_proc_produces_admissible_snapshot_bytes() {
    let source = r#"
class Gate < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
  end
end

def capture(): String with Proc, Vm
  offset = 7
  gate = Gate.spawn()
  worker = sys.proc.run(do ||: Int with Proc
    gate.send(1)
    value = 0
    while value < 2000000
      value = value + 1
    end
    value + offset
  end)
  gate.done().value()
  admitted = case worker.snapshot_wait(0)
  in Ok(snapshot)
    case snapshot.to_bytes()
    in Ok(bytes)
      case sys.vm.load_snapshot(bytes)
      in Ok(_) then "admitted"
      in Err(error) then "load: #{error}"
      end
    in Err(error) then "encode: #{error}"
    end
  in Err(error) then "capture: #{error}"
  end
  if worker.done().value() == 2000007
    admitted
  else
    "bad result"
  end
end
capture()
"#;
    let (outcome, stats) =
        run_parallel_with(source, 2, &["Proc", "Vm"]).expect("the closure snapshot runs");
    assert_eq!(outcome, "Done(\"admitted\")");
    assert!(stats.scoped_safepoint_waits > 0);
}

#[test]
fn restore_does_not_stop_unrelated_active_workers() {
    let source = r#"
class Gate < Proc[Int]
  def on_spawn(self): Int with Proc
    first = case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
    second = case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
    first + second
  end
end

class Spinner < Proc
  gate: Handle[Int, Int]

  def init(mut self, gate: Handle[Int, Int])
    self.gate = gate
  end

  def on_spawn(self): Int with Proc
    self.gate.send(1)
    value = 0
    while value < 2000000
      value = value + 1
    end
    value
  end
end

def answer(): Int
  7
end

def exercise(): Bool with Proc, Vm
  original = sys.vm.Vm().activate_or_fault(answer, args: ())
  snapshot = case original.snapshot()
  in Ok(value) then value
  in Err(_) then return false
  end

  gate = Gate.spawn()
  left = Spinner.spawn(gate)
  right = Spinner.spawn(gate)
  case gate.done()
  in Ok(2) then ()
  in Ok(_) then return false
  in Err(_) then return false
  end

  restored = case sys.vm.Vm().restore(snapshot)
  in Ok(value) then value
  in Err(_) then return false
  end
  restored_value = case restored.run()
  in Ok(value) then value
  in Err(_) then return false
  end
  left_done = left.done().is_ok()
  right_done = right.done().is_ok()
  restored_value == 7 and left_done and right_done
end

exercise()
"#;
    let (outcome, stats) =
        run_parallel_with(source, 3, &["Proc", "Vm"]).expect("the active restore runs");
    assert_eq!(outcome, "Done(true)");
    assert!(stats.max_active_leases >= 2);
    assert_eq!(stats.global_quiescence, 0);
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
        "Done((Ok(3), Ok(30)))"
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
  in Ok(value) then value
  in Err(_) then -1
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
    assert_eq!(world.show_outcome(&expected), "Done((Ok(14), Ok(300000)))");

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
    assert_eq!(world.show_outcome(&outcome), "Done((Ok(14), Ok(300000)))");
    assert!(scheduler.stats().max_active_leases >= 2);
    assert!(scheduler.stats().global_quiescence > 0);
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
in Ok(child)
  child.send(3)
  child.close()
  case child.done()
  in Ok(value) then value
  in Err(_) then -1
  end
in Err(_)
  -2
end
"#;
    let (expected, actual, _) =
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
    assert_eq!(outcome, "Done((Ok(200000), Ok(200000)))");
    assert_eq!(stats.max_active_leases, 2);
    assert!(stats.local_continuations > 0);
}

#[test]
fn worker_pool_rotates_compute_tasks_locally() {
    let source = r#"
class Spinner < Proc
  def on_spawn(self): Int
    i = 0
    while i < 200000
      i = i + 1
    end
    i
  end
end

a = Spinner.spawn()
b = Spinner.spawn()
c = Spinner.spawn()
d = Spinner.spawn()
(a.done(), b.done(), c.done(), d.done())
"#;
    let (outcome, stats) = run_parallel(source, 2).expect("the local queue world runs");
    assert_eq!(
        outcome,
        "Done((Ok(200000), Ok(200000), Ok(200000), Ok(200000)))"
    );
    assert!(stats.local_rotations > 0);
}

#[test]
fn a_deferred_send_recalls_its_busy_target() {
    let source = r#"
class BusyTarget < Proc[Int]
  def on_spawn(self): Int with Proc
    i = 0
    while i < 2000000
      i = i + 1
    end
    case self.receive()
    in Msg(value) then value + i
    in Closed then -1
    end
  end
end

class Sender < Proc
  target: Handle[Int, Int]

  def init(mut self, target: Handle[Int, Int])
    self.target = target
  end

  def on_spawn(self): Bool with Proc
    i = 0
    while i < 10000
      i = i + 1
    end
    self.target.send(7).is_sent()
  end
end

target = BusyTarget.spawn()
sender = Sender.spawn(target)
(sender.done(), target.done())
"#;
    let (outcome, stats) = run_parallel(source, 2).expect("the recalled target completes");
    assert_eq!(outcome, "Done((Ok(true), Ok(2000007)))");
    assert!(stats.worker_recalls > 0);
}

#[test]
fn allocating_workers_stay_inside_the_worker_pool() {
    let source = r#"
class Builder < Proc
  def on_spawn(self): Int
    values: [Int] = []
    i = 0
    while i < 6000
      values.push(i)
      i = i + 1
    end
    text = "x".pad_start(100000)
    values.len() + text.len()
  end
end

left = Builder.spawn()
right = Builder.spawn()
(left.done(), right.done())
"#;
    let (outcome, stats) = run_parallel(source, 2).expect("the allocation world runs");
    assert_eq!(outcome, "Done((Ok(106000), Ok(106000)))");
    assert_eq!(stats.max_active_leases, 2);
    assert!(stats.worker_heap_growth_bytes > 0);
    assert!(stats.local_continuations > 0);
    assert_eq!(stats.global_quiescence, 0);
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
fn a_pause_stops_an_active_worker_at_one_turn_boundary() {
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
    assert_eq!(outcome, "Done(Ok(true))");
    assert_eq!(stats.max_active_leases, 2);
    assert!(stats.scoped_safepoint_waits > 0);
    assert_eq!(stats.global_quiescence, 0);
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
        "Done(Ok([1, 2, 10, 20]))",
        "Done(Ok([1, 10, 2, 20]))",
        "Done(Ok([1, 10, 20, 2]))",
        "Done(Ok([10, 20, 1, 2]))",
        "Done(Ok([10, 1, 20, 2]))",
        "Done(Ok([10, 1, 2, 20]))",
    ];
    assert!(accepted.contains(&outcome.as_str()), "{outcome}");
}

#[test]
fn many_senders_complete_against_one_mailbox() {
    let mut source = r#"
class ManySink < Proc[Int]
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

class ManySender < Proc
  sink: Handle[Int, Int]

  def init(mut self, sink: Handle[Int, Int])
    self.sink = sink
  end

  def on_spawn(self): Int with Proc
    i = 0
    while i < 20
      self.sink.send(1)
      i = i + 1
    end
    i
  end
end

sink = ManySink.spawn()
"#
    .to_string();
    for sender in 0..8 {
        source.push_str(&format!("sender{sender} = ManySender.spawn(sink)\n"));
    }
    for sender in 0..8 {
        source.push_str(&format!("sender{sender}.done()\n"));
    }
    source.push_str("sink.close()\nsink.done()\n");
    let (outcome, _) = run_parallel(&source, 4).expect("all concurrent senders finish");
    assert_eq!(outcome, "Done(Ok(160))");
}

#[test]
fn boundary_heavy_tasks_stay_on_the_coordinator_fast_path() {
    let source = r#"
class Sink < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed then return total
      end
    end
  end
end

sink = Sink.spawn()
i = 0
while i < 100
  sink.send(1)
  i = i + 1
end
sink.close()
sink.done()
"#;
    let (outcome, stats) =
        run_parallel_with(source, 4, &["Proc"]).expect("the message stream runs");
    assert_eq!(outcome, "Done(Ok(100))");
    assert_eq!(stats.max_active_leases, 0);
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
    assert!(matches!(shown.as_str(), "Done(Ok(0))" | "Done(Ok(7))"));
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
    assert_eq!(world.show_outcome(&outcome), "Done((Ok(300000), Ok(2)))");
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
in Ok(value) then value
in Err(fault) then fault.code()
end
"#;
    let (outcome, _) = run_parallel_with(source, 3, &["Proc", "Vm", "Wait", "Clock"])
        .expect("the parallel mixed wait runs");
    assert_eq!(outcome, "Done(\"mailbox\")");
}

#[test]
fn two_armed_drive_waits_execute_on_parallel_workers() {
    let source = r#"
def work(answer: Int): Int
  i = 0
  total = answer
  while i < 500000
    total = total.wrapping_add(i)
    i = i + 1
  end
  answer
end

class Supervisor < Proc
  def on_spawn(self): Int with Proc, Vm, Wait
    first = sys.vm.Vm().activate_or_fault(work, args: (1,))
    second = sys.vm.Vm().activate_or_fault(work, args: (2,))
    select
    in first.drive_wait() -> event
      case event
      in Done(value) then value
      in Asked(_) then -1
      in Fault(_) then -2
      end
    in second.drive_wait() -> event
      case event
      in Done(value) then value
      in Asked(_) then -3
      in Fault(_) then -4
      end
    end
  end
end

case Supervisor.spawn().done()
in Ok(value) then value > 0
in Err(_) then false
end
"#;
    let (outcome, stats) = run_parallel_with(source, 3, &["Proc", "Vm", "Wait"])
        .expect("the parallel drive wait runs");
    assert_eq!(outcome, "Done(true)");
    assert!(stats.max_active_leases >= 2, "{stats:?}");
    assert_eq!(stats.global_quiescence, 0, "{stats:?}");
}

#[test]
fn snapshot_control_uses_a_scoped_reachability_barrier() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root().join("examples/08-snapshots/machine-world.lm"),
    )
    .expect("the snapshot example reads");
    let (deterministic, parallel, _) =
        compare_modes(&source, &["Proc", "Vm"]).expect("both scheduler modes run");
    assert_eq!(parallel, deterministic);
}

#[test]
fn snapshot_wait_tracks_worker_reports_without_a_global_stop() {
    let source = r#"
def index_file(file: FileHandle): Int with Fs.Read, Fs.Close
  size = case file.read(1024)
  in Ok(bytes) then bytes.len()
  in Err(_) then return -1
  end
  file.close()
  size
end

class Busy < Proc
  def on_spawn(self): Int
    i = 0
    while i < 1000000
      i = i + 1
    end
    i
  end
end

busy = Busy.spawn()
case sys.fs.open("message.txt", ReadOnly)
in Ok(file)
  child = sys.vm.Vm().activate_or_fault(index_file, args: (file,))
  child.table().pass(Fs.Read)
  child.table().pass(Fs.Close)
  worker = sys.proc.run(child)
  captured = worker.snapshot_wait(10000).is_ok()
  result = worker.done().value_or(-1)
  (captured, result, busy.done().value_or(-1))
in Err(_) then (false, -1, busy.done().value_or(-1))
end
"#;
    let bytes = compile_to_bytes("parallel-snapshot-wait.lm", source).expect("the source compiles");
    let loaded = load_bytes(&bytes).expect("the artifact loads");
    let mut host = RecordingHost::new(1);
    host.set_file("message.txt", b"transient".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Proc", "Vm", "Fs"] {
        world.allow(grant).expect("the grant exists");
    }
    let mut scheduler = Scheduler::default();
    let outcome = scheduler
        .run_parallel(&mut world, 3)
        .expect("the snapshot wait world runs");
    assert_eq!(world.show_outcome(&outcome), "Done((true, 9, 1000000))");
    assert!(scheduler.stats().max_active_leases >= 2);
    assert_eq!(scheduler.stats().global_quiescence, 0);
}

#[test]
fn snapshot_keeps_two_active_reachable_procs_at_scoped_safepoints() {
    let source = r#"
class Gate < Proc[Int]
  def on_spawn(self): Int with Proc
    self.receive()
    self.receive()
    2
  end
end

class BusyChild < Proc
  gate: Handle[Int, Int]

  def init(mut self, gate: Handle[Int, Int])
    self.gate = gate
  end

  def on_spawn(self): Int with Proc
    self.gate.send(1)
    value = 0
    while value < 2000000
      value = value + 1
    end
    value
  end
end

class BusySnapshot < Proc
  gate: Handle[Int, Int]
  child: Handle[Never, Int]

  def init(mut self, gate: Handle[Int, Int], child: Handle[Never, Int])
    self.gate = gate
    self.child = child
  end

  def on_spawn(self): Int with Proc
    self.gate.send(1)
    value = 0
    while value < 2000000
      value = value + 1
    end
    value
  end
end

gate = Gate.spawn()
child = BusyChild.spawn(gate)
worker = BusySnapshot.spawn(gate, child)
case gate.done()
in Ok(2) then ()
in Ok(_) then panic("the gate returned a bad count")
in Err(_) then panic("the gate faulted")
end
captured = worker.snapshot_wait(0).is_ok()
worker_finished = case worker.done()
in Ok(value) then value == 2000000
in Err(_) then false
end
child_finished = case child.done()
in Ok(value) then value == 2000000
in Err(_) then false
end
captured and worker_finished and child_finished
"#;
    let (outcome, stats) =
        run_parallel_with(source, 3, &["Proc", "Vm"]).expect("the active capture runs");
    assert_eq!(outcome, "Done(true)");
    assert!(stats.max_active_leases >= 3);
    assert!(stats.scoped_safepoint_waits >= 2);
    assert_eq!(stats.global_quiescence, 0);
}

#[test]
fn additive_installation_does_not_stop_an_active_image_proc() {
    let source = r#"
class Signal < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
  end
end

def spare(value: Int): Int
  value + 1
end

class BusyInstall < Proc
  signal: Handle[Int, Int]

  def init(mut self, signal: Handle[Int, Int])
    self.signal = signal
  end

  def on_spawn(self): Int with Proc
    self.signal.send(1)
    value = 0
    while value < 500000
      value = value + 1
    end
    value
  end
end

def launch(signal: Handle[Int, Int]): Handle[Never, Int] with Proc
  BusyInstall.spawn(signal)
end

def exercise(): Result[Bool, String] with Vm, Proc
  signal = Signal.spawn()
  image = sys.vm.Vm()
  image.install(codeof(BusyInstall)).map_error() {
    |error: CodeError| error.message
  }?
  launcher = image.install(launch).map_error() {
    |error: CodeError| error.message
  }?
  run = image.activate(launcher, args: (signal,)).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  worker = case run.run()
  in Ok(handle) then handle
  in Err(_) then return Err("the launcher faulted")
  end
  case signal.done()
  in Ok(_) then ()
  in Err(_) then return Err("the start signal faulted")
  end
  image.install(spare).map_error() {
    |error: CodeError| error.message
  }?
  case worker.done()
  in Ok(value) then Ok(value == 500000)
  in Err(_) then Err("the worker faulted")
  end
end

exercise()
"#;
    let (outcome, stats) =
        run_parallel_with(source, 3, &["Proc", "Vm"]).expect("the active installation runs");
    assert_eq!(outcome, "Done(Ok(true))");
    assert!(stats.max_active_leases >= 2);
    assert_eq!(stats.scoped_safepoint_waits, 0);
    assert_eq!(stats.global_quiescence, 0);
}

#[test]
fn replacement_under_parallel_execution_matches_deterministic_execution() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root()
            .join("examples/15-compiler-and-hot-code-reloading/06-upgrade-a-running-proc.lm"),
    )
    .expect("the replacement example reads");
    let (deterministic, parallel, stats) =
        compare_modes(&source, &["Proc", "Vm"]).expect("both scheduler modes run");
    assert_eq!(parallel, deterministic);
    assert_eq!(parallel, "Done(Ok((20, 30, 2)))");
    assert_eq!(stats.global_quiescence, 0);
}

#[test]
fn replacement_stops_an_active_image_proc_at_one_scoped_safepoint() {
    let source = r#"
class Signal < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(value) then value
    in Closed then 0
    end
  end
end

def rate(value: Int): Int
  value + 1
end

def revised_rate(value: Int): Int
  value + 2
end

class BusyWorker < Proc
  signal: Handle[Int, Int]

  def init(mut self, signal: Handle[Int, Int])
    self.signal = signal
  end

  def on_spawn(self): Int with Proc
    self.signal.send(1)
    value = 0
    i = 0
    while i < 200000
      value = rate(value)
      i = i + 1
    end
    value
  end
end

def launch(signal: Handle[Int, Int]): Handle[Never, Int] with Proc
  BusyWorker.spawn(signal)
end

def exercise(): Result[Bool, String] with Vm, Proc
  signal = Signal.spawn()
  image = sys.vm.Vm()
  worker_class = image.install(codeof(BusyWorker)).map_error() {
    |error: CodeError| error.message
  }?
  instance = worker_class.instance().map_error() {
    |error: CodeError| error.message
  }?
  launcher = image.install(launch).map_error() {
    |error: CodeError| error.message
  }?
  original = instance.function_binding[(Int,), Int]("rate").map_error() {
    |error: CodeError| error.message
  }?
  revision = image.install(revised_rate).map_error() {
    |error: CodeError| error.message
  }?
  run = image.activate(launcher, args: (signal,)).map_error() {
    |error: CodeError| error.message
  }?
  run.table().pass(Proc)
  worker = case run.run()
  in Ok(handle) then handle
  in Err(_) then return Err("the launcher faulted")
  end
  case signal.done()
  in Ok(_) then ()
  in Err(_) then return Err("the start signal faulted")
  end
  image.replace(original, revision).map_error() {
    |error: CodeError| error.message
  }?
  total = case worker.done()
  in Ok(value) then value
  in Err(_) then return Err("the worker faulted")
  end
  Ok(total >= 200000 and total <= 400000)
end

exercise()
"#;
    let (outcome, stats) =
        run_parallel_with(source, 3, &["Proc", "Vm"]).expect("the active upgrade runs");
    assert_eq!(outcome, "Done(Ok(true))");
    assert!(stats.max_active_leases >= 2);
    assert!(stats.scoped_safepoint_waits > 0);
    assert_eq!(stats.global_quiescence, 0);
}
