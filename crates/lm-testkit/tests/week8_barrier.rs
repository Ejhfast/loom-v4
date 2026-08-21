//! Week-8 barrier suite: the consistent cut of specification 17.3.
//!
//! Week 8 defines no snapshot byte format, so the barrier always
//! resumes the world. The tests read the closed set, the cut marker,
//! and the preflight result, which is what the week-9 encoder
//! consumes.

use lm_proc::{Barrier, BarrierError, Scheduler};
use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::{load_bytes, FaultCode, RecordingHost, RootEvent, VmConfig, World};

/// Build one world from source with the `Proc` group granted.
fn world_of(source: &str) -> (lm_vm::LoadedModule, ()) {
    let bytes = compile_to_bytes("barrier.lm", source).expect("the program compiles");
    (load_bytes(&bytes).expect("the program loads"), ())
}

fn ready_world(loaded: &lm_vm::LoadedModule, allow: &[&str]) -> World {
    let mut world = World::new(loaded, VmConfig::default(), Box::new(RecordingHost::new(1)));
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    world
}

/// Drive the root until it blocks on a proc, so the barrier finds a
/// world with live machines.
fn run_to_first_block(world: &mut World) {
    match world.drive_root() {
        RootEvent::Blocked => {}
        other => panic!("the root must block on a proc, got {other:?}"),
    }
}

/// The barrier set is closed: a helper that only one accepted mailbox
/// message names still joins the set.
#[test]
fn the_barrier_set_closes_over_a_mailbox_handle() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    // Three machines exist: the root, the worker, and the helper.
    assert_eq!(world.machine_count(), 3);
    // The root names the worker only. The helper handle sits in the
    // worker mailbox.
    assert_eq!(
        world.machine_references(0).expect("the walk finishes"),
        vec![1]
    );
    assert_eq!(
        world.machine_references(1).expect("the walk finishes"),
        vec![2]
    );
    let report = Barrier::new(1)
        .run(&mut world, 0)
        .expect("the barrier opens");
    assert_eq!(report.set, vec![0, 1, 2]);
    assert!(report.resumed);
    assert!(report.objects > 0);
    // The barrier resumed the world, so the program still finishes.
    let mut scheduler = Scheduler::default();
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Done(5))");
}

/// The barrier records one cut marker, and every mailbox of the set
/// is frozen while it holds the world.
#[test]
fn the_barrier_records_one_mailbox_cut() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    let first = Barrier::new(1)
        .run(&mut world, 0)
        .expect("the barrier opens");
    let second = Barrier::new(1)
        .run(&mut world, 0)
        .expect("the barrier opens");
    // One barrier takes one marker, and the marker is monotone.
    assert_eq!(second.cut, first.cut + 1);
    assert_eq!(second.set, first.set);
    // The barrier thawed every mailbox it froze.
    for vm in &second.set {
        assert!(!world.mailbox_metrics(*vm).frozen, "machine {vm}");
    }
}

/// A frozen mailbox accepts no message, so the accepted queue at the
/// cut is exactly what a snapshot would copy.
#[test]
fn a_frozen_mailbox_blocks_the_sender_instead_of_accepting() {
    let source = "class Sink < Proc[Int]\n\
                  \x20 def on_spawn(self): Int with Proc\n\
                  \x20   case self.receive()\n\
                  \x20   in Msg(n)\n\
                  \x20     n\n\
                  \x20   in Closed\n\
                  \x20     0\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Sink.spawn()\n\
                  h.send(1)\n\
                  h.done()\n";
    let (loaded, ()) = world_of(source);
    let mut world = ready_world(&loaded, &["Proc"]);
    // Step the root until the spawn created the proc, then freeze the
    // mailbox before the send reaches it.
    let mut spawned = false;
    for _ in 0..512 {
        world.step_root();
        if world.machine_count() == 2 {
            spawned = true;
            break;
        }
    }
    assert!(spawned, "the root spawns the proc");
    world.set_barrier(1, Some(9));
    world.freeze_mailbox(1, true);
    run_to_first_block(&mut world);
    let metrics = world.mailbox_metrics(1);
    assert!(metrics.frozen);
    assert_eq!(metrics.accepted, 0, "a frozen mailbox accepts nothing");
    assert_eq!(world.blocked_machines(), vec![0]);
    // The thaw releases the sender, and the program finishes.
    world.freeze_mailbox(1, false);
    world.set_barrier(1, None);
    let mut scheduler = Scheduler::default();
    let outcome = scheduler.run(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(Done(1))");
}

/// Barriers over overlapping worlds serialize. The second barrier
/// finds the first and reports the overlap instead of racing it.
#[test]
fn overlapping_barriers_serialize() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    // Another barrier already holds the worker.
    world.set_barrier(1, Some(7));
    let error = Barrier::new(1)
        .run(&mut world, 0)
        .expect_err("the barrier must find the overlap");
    assert_eq!(error, BarrierError::Overlaps(1));
    // The refused barrier released every machine it had taken.
    assert_eq!(world.barrier_of(0), None);
    assert_eq!(world.barrier_of(2), None);
    // The other barrier releases the worker; the retry now opens.
    world.set_barrier(1, None);
    let report = Barrier::new(1).run(&mut world, 0).expect("the retry opens");
    assert_eq!(report.set, vec![0, 1, 2]);
}

/// Barriers over disjoint sets proceed. A machine no handle names is
/// outside the set, and a barrier over it does not touch the world of
/// the root.
#[test]
fn a_disjoint_barrier_proceeds() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    // A barrier rooted at the helper reaches the helper only.
    let helper = Barrier::new(2)
        .run(&mut world, 2)
        .expect("the helper barrier opens");
    assert_eq!(helper.set, vec![2]);
    // The root world is untouched, so a barrier over it still opens.
    let root = Barrier::new(3)
        .run(&mut world, 0)
        .expect("the root barrier opens");
    assert_eq!(root.set, vec![0, 1, 2]);
}

/// A live host attachment blocks the barrier, and the failure resumes
/// every machine the barrier stopped.
#[test]
fn a_live_host_attachment_blocks_the_barrier_and_resumes() {
    let source = "def go() with Clock.Sleep\n  sys.clock.sleep(1)\nend\ngo()\n";
    let (loaded, ()) = world_of(source);
    let mut world = ready_world(&loaded, &["Clock"]);
    // Step until the root waits on the host.
    let mut waited = false;
    for _ in 0..64 {
        match world.step_root() {
            RootEvent::Waiting => {
                waited = true;
                break;
            }
            RootEvent::Ran => {}
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert!(waited, "the program reaches the sleep");
    assert_eq!(
        world.snapshot_preflight(0),
        Err(FaultCode::BoundaryViolation)
    );
    let error = Barrier::new(1)
        .run(&mut world, 0)
        .expect_err("a live attachment blocks the barrier");
    // The path starts at the root and names machines by ordinal, so
    // the error carries no scheduler identifier.
    assert_eq!(
        error,
        BarrierError::ResourceActive {
            path: vec![0],
            kind: "a pending Clock.Sleep".to_string()
        }
    );
    // Failure resumes every paused machine.
    assert_eq!(world.barrier_of(0), None);
    assert!(!world.mailbox_metrics(0).frozen);
    // The original world is unchanged: it still finishes.
    loop {
        match world.step_root() {
            RootEvent::Done(_) | RootEvent::Fault(_) => break,
            _ => {}
        }
    }
    assert_eq!(world.state_of(0), lm_vm::MachineState::Done);
}

/// A barrier holds a machine out of the scheduler run set, so no
/// machine of the set runs while the barrier is open.
#[test]
fn a_held_machine_leaves_the_run_set() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    assert_eq!(world.runnable_procs(), vec![1, 2]);
    world.set_barrier(1, Some(5));
    world.set_barrier(2, Some(5));
    assert!(world.runnable_procs().is_empty());
    world.set_barrier(1, None);
    world.set_barrier(2, None);
    assert_eq!(world.runnable_procs(), vec![1, 2]);
}

/// The closed-set gate stated directly: every handle in the stopped
/// state of a set member targets another member of the set.
#[test]
fn every_handle_in_the_set_targets_a_member_of_the_set() {
    let source = std::fs::read_to_string(repo_root().join("examples/07-procs/barrier.lm"))
        .expect("the example reads");
    let (loaded, ()) = world_of(&source);
    let mut world = ready_world(&loaded, &["Proc"]);
    run_to_first_block(&mut world);
    let report = Barrier::new(1)
        .run(&mut world, 0)
        .expect("the barrier opens");
    for vm in &report.set {
        for target in world.machine_references(*vm).expect("the walk finishes") {
            assert!(
                report.set.contains(&target),
                "machine {vm} names {target} outside the set {:?}",
                report.set
            );
        }
    }
}

/// Five native shapes name a machine. The barrier set closes over all
/// five, so a machine that only a policy-table handle names still
/// joins the set.
#[test]
fn the_set_closes_over_every_machine_reference_shape() {
    let source = "def go(): Int with Vm, Clock.Now\n\
                  \x20 table = sys.vm.Vm().activate_or_fault({ ||: Int 1 }, args: ()).table()\n\
                  \x20 table.pass(Clock.Now)\n\
                  \x20 case sys.vm.Vm().activate_or_fault({ ||: Int 2 }, args: ()).run()\n\
                  \x20 in Done(v)  then v\n\
                  \x20 in Fault(_) then 0\n\
                  \x20 end\n\
                  end\n\
                  go()\n";
    let (loaded, ()) = world_of(source);
    let mut world = ready_world(&loaded, &["Vm", "Clock.Now"]);
    // Step until both machines exist and the table handle is live.
    let mut ready = false;
    for _ in 0..2048 {
        world.step_root();
        if world.machine_count() == 3 {
            ready = true;
            break;
        }
    }
    assert!(ready, "the program creates two machines");
    let report = Barrier::new(1)
        .run(&mut world, 0)
        .expect("the barrier opens");
    // The table handle alone names the first machine, so the set
    // still reaches it.
    assert!(report.set.contains(&1), "{:?}", report.set);
    for vm in &report.set {
        for target in world.machine_references(*vm).expect("the walk finishes") {
            assert!(report.set.contains(&target), "{:?}", report.set);
        }
    }
}
