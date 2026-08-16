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

/// A message must be a frozen graph. A mutable one faults the sender
/// at its own boundary (specification 18.4).
#[test]
fn a_mutable_message_faults_the_sender() {
    let source = "class Sink < Proc[[Int]]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(xs)\n\
                  \x20     xs.len()\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Sink.spawn()\n\
                  h.send([1, 2])\n\
                  h.done()\n";
    assert_eq!(run(source), "Fault(UnsendableValue)");
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
