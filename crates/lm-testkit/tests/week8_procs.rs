//! Week-8 proc suite: mailboxes, handles, ownership, the scheduler,
//! and the deterministic model rules.
//!
//! Every case runs the deterministic scheduler, so the interleaving
//! and the result repeat exactly.

use lm_proc::{Scheduler, SchedulerMode, StopReason};
use lm_testkit::{compile_to_bytes, repo_root, run_allowed};
use lm_vm::{load_bytes, MachineState, Ownership, RecordingHost, TraceEvent, VmConfig, World};

/// Compile and run one program with the deterministic scheduler.
fn run(source: &str) -> String {
    run_allowed("proc.lm", source, &["Proc"]).expect("the program compiles")
}

/// The proc class used by most cases: it sums every accepted message.
const ADDER: &str = "class Adder < Proc[Int]\n\
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
                     \x20   self.total\n\
                     \x20 end\n\
                     end\n";

// ---------------------------------------------------------------
// Mailbox model rules.
// ---------------------------------------------------------------

/// Accepted messages reach `receive` in host acceptance order.
#[test]
fn a_mailbox_delivers_in_fifo_order() {
    let source = "class Echo < Proc[Int]\n\
                  \x20 out: [Int] = []\n\
                  \x20 def on_spawn(mut self): [Int] with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.out.push(n)\n\
                  \x20     in Closed\n\
                  \x20       return self.out.freeze()\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20   self.out.freeze()\n\
                  \x20 end\n\
                  end\n\
                  h = Echo.spawn()\n\
                  h.send(1)\n\
                  h.send(2)\n\
                  h.send(3)\n\
                  h.close()\n\
                  h.done()\n";
    assert_eq!(run(source), "Done(Done([1, 2, 3]))");
}

/// `close` prevents later acceptance and preserves the queue.
/// `Closed` arrives after the drain.
#[test]
fn close_stops_acceptance_and_drains_the_queue() {
    let source = format!(
        "{ADDER}h = Adder.spawn()\n\
         h.send(4)\n\
         h.send(5)\n\
         first = h.close()\n\
         second = h.close()\n\
         after = h.send(6)\n\
         (first, second, after, h.done())\n"
    );
    assert_eq!(run(&source), "Done((Sent, Closed, Closed, Done(9)))");
}

/// A send to a terminal proc is a dead-peer result, not a fault.
#[test]
fn a_send_to_a_dead_proc_returns_a_fault_result() {
    let source = "class Quick < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   7\n\
                  \x20 end\n\
                  end\n\
                  h = Quick.spawn()\n\
                  done = h.done()\n\
                  after = h.send(1)\n\
                  closed = h.close()\n\
                  (done, after.is_sent(), closed.is_sent())\n";
    assert_eq!(run(source), "Done((Done(7), false, false))");
}

/// A send copies the message (specification 18.4). The receiver owns
/// a fresh mutable copy, so a write after the send never reaches it.
#[test]
fn a_mutable_message_copies_and_the_two_graphs_diverge() {
    let source = "class Sink < Proc[[Int]]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(xs)\n\
                  \x20     xs.push(0)\n\
                  \x20     xs.len()\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Sink.spawn()\n\
                  xs = [1, 2]\n\
                  h.send(xs)\n\
                  xs.push(3)\n\
                  sent = h.done()\n\
                  (sent, xs.len())\n";
    // The receiver counted two elements and added one of its own.
    // The sender counted three. Neither write reached the other.
    assert_eq!(run(source), "Done((Done(3), 3))");
}

/// A proc that holds its own handle sends to itself. The message
/// copies inside one heap, so the mailbox never shares a mutable
/// graph with the sender.
#[test]
fn a_self_send_copies_the_message_inside_one_heap() {
    let source = "enum Job\n\
                  \x20 Own(target: Handle[Job, Int])\n\
                  \x20 Data(items: [Int])\n\
                  end\n\
                  class Echo < Proc[Job]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(Own(me))\n\
                  \x20     xs = [1, 2]\n\
                  \x20     me.send(Data(xs))\n\
                  \x20     xs.push(3)\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(Data(ys)) then ys.len()\n\
                  \x20     in Msg(Own(_))   then 0 - 1\n\
                  \x20     in Closed        then 0 - 2\n\
                  \x20     end\n\
                  \x20   in Msg(Data(_)) then 0 - 3\n\
                  \x20   in Closed       then 0 - 4\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Echo.spawn()\n\
                  h.send(Own(h))\n\
                  h.done()\n";
    // The received list holds the two elements it had at the send.
    assert_eq!(run(source), "Done(Done(2))");
}

/// A terminal proc result crosses as a copy. The holder receives a
/// mutable copy when the proc returned a mutable graph.
#[test]
fn a_mutable_terminal_proc_result_crosses_as_a_mutable_copy() {
    let source = "class Maker < Proc\n\
                  \x20 def on_spawn(self): [Int] with Proc\n\
                  \x20   [1, 2, 3]\n\
                  \x20 end\n\
                  end\n\
                  case Maker.spawn().done()\n\
                  in Done(xs)\n\
                  \x20 xs.push(4)\n\
                  \x20 xs.len()\n\
                  in Fault(_) then 0 - 1\n\
                  end\n";
    assert_eq!(run(source), "Done(4)");
}

/// The mailbox limit is checked before the copy. A send past the
/// bound blocks the sender until the proc frees one slot.
#[test]
fn a_full_mailbox_blocks_the_sender_before_the_copy() {
    let source = format!("{ADDER}h = Adder.spawn()\nh.send(1)\nh.send(2)\nh.close()\nh.done()\n");
    let bytes = compile_to_bytes("proc.lm", &source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let config = VmConfig {
        mailbox_limit: 1,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, config, Box::new(RecordingHost::new(1)));
    world.trace_procs();
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::new(SchedulerMode::Deterministic);
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Done(3))");
    // The proc mailbox never held more than its bound.
    let metrics = world.mailbox_metrics(1);
    assert_eq!(metrics.limit, 1);
    assert_eq!(metrics.accepted, 2);
    assert_eq!(metrics.delivered, 2);
    // The second send blocked before it copied anything.
    assert!(
        world
            .trace()
            .iter()
            .any(|e| matches!(e, TraceEvent::Block { vm: 0, .. })),
        "{:?}",
        world.trace()
    );
}

// ---------------------------------------------------------------
// Handles.
// ---------------------------------------------------------------

/// A handle crosses a mailbox and still names the same proc.
#[test]
fn a_handle_keeps_its_target_across_a_transfer() {
    let text = std::fs::read_to_string(repo_root().join("examples/07-procs/mailbox-handle.lm"))
        .expect("the example reads");
    assert_eq!(run(&text), "Done((Done(1), Done(12)))");
}

/// A proc with no mailbox has no callable `send`.
#[test]
fn a_never_mailbox_has_no_send() {
    let source = "class Quiet < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   1\n\
                  \x20 end\n\
                  end\n\
                  h = Quiet.spawn()\n\
                  h.send(1)\n";
    let error = run_allowed("proc.lm", source, &["Proc"]).expect_err("the send must reject");
    assert!(error.contains("E1026"), "{error}");
    assert!(error.contains("no mailbox"), "{error}");
}

/// A message of the wrong type rejects at the checker.
#[test]
fn a_wrong_message_type_rejects() {
    let source = format!("{ADDER}h = Adder.spawn()\nh.send(\"x\")\nh.done()\n");
    let error = run_allowed("proc.lm", &source, &["Proc"]).expect_err("the send must reject");
    assert!(error.contains("E1004"), "{error}");
}

/// The handle result type follows `on_spawn`, and it never widens.
#[test]
fn the_handle_types_follow_the_proc_class() {
    let source = "class Named < Proc[Int]\n\
                  \x20 def on_spawn(self): String with Proc\n\
                  \x20   \"done\"\n\
                  \x20 end\n\
                  end\n\
                  h: Handle[Int, String] = Named.spawn()\n\
                  h.close()\n\
                  h.done()\n";
    assert_eq!(run(source), "Done(Done(\"done\"))");
    let wrong = "class Named < Proc[Int]\n\
                 \x20 def on_spawn(self): String with Proc\n\
                 \x20   \"done\"\n\
                 \x20 end\n\
                 end\n\
                 h: Handle[Int, Int] = Named.spawn()\n\
                 h.done()\n";
    let error = run_allowed("proc.lm", wrong, &["Proc"]).expect_err("the binding must reject");
    assert!(error.contains("E1004"), "{error}");
}

/// `spawn` needs a proc class with a valid `on_spawn`.
#[test]
fn spawn_needs_a_proc_class_and_an_on_spawn() {
    let plain = "class Plain\nend\nPlain.spawn()\n";
    let error = run_allowed("proc.lm", plain, &["Proc"]).expect_err("the spawn must reject");
    assert!(error.contains("E1026"), "{error}");
    assert!(error.contains("subclass of `Proc`"), "{error}");
    let missing = "class Silent < Proc[Int]\nend\nSilent.spawn()\n";
    let error = run_allowed("proc.lm", missing, &["Proc"]).expect_err("the spawn must reject");
    assert!(error.contains("declares no `on_spawn`"), "{error}");
    let extra = "class Odd < Proc[Int]\n\
                 \x20 def on_spawn(self, n: Int): Int with Proc\n\
                 \x20   n\n\
                 \x20 end\n\
                 end\n\
                 Odd.spawn(1)\n";
    let error = run_allowed("proc.lm", extra, &["Proc"]).expect_err("the spawn must reject");
    assert!(error.contains("must take `self` only"), "{error}");
}

/// `receive` is valid only inside a proc class.
#[test]
fn receive_is_only_valid_inside_a_proc_class() {
    let source = "def go(): Int with Proc.Recv\n  sys.proc.recv()\n  1\nend\ngo()\n";
    let error = run_allowed("proc.lm", source, &["Proc"]).expect_err("the call must reject");
    assert!(error.contains("E1051"), "{error}");
    assert!(error.contains("`Proc` subclass"), "{error}");
}

// ---------------------------------------------------------------
// Ownership, pause, and resume.
// ---------------------------------------------------------------

/// `Proc.Run` moves execution ownership to the scheduler, and the
/// original `Vm` handle faults until `pause()` returns it.
#[test]
fn a_dormant_vm_handle_faults_until_pause() {
    let source = "vm = sys.vm.Vm().from_object(do ||: Int 41 + 1 end, args: ())\n\
                  h = sys.proc.run(vm)\n\
                  vm.run()\n";
    assert_eq!(
        run_allowed("proc.lm", source, &["Proc", "Vm"]).expect("the program compiles"),
        "Fault(InvalidVmState)"
    );
}

/// A paused proc hands the holder a live machine back, and `resume`
/// gives it to the scheduler again.
#[test]
fn pause_and_resume_move_execution_ownership() {
    let source = "vm = sys.vm.Vm().from_object(do ||: Int 1 + 1 end, args: ())\n\
                  h = sys.proc.run(vm)\n\
                  first = h.pause()\n\
                  second = h.pause()\n\
                  back = h.resume()\n\
                  again = h.resume()\n\
                  case first\n\
                  in Ok(_)  then (second.is_err(), back.is_ok(), again.is_err(), h.done())\n\
                  in Err(_) then (false, false, false, h.done())\n\
                  end\n";
    assert_eq!(
        run_allowed("proc.lm", source, &["Proc", "Vm"]).expect("the program compiles"),
        "Done((true, true, true, Done(2)))"
    );
}

/// A paused proc runs under the holder, not the scheduler.
#[test]
fn a_paused_proc_leaves_the_scheduler_run_set() {
    let bytes = compile_to_bytes(
        "proc.lm",
        "vm = sys.vm.Vm().from_object(do ||: Int 1 + 1 end, args: ())\n\
         h = sys.proc.run(vm)\n\
         h.pause()\n",
    )
    .expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    world.allow("Vm").expect("the grant names a group");
    let mut scheduler = Scheduler::default();
    scheduler.run(&mut world);
    assert_eq!(world.owner_of(1), Ownership::Holder);
    assert!(world.runnable_procs().is_empty());
}

/// A quantum boundary lets the holder pause a task that has run.
#[test]
fn a_quantum_boundary_accepts_a_pause() {
    let source = "vm = sys.vm.Vm().from_object(do ||: Int\n\
                  \x20 i = 0\n\
                  \x20 while i < 10000\n\
                  \x20   i = i + 1\n\
                  \x20 end\n\
                  \x20 i\n\
                  end, args: ())\n\
                  h = sys.proc.run(vm)\n\
                  i = 0\n\
                  while i < 20\n\
                  \x20 i = i + 1\n\
                  end\n\
                  case h.pause()\n\
                  in Ok(_)  then 1\n\
                  in Err(_) then 0 - 1\n\
                  end\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    world.allow("Vm").expect("the grant names a group");
    let mut scheduler = Scheduler::new_with_quantum(SchedulerMode::Deterministic, 4);
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    assert_eq!(world.owner_of(1), Ownership::Holder);
    assert_eq!(world.active_of(1), 0);
}

// ---------------------------------------------------------------
// Parent lifetime and revocation.
// ---------------------------------------------------------------

/// A child table passes through the live parent table. Parent death
/// removes the pass-through and a later request fails closed.
#[test]
fn parent_death_closes_the_pass_through() {
    let source = "class Late < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Late.spawn()\n\
                  h.close()\n\
                  1\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::default();
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    // The root is terminal now, so the proc pass-through is gone. Its
    // next receive faults with the default denial.
    assert_eq!(world.state_of(0), MachineState::Done);
    lm_proc::drain_procs(&mut world);
    assert_eq!(world.state_of(1), MachineState::Faulted);
}

/// Revocation works through the paused machine table. The birth grant
/// is an ordinary table entry, so blocking the group closes it.
#[test]
fn a_revoked_group_denies_the_next_receive() {
    let source = "class Waiter < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Waiter.spawn()\n\
                  case h.pause()\n\
                  in Ok(machine)\n\
                  \x20 machine.table().block(Proc)\n\
                  in Err(_)\n\
                  \x20 ()\n\
                  end\n\
                  h.resume()\n\
                  h.close()\n\
                  h.done()\n";
    assert_eq!(
        run_allowed("proc.lm", source, &["Proc", "Vm"]).expect("the program compiles"),
        "Done(Fault(PolicyDenied))"
    );
}

// ---------------------------------------------------------------
// The scheduler.
// ---------------------------------------------------------------

/// Two runs of one program produce the same trace.
#[test]
fn the_deterministic_scheduler_repeats_its_interleaving() {
    let source = format!(
        "{ADDER}a = Adder.spawn()\n\
         b = Adder.spawn()\n\
         a.send(1)\n\
         b.send(2)\n\
         a.close()\n\
         b.close()\n\
         (a.done(), b.done())\n"
    );
    let bytes = compile_to_bytes("proc.lm", &source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let trace_of = || {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        world.trace_procs();
        world.allow("Proc").expect("the grant names a group");
        let mut scheduler = Scheduler::default();
        let outcome = scheduler.run(&mut world);
        (
            world.show_outcome(&outcome),
            world.trace().to_vec(),
            scheduler.stats(),
        )
    };
    let first = trace_of();
    let second = trace_of();
    assert_eq!(first.0, "Done((Done(1), Done(2)))");
    assert_eq!(first.0, second.0);
    assert_eq!(first.1, second.1);
    assert_eq!(first.2, second.2);
}

/// A bounded quantum lets a short later proc finish before a long
/// earlier proc.
#[test]
fn bounded_slices_let_a_later_short_proc_finish_first() {
    let source = "class Spin < Proc\n\
                  \x20 limit: Int\n\
                  \x20 def init(mut self, limit: Int)\n\
                  \x20   self.limit = limit\n\
                  \x20 end\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   i = 0\n\
                  \x20   while i < self.limit\n\
                  \x20     i = i + 1\n\
                  \x20   end\n\
                  \x20   i\n\
                  \x20 end\n\
                  end\n\
                  slow = Spin.spawn(200)\n\
                  fast = Spin.spawn(1)\n\
                  (slow.done(), fast.done())\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.trace_procs();
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::new_with_quantum(SchedulerMode::Deterministic, 8);
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done((Done(200), Done(1)))");
    let terminal: Vec<u32> = world
        .trace()
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Terminal { proc, .. } => Some(*proc),
            _ => None,
        })
        .collect();
    assert_eq!(terminal, vec![2, 1]);
    assert!(scheduler.stats().proc_slices > 2);
}

/// A waiting host operation does not stop a task that is ready.
#[test]
fn a_host_wait_does_not_stop_a_ready_task() {
    let source = "nap = sys.vm.Vm().from_object(do ||: Int with Clock.Sleep\n\
                  \x20 sys.clock.sleep(5)\n\
                  \x20 1\n\
                  end, args: ())\n\
                  nap.table().pass(Clock)\n\
                  quick = sys.vm.Vm().from_object(do ||: Int 2 end, args: ())\n\
                  slow = sys.proc.run(nap)\n\
                  fast = sys.proc.run(quick)\n\
                  (slow.done(), fast.done())\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.trace_procs();
    for grant in ["Proc", "Vm", "Clock"] {
        world.allow(grant).expect("the grant names a group");
    }
    let outcome = Scheduler::default().run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done((Done(1), Done(2)))");
    let terminal: Vec<u32> = world
        .trace()
        .iter()
        .filter_map(|event| match event {
            TraceEvent::Terminal { proc, .. } => Some(*proc),
            _ => None,
        })
        .collect();
    assert_eq!(terminal, vec![2, 1]);
}

/// A scheduler run does not install a reply for a holder-controlled
/// machine.
#[test]
fn host_completion_waits_for_its_controlling_task() {
    let source = "inner = sys.vm.Vm().from_object(do ||: Int with Clock.Sleep\n\
                  \x20 sys.clock.sleep(5)\n\
                  \x20 9\n\
                  end, args: ())\n\
                  inner.table().pass(Clock)\n\
                  inner.step()\n\
                  inner.step()\n\
                  inner.step()\n\
                  nap = sys.vm.Vm().from_object(do ||: Int with Clock.Sleep\n\
                  \x20 sys.clock.sleep(5)\n\
                  \x20 7\n\
                  end, args: ())\n\
                  nap.table().pass(Clock)\n\
                  sys.proc.run(nap).done()\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Proc", "Vm", "Clock"] {
        world.allow(grant).expect("the grant names a group");
    }
    let outcome = Scheduler::default().run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Done(7))");
    assert_eq!(world.state_of(1), MachineState::Waiting);
    assert_eq!(world.resource_count(1), 1);
    assert_eq!(world.world_resource_count(), 1);
}

/// Statistics describe one requested run only.
#[test]
fn scheduler_statistics_reset_at_each_run() {
    let source = "i = 0\nwhile i < 20\n  i = i + 1\nend\ni\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let mut scheduler = Scheduler::new_with_quantum(SchedulerMode::Deterministic, 4);
    let first = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&first), "Done(20)");
    assert!(scheduler.stats().root_slices > 1);
    let second = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&second), "Done(20)");
    assert_eq!(scheduler.stats().root_slices, 0);
    assert_eq!(scheduler.stats().proc_slices, 0);
    assert_eq!(scheduler.stats().unblocked, 0);
}

/// A world with no runnable machine and a blocked root is a deadlock.
/// Every blocked machine faults, so no run hangs.
#[test]
fn a_deadlock_faults_every_blocked_machine() {
    // The root waits for a proc that never terminates, and the proc
    // waits for a message the root will never send.
    let source = "class Patient < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Patient.spawn()\n\
                  h.done()\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::default();
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(HostFault)");
    assert_eq!(scheduler.stop_reason(), Some(StopReason::Deadlock));
}

/// One VM never executes concurrently: the scheduler drives one
/// machine per slice, and a machine of a suspended stack stays out of
/// the run set.
#[test]
fn one_machine_runs_at_a_time() {
    let source = format!("{ADDER}h = Adder.spawn()\nh.send(1)\nh.close()\nh.done()\n");
    let bytes = compile_to_bytes("proc.lm", &source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    let scheduler = Scheduler::default();
    // The root runs first and blocks; the run set then holds one proc
    // at a time, and never a machine of a suspended stack.
    let mut peak = 0;
    loop {
        let event = world.drive_root();
        peak = peak.max(world.runnable_procs().len());
        match event {
            lm_vm::RootEvent::Done(_) | lm_vm::RootEvent::Fault(_) => break,
            lm_vm::RootEvent::Blocked => {}
            other => panic!("unexpected event {other:?}"),
        }
        if world.poll_blocked() == 0 {
            let procs = world.runnable_procs();
            peak = peak.max(procs.len());
            match procs.first() {
                Some(proc) => {
                    world.drive_proc(*proc);
                }
                None => panic!("the world deadlocked"),
            }
        }
    }
    assert_eq!(peak, 1, "one proc is runnable at a time here");
    assert_eq!(scheduler.stats().proc_slices, 0);
}

// ---------------------------------------------------------------
// The runnable outputs.
// ---------------------------------------------------------------

#[test]
fn week8_examples_have_checked_output() {
    let read =
        |path: &str| std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    assert_eq!(run(&read("examples/07-procs/worker.lm")), "Done(Done(42))");
    assert_eq!(
        run(&read("examples/07-procs/mailbox-handle.lm")),
        "Done((Done(1), Done(12)))"
    );
    assert_eq!(run(&read("examples/07-procs/barrier.lm")), "Done(Done(5))");
}

/// A bare `Proc` parent is sugar for `Proc[Never]`: the proc takes no
/// message, and the spawn arguments reach its `init`.
#[test]
fn a_bare_proc_parent_takes_no_message() {
    let source = "class Doubler < Proc\n\
                  \x20 n: Int\n\
                  \x20 def init(mut self, n: Int)\n\
                  \x20   self.n = n\n\
                  \x20 end\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   self.n * 2\n\
                  \x20 end\n\
                  end\n\
                  h: Handle[Never, Int] = Doubler.spawn(21)\n\
                  case h.done()\n\
                  in Done(v)  then v\n\
                  in Fault(_) then 0\n\
                  end\n";
    assert_eq!(run(source), "Done(42)");
}

/// The spawn arguments are checked against the proc class `init`.
#[test]
fn the_spawn_arguments_check_against_init() {
    let source = "class Doubler < Proc\n\
                  \x20 n: Int\n\
                  \x20 def init(mut self, n: Int)\n\
                  \x20   self.n = n\n\
                  \x20 end\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   self.n * 2\n\
                  \x20 end\n\
                  end\n\
                  Doubler.spawn(\"x\")\n";
    let error = run_allowed("proc.lm", source, &["Proc"]).expect_err("the spawn must reject");
    assert!(error.contains("E1004"), "{error}");
    let arity = "class Doubler < Proc\n\
                 \x20 n: Int\n\
                 \x20 def init(mut self, n: Int)\n\
                 \x20   self.n = n\n\
                 \x20 end\n\
                 \x20 def on_spawn(self): Int with Proc\n\
                 \x20   self.n * 2\n\
                 \x20 end\n\
                 end\n\
                 Doubler.spawn()\n";
    let error = run_allowed("proc.lm", arity, &["Proc"]).expect_err("the spawn must reject");
    assert!(error.contains("E1006"), "{error}");
}

/// The spawner charges `Proc.Spawn` only. The constructor and the
/// proc body run inside the child, so their rows resolve through the
/// child table and the birth grant.
#[test]
fn the_spawner_charges_the_spawn_operation_only() {
    let source = "class Worker < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  def launch(): Handle[Int, Int] with Proc.Spawn\n\
                  \x20 Worker.spawn()\n\
                  end\n\
                  h = launch()\n\
                  h.send(4)\n\
                  h.close()\n\
                  h.done()\n";
    assert_eq!(run(source), "Done(Done(4))");
}

/// A handle is sendable data, so one proc may reach another proc that
/// a mailbox message named. The message crosses one boundary only.
#[test]
fn a_proc_reaches_a_peer_it_learned_from_a_message() {
    let source = "class Echo < Proc[Int]\n\
                  \x20 seen: Int = 0\n\
                  \x20 def on_spawn(mut self): Int with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.seen = self.seen + n\n\
                  \x20     in Closed\n\
                  \x20       return self.seen\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20   self.seen\n\
                  \x20 end\n\
                  end\n\
                  class Sender < Proc[Handle[Int, Int]]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(target)\n\
                  \x20     target.send(3)\n\
                  \x20     target.close()\n\
                  \x20     1\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  e = Echo.spawn()\n\
                  s = Sender.spawn()\n\
                  s.send(e)\n\
                  s.close()\n\
                  (s.done(), e.done())\n";
    assert_eq!(run(source), "Done((Done(1), Done(3)))");
}

// ---------------------------------------------------------------
// The week-8 gates, stated directly.
// ---------------------------------------------------------------

/// Message and result types never erase. The manifest schema of every
/// proc operation names `M` and `R`, and no schema names `Any`.
#[test]
fn no_proc_operation_erases_its_types() {
    let mut seen = 0;
    for slot in 0..lm_abi::OP_COUNT {
        let def = lm_abi::op(slot);
        if def.group != "Proc" {
            continue;
        }
        seen += 1;
        assert!(!def.schema.contains("Any"), "{}", def.schema);
        assert!(!def.schema.is_empty(), "{}.{}", def.group, def.member);
    }
    assert_eq!(seen, 10, "the manifest declares ten proc operations");
}

/// A transfer keeps the exact proc reference: the copy carries the
/// same machine identifier and the same generation.
#[test]
fn a_transfer_keeps_the_proc_identifier_and_generation() {
    let source = "class Echo < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   0\n\
                  \x20 end\n\
                  end\n\
                  class Holder < Proc[Handle[Int, Int]]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(_)\n\
                  \x20     1\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  e = Echo.spawn()\n\
                  hold = Holder.spawn()\n\
                  hold.send(e)\n\
                  hold.close()\n\
                  hold.done()\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    // Drive the root until it blocks, so the message sits in the
    // holder mailbox and the root still names the echo proc.
    match world.drive_root() {
        lm_vm::RootEvent::Blocked => {}
        other => panic!("the root must block, got {other:?}"),
    }
    // The root names the echo proc and the holder proc; the holder
    // names the echo proc through its accepted message.
    assert_eq!(
        world.machine_references(0).expect("the walk finishes"),
        vec![1, 2]
    );
    assert_eq!(
        world.machine_references(2).expect("the walk finishes"),
        vec![1]
    );
    assert_eq!(world.generation_of(1), 0);
}

/// A proc fault is a value for its holder (specification 18.6).
#[test]
fn a_proc_fault_publishes_as_a_terminal_value() {
    let source = "class Bad < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   1 / 0\n\
                  \x20 end\n\
                  end\n\
                  case Bad.spawn().done()\n\
                  in Done(v)  then \"{v}\"\n\
                  in Fault(f) then f.code()\n\
                  end\n";
    assert_eq!(run(source), "Done(\"DivideByZero\")");
}

/// The birth grant carries the `Proc` group and nothing else.
/// Additional grants use the explicit machine path (18.3).
#[test]
fn the_birth_grant_carries_the_proc_group_only() {
    let spawned = "class Talker < Proc\n\
                   \x20 def on_spawn(self): Int with Proc, Io.Print\n\
                   \x20   sys.io.print(\"x\")\n\
                   \x20   1\n\
                   \x20 end\n\
                   end\n\
                   case Talker.spawn().done()\n\
                   in Done(v)  then \"{v}\"\n\
                   in Fault(f) then f.code()\n\
                   end\n";
    assert_eq!(
        run_allowed("proc.lm", spawned, &["Proc", "Io"]).expect("the program compiles"),
        "Done(\"PolicyDenied\")"
    );
    // The explicit path grants what the launch needs.
    let explicit = "vm = sys.vm.Vm().from_object(do ||: Int with Io.Print\n\
                    \x20 sys.io.print(\"x\")\n\
                    \x20 1\n\
                    end, args: ())\n\
                    vm.table().pass(Io.Print)\n\
                    h = sys.proc.run(vm)\n\
                    case h.done()\n\
                    in Done(v)  then \"{v}\"\n\
                    in Fault(f) then f.code()\n\
                    end\n";
    assert_eq!(
        run_allowed("proc.lm", explicit, &["Proc", "Io", "Vm"]).expect("the program compiles"),
        "Done(\"1\")"
    );
}

/// A proc reserves a child from the parent budget, like every other
/// machine. A refused reservation faults the spawner.
#[test]
fn a_spawn_reserves_a_child_from_the_parent_budget() {
    let source = "class Q < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   1\n\
                  \x20 end\n\
                  end\n\
                  Q.spawn()\n\
                  Q.spawn()\n\
                  1\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let config = VmConfig {
        max_children: 1,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, config, Box::new(RecordingHost::new(1)));
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::default();
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(InvalidVmState)");
    // The refused spawn created no machine.
    assert_eq!(world.machine_count(), 2);
    assert_eq!(world.child_count(0), 1);
}

/// A proc launched inside another proc is an ordinary child machine.
#[test]
fn a_proc_may_spawn_a_proc() {
    let source = "class Inner < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   5\n\
                  \x20 end\n\
                  end\n\
                  class Outer < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case Inner.spawn().done()\n\
                  \x20   in Done(v)  then v + 1\n\
                  \x20   in Fault(_) then 0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  case Outer.spawn().done()\n\
                  in Done(v)  then v\n\
                  in Fault(_) then 0 - 1\n\
                  end\n";
    assert_eq!(run(source), "Done(6)");
}

/// A holder-driven nested machine may block on a proc. The whole
/// activation stack stops, the scheduler runs the proc, and the
/// stored stack resumes where it stopped.
#[test]
fn a_nested_machine_may_block_on_a_proc() {
    let source = "class Q < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Q.spawn()\n\
                  h.send(9)\n\
                  h.close()\n\
                  vm = sys.vm.Vm().from_object(do ||: Int with Proc\n\
                  \x20 case h.done()\n\
                  \x20 in Done(v)  then v\n\
                  \x20 in Fault(_) then 0 - 2\n\
                  \x20 end\n\
                  end, args: ())\n\
                  vm.table().pass(Proc)\n\
                  case vm.run()\n\
                  in Done(v)  then v\n\
                  in Fault(_) then 0 - 1\n\
                  end\n";
    assert_eq!(
        run_allowed("proc.lm", source, &["Proc", "Vm"]).expect("the program compiles"),
        "Done(9)"
    );
}

/// A handle is a frozen sendable designator, so a closure may capture
/// one and carry it into another machine.
#[test]
fn a_closure_may_capture_a_handle() {
    let source = "class Q < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Q.spawn()\n\
                  f = do ||: SendResult with Proc.Send\n\
                  \x20 h.send(4)\n\
                  end\n\
                  first = f()\n\
                  h.close()\n\
                  (first, h.done())\n";
    assert_eq!(run(source), "Done((Sent, Done(4)))");
}

/// `on_spawn` may come from an ancestor of the proc class. The body
/// function then declares that ancestor as its receiver, and the
/// constructed instance satisfies it.
#[test]
fn a_proc_class_may_inherit_its_on_spawn() {
    let source = "class Base < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  class Derived < Base\n\
                  end\n\
                  d = Derived.spawn()\n\
                  d.send(6)\n\
                  d.close()\n\
                  d.done()\n";
    assert_eq!(run(source), "Done(Done(6))");
}

/// `spawn` works inside a generic callable. The construction function
/// and the proc body declare no generic parameter, so their
/// signatures are closed and any scope may close over them.
#[test]
fn spawn_works_inside_a_generic_function() {
    let source = "class W < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   1\n\
                  \x20 end\n\
                  end\n\
                  def launch[T](x: T): Handle[Never, Int] with Proc.Spawn\n\
                  \x20 W.spawn()\n\
                  end\n\
                  case launch(1).done()\n\
                  in Done(v)  then v\n\
                  in Fault(_) then 0\n\
                  end\n";
    assert_eq!(run(source), "Done(1)");
}

/// A handle may leave a proc as its terminal result, and it still
/// names the same machine. The proc it names outlives its spawner, so
/// specification 18.6 closes the pass-through: the mailbox still
/// accepts, and the next request of the orphan fails closed.
///
/// The fault message names the cause, because the code alone reads
/// like an ordinary denial.
#[test]
fn a_proc_that_outlives_its_spawner_loses_its_pass_through() {
    let source = "class Inner < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  class Maker < Proc\n\
                  \x20 def on_spawn(self): Handle[Int, Int] with Proc\n\
                  \x20   Inner.spawn()\n\
                  \x20 end\n\
                  end\n\
                  case Maker.spawn().done()\n\
                  in Done(h)\n\
                  \x20 first = h.send(3)\n\
                  \x20 second = h.close()\n\
                  \x20 case h.done()\n\
                  \x20 in Done(v)  then \"done {v}\"\n\
                  \x20 in Fault(f) then \"{f.code()} {first.is_sent()} {second.is_sent()}\"\n\
                  \x20 end\n\
                  in Fault(f)\n\
                  \x20 f.code()\n\
                  end\n";
    assert_eq!(run(source), "Done(\"PolicyDenied true true\")");
    // The guest reads the stable code only, so the message is read
    // from the orphan machine itself. Machine 1 is the maker, and
    // machine 2 is the proc that outlived it.
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::new(SchedulerMode::Deterministic);
    scheduler.run(&mut world);
    let fault = world.fault_of(2).expect("the orphan faulted");
    assert_eq!(
        fault.message,
        "the operation Proc.Recv lost its pass through: the parent machine is gone"
    );
}

/// Two procs that wait for each other deadlock. The scheduler faults
/// every blocked machine, so no run hangs.
#[test]
fn two_waiting_procs_deadlock_without_hanging() {
    let source = "class A < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  a = A.spawn()\n\
                  b = A.spawn()\n\
                  (a.done(), b.done())\n";
    assert_eq!(run(source), "Fault(HostFault)");
}

/// A proc runs under its own fuel budget, and exhaustion is a value
/// for its holder.
#[test]
fn a_proc_runs_under_its_own_fuel_budget() {
    let source = "class Spin < Proc\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   i = 0\n\
                  \x20   while i < 100000\n\
                  \x20     i = i + 1\n\
                  \x20   end\n\
                  \x20   i\n\
                  \x20 end\n\
                  end\n\
                  case Spin.spawn().done()\n\
                  in Done(v)  then \"{v}\"\n\
                  in Fault(f) then f.code()\n\
                  end\n";
    let config = VmConfig {
        fuel: 400,
        ..VmConfig::default()
    };
    let outcome = lm_testkit::run_world("proc.lm", source, &["Proc"], config)
        .expect("the program compiles")
        .0;
    assert_eq!(outcome, "Done(\"OutOfFuel\")");
}

/// The proc trace and the mailbox table both have a readable dump,
/// and both repeat exactly.
#[test]
fn the_proc_dumps_are_readable_and_deterministic() {
    let source = format!("{ADDER}h = Adder.spawn()\nh.send(1)\nh.send(2)\nh.close()\nh.done()\n");
    let bytes = compile_to_bytes("proc.lm", &source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let dumps = || {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        world.trace_procs();
        world.allow("Proc").expect("the grant names a group");
        let outcome = Scheduler::default().run(&mut world);
        (
            world.show_outcome(&outcome),
            world.dump_trace(),
            world.dump_mailboxes(),
        )
    };
    let (outcome, trace, mailboxes) = dumps();
    assert_eq!(outcome, "Done(Done(3))");
    assert!(
        trace.starts_with("spawn parent 0 proc 1 gen 0\n"),
        "{trace}"
    );
    assert!(
        trace.contains("send from 0 to 1 accepted true\n"),
        "{trace}"
    );
    assert!(trace.contains("close proc 1 first true\n"), "{trace}");
    assert!(trace.contains("block vm 0 on done target 1\n"), "{trace}");
    assert!(trace.contains("receive proc 1 closed true\n"), "{trace}");
    assert!(trace.contains("terminal proc 1 faulted false\n"), "{trace}");
    assert_eq!(
        mailboxes,
        "proc 1 gen 0 limit 64 queued 0 accepted 2 delivered 2 closed true frozen false\n"
    );
    assert_eq!(dumps(), (outcome, trace, mailboxes));
}

/// A copy that passes a limit is not a sendability failure. The
/// sender fault names the limit instead of the shape rule.
#[test]
fn a_message_past_the_boundary_limit_names_the_limit() {
    let source = "class Sink < Proc[[Int]]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(xs) then xs.len()\n\
                  \x20   in Closed  then 0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Sink.spawn()\n\
                  h.send([1, 2, 3, 4, 5, 6, 7, 8, 9, 10])\n\
                  h.done()\n";
    let bytes = compile_to_bytes("proc.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let base = VmConfig::default();
    let config = VmConfig {
        // The message costs more bytes than one copy may walk. Every
        // other graph of this program is smaller.
        graph: lm_vm::GraphLimits {
            max_bytes: 120,
            ..base.graph
        },
        ..base
    };
    let mut world = World::new(&loaded, config, Box::new(RecordingHost::new(1)));
    world.allow("Proc").expect("the grant names a group");
    let mut scheduler = Scheduler::new(SchedulerMode::Deterministic);
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(BoundaryLimit)");
    let fault = world.fault_of(0).expect("the sender faulted");
    assert_eq!(
        fault.message,
        "the message copy exceeded the boundary limit"
    );
}
