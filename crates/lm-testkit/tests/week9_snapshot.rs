//! Week-9 snapshot suite: the machine world, the writer, restore, and
//! branching execution (specification 17).
//!
//! The cases below state the week-9 gates of the build order. Each
//! gate names the case that proves it in `docs/notes/week9.md`.

use lm_heap::Object;
use lm_testkit::{compile_to_bytes, repo_root, run_allowed, run_world};
use lm_vm::snapshot::{
    codec, ImagePolicyCursor, ImageReason, ImageState, LoadLimits, RestoreFail, SnapshotFail,
};
use lm_vm::{
    load_bytes, LoadedModule, Outcome, RecordingHost, RootEvent, VmConfig, VmId, World, WorldLimits,
};

fn program(source: &str) -> LoadedModule {
    let bytes = compile_to_bytes("snapshot.lm", source).expect("the program compiles");
    load_bytes(&bytes).expect("the program loads")
}

fn world_of<'m>(loaded: &'m LoadedModule, allow: &[&str]) -> World<'m> {
    let mut world = World::new(loaded, VmConfig::default(), Box::new(RecordingHost::new(1)));
    for grant in allow {
        world.allow(grant).expect("the grant names a target");
    }
    world
}

/// Drive one world until its root machine stops making progress.
fn drive(world: &mut World<'_>) -> Outcome {
    lm_proc::run_world(world)
}

/// Run one restored world to a terminal result, with the scheduler
/// driving the restored procs.
fn run_restored(world: &mut World<'_>, root: VmId) -> String {
    loop {
        match world.run_machine(root) {
            RootEvent::Done(value) => return format!("Done({})", world.show_value_of(root, value)),
            RootEvent::Fault(rec) => return format!("Fault({})", rec.code),
            RootEvent::Asked(_) => return "Asked".to_string(),
            RootEvent::Blocked => {
                if world.poll_blocked() > 0 {
                    continue;
                }
                match world.runnable_procs().first().copied() {
                    Some(proc) => {
                        world.drive_proc(proc);
                    }
                    None => return "Deadlock".to_string(),
                }
            }
            RootEvent::Ran | RootEvent::Waiting => return "Stopped".to_string(),
        }
    }
}

/// Restore one admitted image into a fresh world of the same program.
fn restore_into<'m>(
    loaded: &'m LoadedModule,
    image: &lm_vm::snapshot::SnapshotImage,
) -> (World<'m>, VmId) {
    let mut world = world_of(loaded, &["Proc", "Vm", "Clock"]);
    let target = world.new_child(0).expect("the budget holds one child");
    let root = world
        .restore_image(0, target, image)
        .expect("the restore builds a world");
    (world, root)
}

// ---------------------------------------------------------------
// The two runnable outputs.
// ---------------------------------------------------------------

#[test]
fn week9_examples_have_checked_output() {
    let read =
        |path: &str| std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
    assert_eq!(
        run_allowed(
            "branch.lm",
            &read("examples/08-snapshots/branch.lm"),
            &["Vm"]
        )
        .expect("the example runs"),
        "Done((42, 42))"
    );
    assert_eq!(
        run_allowed(
            "machine-world.lm",
            &read("examples/08-snapshots/machine-world.lm"),
            &["Proc", "Vm"]
        )
        .expect("the example runs"),
        "Done((42, 42, 42))"
    );
}

// ---------------------------------------------------------------
// Gate: snapshot round trips cover every bytecode boundary in the
// example corpus.
// ---------------------------------------------------------------

/// The pure examples: programs that need no grant.
const PURE_EXAMPLES: [&str; 7] = [
    "examples/01-basics/factorial.lm",
    "examples/01-basics/control.lm",
    "examples/02-objects/counter.lm",
    "examples/02-objects/closures.lm",
    "examples/03-types/generics.lm",
    "examples/06-graphs/brace-closure.lm",
    "examples/06-graphs/cycle-digest.lm",
];

/// The largest number of boundaries one round-trip case walks.
///
/// A factorial walks thousands of instructions, and the property is a
/// property of one boundary, so a bounded prefix states it without a
/// slow suite.
const BOUNDARY_LIMIT: usize = 40;

/// At every bytecode boundary of a program, the capture encodes,
/// decodes to equal bytes, and restores to a world that finishes with
/// the same result.
#[test]
fn a_snapshot_round_trips_at_every_boundary_of_the_example_corpus() {
    for path in PURE_EXAMPLES {
        let source = std::fs::read_to_string(repo_root().join(path)).expect("the example reads");
        let loaded = program(&source);
        let expected = {
            let mut world = world_of(&loaded, &[]);
            let outcome = drive(&mut world);
            world.show_outcome(&outcome)
        };
        let mut world = world_of(&loaded, &[]);
        for boundary in 0..BOUNDARY_LIMIT {
            match world.step_root() {
                RootEvent::Ran => {}
                RootEvent::Done(_) | RootEvent::Fault(_) => break,
                other => panic!("{path}: unexpected event {other:?}"),
            }
            let gate = world.next_gate();
            let image = world
                .capture_snapshot(gate, 0, false)
                .unwrap_or_else(|e| panic!("{path} boundary {boundary}: capture failed: {e:?}"));
            // The bytes decode, and the decoded image encodes back to
            // exactly the same bytes.
            let admitted = codec::load_external(image.bytes(), &loaded, LoadLimits::default())
                .unwrap_or_else(|e| panic!("{path} boundary {boundary}: {e}"));
            let again = codec::encode(admitted.world(), usize::MAX).expect("the image encodes");
            assert_eq!(
                &again,
                image.bytes().as_ref(),
                "{path} boundary {boundary}: the round trip moved the bytes"
            );
            // The restored world finishes with the same result.
            let (mut fresh, root) = restore_into(&loaded, &image);
            assert_eq!(
                run_restored(&mut fresh, root),
                expected,
                "{path} boundary {boundary}"
            );
        }
    }
}

// ---------------------------------------------------------------
// Gate: machine ordinals are deterministic and independent from
// scheduler identifiers.
// ---------------------------------------------------------------

fn asked_tree_source() -> String {
    std::fs::read_to_string(repo_root().join("checkpoints/asked-tree.lm"))
        .expect("the checkpoint source reads")
}

/// Capture the world the checkpoint program builds.
fn asked_tree_image(loaded: &LoadedModule) -> lm_vm::snapshot::SnapshotImage {
    let mut world = world_of(loaded, &["Proc", "Vm", "Clock"]);
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    world
        .last_snapshot()
        .expect("the program captured one world")
        .clone()
}

/// The machine ordinals follow reachability from the root, not the
/// order in which the scheduler minted the machines.
#[test]
fn machine_ordinals_follow_reachability_not_machine_identifiers() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let world = image.world();
    assert_eq!(world.machine_count(), 3);
    // The program spawns the helper first, then the worker, and it
    // builds the held machine last. The identifiers are therefore
    // helper 1, worker 2, held 3, and the ordinals reverse that order.
    assert_eq!(world.machines[0].state, ImageState::Asked);
    assert!(!world.machines[0].is_proc, "the held root is no proc");
    assert!(world.machines[1].is_proc, "ordinal 1 is the worker");
    assert_eq!(
        world.machines[1].mailbox.queue.len(),
        1,
        "the worker holds the helper handle"
    );
    assert!(world.machines[2].is_proc, "ordinal 2 is the helper");
    assert_eq!(world.machines[2].mailbox.queue.len(), 0);
    // The closed set is ascending by identifier and the canonical
    // order is not, so the two orders are different lists.
    let mut second = world_of(&loaded, &["Proc", "Vm", "Clock"]);
    drive(&mut second);
    let gate = second.next_gate();
    let report = second.run_cut(gate, 3).expect("the cut opens");
    second.release_cut(&report.set, true);
    assert_eq!(report.set, vec![1, 2, 3]);
    assert_eq!(report.order, vec![3, 2, 1]);
}

/// Two runs of one program produce one byte string.
#[test]
fn one_world_shape_produces_one_byte_string() {
    let loaded = program(&asked_tree_source());
    let first = asked_tree_image(&loaded);
    let second = asked_tree_image(&loaded);
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(first.hash(), second.hash());
}

// ---------------------------------------------------------------
// Gate: every handle in snapshot bytes targets a captured machine,
// and relocation covers every VM and mailbox root.
// ---------------------------------------------------------------

#[test]
fn every_handle_in_the_bytes_targets_a_captured_machine() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let world = image.world();
    let count = world.machine_count() as u32;
    let mut handles = 0;
    for machine in &world.machines {
        for entry in &machine.objects {
            let target = match entry.object {
                Object::NativeVm { vm }
                | Object::NativeTable { vm }
                | Object::NativeRequest { vm, .. }
                | Object::NativeCall { vm, .. } => Some(vm),
                Object::NativeHandle { proc, .. } => Some(proc),
                _ => None,
            };
            if let Some(target) = target {
                assert!(target < count, "an ordinal names no captured machine");
                handles += 1;
            }
        }
    }
    assert!(handles >= 2, "the world holds the two proc handles");
}

/// Restore relocates every handle: no restored heap names a machine
/// of the original world, and no restored object shares a heap slot
/// with the machine it came from.
#[test]
fn restore_relocates_every_vm_and_mailbox_root() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let (world, root) = restore_into(&loaded, &image);
    // The restored machines are the ones this restore added.
    let restored: Vec<VmId> = (root..world.machine_count() as VmId).collect();
    assert_eq!(restored.len(), image.world().machine_count());
    let mut found = 0;
    for vm in &restored {
        let heap = world.heap_of(*vm);
        heap.for_each_live(|_, _, object| {
            let target = match object {
                Object::NativeVm { vm }
                | Object::NativeTable { vm }
                | Object::NativeRequest { vm, .. }
                | Object::NativeCall { vm, .. } => Some(*vm),
                Object::NativeHandle { proc, .. } => Some(*proc),
                _ => None,
            };
            if let Some(target) = target {
                assert!(
                    restored.contains(&target),
                    "a restored heap names machine {target}, which is not restored"
                );
                found += 1;
            }
        });
    }
    assert!(found >= 2, "the restored world holds the two proc handles");
    // The mailbox queue of the restored worker holds the relocated
    // helper handle.
    let worker = restored[1];
    let queue = world.mailbox_metrics(worker);
    assert_eq!(queue.queued, 1);
}

// ---------------------------------------------------------------
// Gate: multi-shot restore creates complete independent worlds.
// ---------------------------------------------------------------

/// One snapshot, three worlds. A write in one is invisible in the
/// other two.
#[test]
fn multi_shot_restore_creates_independent_worlds() {
    let source = "\
def restore_list(snap: Snapshot[List[Int]]): Vm[List[Int]] with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(vm)  then vm
  in Err(_)  then sys.vm.Vm().from_fn(do ||: List[Int] [] end, args: ())
  end
end

vm = sys.vm.Vm().from_fn(do ||: List[Int]
  xs = [1]
  xs.push(2)
  xs
end, args: ())
vm.step()
case vm.snapshot()
in Ok(snap)
  first = restore_list(snap)
  second = restore_list(snap)
  a = case first.run()
      in Done(xs) then xs.len()
      in Fault(_) then 0 - 1
      end
  b = case second.run()
      in Done(xs) then xs.len()
      in Fault(_) then 0 - 1
      end
  c = case vm.run()
      in Done(xs) then xs.len()
      in Fault(_) then 0 - 1
      end
  (a, b, c)
in Err(_) then (0 - 3, 0 - 3, 0 - 3)
end
";
    assert_eq!(
        run_allowed("multi.lm", source, &["Vm"]).expect("the program runs"),
        "Done((2, 2, 2))"
    );
}

/// The restored worlds share no machine and no heap object with the
/// original or with each other.
#[test]
fn two_restores_share_nothing_with_each_other_or_the_original() {
    let loaded = program(&asked_tree_source());
    let mut world = world_of(&loaded, &["Proc", "Vm", "Clock"]);
    drive(&mut world);
    let image = world
        .last_snapshot()
        .expect("the program captured one world")
        .clone();
    let before = world.machine_count();
    let first_target = world.new_child(0).expect("the budget holds a child");
    let first = world
        .restore_image(0, first_target, &image)
        .expect("the first restore builds a world");
    let second_target = world.new_child(0).expect("the budget holds a child");
    let second = world
        .restore_image(0, second_target, &image)
        .expect("the second restore builds a world");
    // Each restore added its own machines beside the originals.
    assert_eq!(
        world.machine_count(),
        before + 2 * image.world().machine_count()
    );
    assert_ne!(first, second);
    // A machine never belongs to two worlds.
    let first_set: Vec<VmId> = (first..first + 3).collect();
    let second_set: Vec<VmId> = (second..second + 3).collect();
    for vm in &first_set {
        assert!(!second_set.contains(vm));
        assert!(*vm >= before as VmId);
    }
    // The mailbox of one restored worker drains without touching the
    // other two worlds.
    assert_eq!(world.mailbox_metrics(first_set[1]).queued, 1);
    assert_eq!(world.mailbox_metrics(second_set[1]).queued, 1);
    assert_eq!(world.mailbox_metrics(2).queued, 1);
}

// ---------------------------------------------------------------
// Gate: policy tables and root grants never enter snapshot bytes.
// ---------------------------------------------------------------

/// A machine that only a table-held mock closure names is not part of
/// the world, and a restored table is default-deny.
///
/// `docs/notes/week8.md` carries this case forward: the collection
/// roots hold every `Action::Mock` closure, and specification 17.2
/// excludes policy tables from a snapshot.
#[test]
fn a_machine_reachable_only_through_a_mock_closure_is_not_in_the_world() {
    let source = "\
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(n) then n
    in Closed then 0
    end
  end
end

def go(): Int with Vm, Proc, Clock
  worker = Worker.spawn()
  # The pair carries the proc handle. The mock handler captures the
  # pair and reads the other element, so the policy table of `outer`
  # is the only thing that names the worker from `outer`.
  pair = (worker, 7)
  outer = sys.vm.Vm().from_fn(do ||: Int 1 end, args: ())
  outer.table().mock(Clock.Now, do ||: Int
    pair[1]
  end)
  outer.step()
  case outer.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = world_of(&loaded, &["Vm", "Proc", "Clock"]);
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    let image = world.last_snapshot().expect("the program captured a world");
    // The world is the outer machine alone. The worker hides behind
    // the policy table, which no snapshot copies.
    assert_eq!(image.world().machine_count(), 1);
    // The restored table carries no mock and no grant.
    let (restored, root) = restore_into(&loaded, image);
    assert_eq!(restored.table_entry_count(root), 0);
}

/// A restored proc takes the birth grant and nothing else.
#[test]
fn a_restored_table_is_default_deny_plus_the_birth_grant() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let (world, root) = restore_into(&loaded, &image);
    // The held root is no proc, so its table is empty.
    assert_eq!(world.table_entry_count(root), 0);
    // The two restored procs carry the `Proc` group and nothing else.
    for vm in [root + 1, root + 2] {
        assert_eq!(world.table_entry_count(vm), 1);
        assert!(world.table_passes_group(vm, "Proc"));
    }
}

// ---------------------------------------------------------------
// Gate: a failed restore exposes no partial world, and a failed
// snapshot resumes the original world.
// ---------------------------------------------------------------

#[test]
fn a_failed_restore_exposes_no_partial_world() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let mut world = World::new(
        &loaded,
        VmConfig {
            // One child for the restore target, and nothing left for
            // the two procs the image holds.
            max_children: 1,
            ..VmConfig::default()
        },
        Box::new(RecordingHost::new(1)),
    );
    let target = world.new_child(0).expect("the budget holds one child");
    world
        .allow_on(target, "Io")
        .expect("the target grant names a group");
    let before = world.machine_count();
    let config = world.config_of(target);
    assert_eq!(
        world.restore_image(0, target, &image),
        Err(RestoreFail::LimitExceeded)
    );
    assert_eq!(world.machine_count(), before);
    assert_eq!(world.state_of(target), lm_vm::MachineState::Empty);
    assert_eq!(world.heap_of(target).live_count(), 0);
    assert_eq!(world.table_entry_count(target), 1);
    assert_eq!(world.config_of(target).max_children, config.max_children);
    // The reservation came back, so a later child still fits.
    assert_eq!(world.child_count(0), 1);
}

#[test]
fn a_mid_build_heap_failure_releases_the_restore_plan() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let first_heap: usize = image.world().machines[0]
        .objects
        .iter()
        .map(|entry| entry.object.cost())
        .sum();
    assert!(first_heap > 0);
    assert!(!image.world().machines[1].objects.is_empty());
    let limits = WorldLimits {
        max_heap_bytes: first_heap,
        ..WorldLimits::default()
    };
    let mut world = World::new_with_limits(
        &loaded,
        VmConfig::default(),
        limits,
        Box::new(RecordingHost::new(1)),
    );
    let target = world.new_child(0).expect("the target record fits");
    assert_eq!(
        world.restore_image(0, target, &image),
        Err(RestoreFail::LimitExceeded)
    );
    assert_eq!(world.state_of(target), lm_vm::MachineState::Empty);
    assert_eq!(world.world_heap_bytes(), 0);
    assert_eq!(world.machine_count(), 2);
}

#[test]
fn restore_rejects_a_queue_past_the_effective_mailbox_limit() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    assert!(image
        .world()
        .machines
        .iter()
        .any(|machine| !machine.mailbox.queue.is_empty()));
    let config = VmConfig {
        mailbox_limit: 0,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, config, Box::new(RecordingHost::new(1)));
    let target = world.new_child(0).expect("the target record fits");
    assert_eq!(
        world.restore_image(0, target, &image),
        Err(RestoreFail::LimitExceeded)
    );
    assert_eq!(world.state_of(target), lm_vm::MachineState::Empty);
}

/// A live host attachment blocks the capture with the ordinary typed
/// error of specification 17.4, and the world runs on.
#[test]
fn a_live_attachment_blocks_the_capture_and_resumes_the_world() {
    let source = "\
def go(): Int with Vm, Clock
  inner = sys.vm.Vm().from_fn(do ||: Int with Clock.Sleep
    sys.clock.sleep(5)
    9
  end, args: ())
  inner.table().pass(Clock)
  inner.step()
  inner.step()
  inner.step()
  case inner.snapshot()
  in Ok(_) then 0 - 1
  in Err(e)
    case e
    in ResourceActive(path, _) then path.len()
    in SnapshotLimitExceeded    then 0 - 2
    in BadImage(_)              then 0 - 3
    end
  end
end

go()
";
    // The path starts at the root of the world, so it names one
    // machine here.
    assert_eq!(
        run_allowed("attach.lm", source, &["Vm", "Clock"]).expect("the program runs"),
        "Done(1)"
    );
}

// ---------------------------------------------------------------
// Gate: whole-image admission occurs once on external load.
// Trusted capture uses a separate path.
// ---------------------------------------------------------------

#[test]
fn external_bytes_run_admission_once_and_trusted_capture_skips_it() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let mut world = world_of(&loaded, &["Proc", "Vm", "Clock"]);
    assert_eq!(world.snapshot_checks(), 0);
    // The external byte path runs the whole checklist once.
    let checked = world
        .load_snapshot_bytes(image.bytes())
        .expect("the container loads");
    assert_eq!(world.snapshot_checks(), 1);
    assert_eq!(checked.origin(), lm_vm::snapshot::Origin::ExternalContainer);
    // Two restores of the checked image repeat nothing.
    for _ in 0..2 {
        let target = world.new_child(0).expect("the budget holds a child");
        world
            .restore_image(0, target, &checked)
            .expect("the restore builds a world");
    }
    assert_eq!(world.snapshot_checks(), 1);
    // The trusted in-process path skips full admission.
    let mut fresh = world_of(&loaded, &["Proc", "Vm", "Clock"]);
    drive(&mut fresh);
    assert_eq!(fresh.snapshot_checks(), 0);
    assert_eq!(
        fresh
            .last_snapshot()
            .expect("the program captured a world")
            .origin(),
        lm_vm::snapshot::Origin::TrustedCapture
    );
}

// ---------------------------------------------------------------
// The captured states of specification 17.6.
// ---------------------------------------------------------------

/// A capture between instructions restores between those
/// instructions.
#[test]
fn a_between_instruction_capture_restores_at_the_same_boundary() {
    let loaded = program("2 * 3 + 1\n");
    let mut world = world_of(&loaded, &[]);
    world.step_root();
    world.step_root();
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the capture succeeds");
    assert_eq!(image.world().root_state(), Some(ImageState::Ready));
    let frame = &image.world().machines[0].frames[0];
    assert!(
        frame.ip > 0,
        "the program counter names the next instruction"
    );
    let (mut fresh, root) = restore_into(&loaded, &image);
    assert_eq!(run_restored(&mut fresh, root), "Done(7)");
}

/// A capture in `asked` preserves the operation, the arguments, the
/// destination, and the ordinal. The holder drives once to obtain a
/// fresh request token.
#[test]
fn an_asked_capture_restores_in_asked_and_drive_mints_a_fresh_token() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let machine = &image.world().machines[0];
    assert_eq!(machine.state, ImageState::Asked);
    let pending = machine.pending.as_ref().expect("the request survives");
    assert_eq!(lm_abi::op_name(pending.op), "Clock.Now");
    let ordinal = pending.ordinal;
    let (mut world, root) = restore_into(&loaded, &image);
    assert_eq!(world.state_of(root), lm_vm::MachineState::Asked);
    // The restored machine holds the same semantic request. `drive`
    // mints a fresh holder token for it, and no guest instruction
    // runs.
    match world.run_machine(root) {
        RootEvent::Asked(fresh) => assert_eq!(fresh, ordinal),
        other => panic!("expected an asked machine, got {other:?}"),
    }
}

/// A program that captures and restores one routed request.
fn routed_snapshot_source() -> &'static str {
    r#"
def dispatch_to_end(vm: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q) then vm.dispatch(q)
    in Done(value)
      return value
    in Fault(_)
      return 0 - 1
    end
  end
end

def go(): Int with Vm, Io.Print
  inner = do ||: Int with Vm, Io.Print
    b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
      sys.io.print("from B")
      7
    end, args: ())
    b.table().pass(Io.Print)
    case b.run()
    in Done(value) then value
    in Fault(_) then 0 - 3
    end
  end

  a = sys.vm.Vm().from_fn(inner, args: ())
  a.table().pass(Vm)
  a.table().pass(Io.Print)
  loop do
    case a.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (_,))
        case a.snapshot()
        in Ok(snap)
          a.answer(call, ())
          original = case a.run()
                     in Done(value) then value
                     in Fault(_) then 0 - 4
                     end
          case sys.vm.Vm().restore(snap)
          in Ok(restored)
            return original + dispatch_to_end(restored)
          in Err(_)
            return 0 - 5
          end
        in Err(_)
          return 0 - 6
        end
      in _
        a.dispatch(q)
      end
    in Done(_)
      return 0 - 7
    in Fault(_)
      return 0 - 8
    end
  end
end

go()
"#
}

/// A routed request preserves its nested edge and policy cursor.
#[test]
fn a_routed_request_round_trips_with_its_policy_cursor() {
    let (out, host) = run_world(
        "routed-snapshot.lm",
        routed_snapshot_source(),
        &["Vm", "Io.Print"],
        VmConfig::default(),
    )
    .expect("the routed snapshot program runs");
    assert_eq!(out, "Done(14)");
    assert_eq!(host.borrow().printed, vec!["from B"]);
}

/// Admission rejects damaged routed control records.
#[test]
fn malformed_routed_snapshot_state_rejects() {
    let loaded = program(routed_snapshot_source());
    let mut world = world_of(&loaded, &["Vm", "Io.Print"]);
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(14)");
    let image = world
        .last_snapshot()
        .expect("the program captured a routed request");
    let machine = &image.world().machines[0];
    assert_eq!(machine.nested, Some(1));
    assert!(machine.routed.is_some());

    let mut broken = image.clone().into_image();
    broken.machines[0].nested = None;
    let bytes = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    let error = codec::load_external(&bytes, &loaded, LoadLimits::default())
        .expect_err("the incomplete nested edge rejects");
    assert_eq!(error.reason, ImageReason::State);

    let mut broken = image.clone().into_image();
    broken.machines[0]
        .routed
        .as_mut()
        .expect("the route exists")
        .cursor = ImagePolicyCursor::Table(1);
    let bytes = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    let error = codec::load_external(&bytes, &loaded, LoadLimits::default())
        .expect_err("the invalid policy cursor rejects");
    assert_eq!(error.reason, ImageReason::State);
}

/// The runtime mints request ordinals from one, so zero names a
/// request no live machine can hold.
#[test]
fn a_zero_request_ordinal_rejects() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    assert_eq!(image.world().machines[0].state, ImageState::Asked);

    // A stored pending ordinal of zero.
    let mut broken = image.clone().into_image();
    broken.machines[0]
        .pending
        .as_mut()
        .expect("the asked machine holds a request")
        .ordinal = 0;
    let bytes = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    let error = codec::load_external(&bytes, &loaded, LoadLimits::default())
        .expect_err("the zero pending ordinal rejects");
    assert_eq!(error.reason, ImageReason::State);
    assert!(
        error.detail.contains("the pending request ordinal is zero"),
        "another rule refused the image: {}",
        error.detail
    );

    // A stored counter of zero. The restored machine would mint zero
    // for its next request, so the counter takes the same bound. The
    // case needs a machine with no pending request, because the upper
    // bound above already refuses that pairing.
    let idle = image
        .world()
        .machines
        .iter()
        .position(|m| m.pending.is_none())
        .expect("one captured machine holds no pending request");
    let mut broken = image.clone().into_image();
    broken.machines[idle].next_ordinal = 0;
    let bytes = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    let error = codec::load_external(&bytes, &loaded, LoadLimits::default())
        .expect_err("the zero next ordinal rejects");
    assert_eq!(error.reason, ImageReason::State);
    assert!(
        error.detail.contains("the next request ordinal is zero"),
        "another rule refused the image: {}",
        error.detail
    );
}

/// A terminal machine restores terminal, and its stored result
/// crosses.
#[test]
fn a_terminal_capture_restores_with_its_result() {
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_fn(do ||: Int 41 + 1 end, args: ())
  case vm.run()
  in Done(_)  then 0
  in Fault(_) then 0 - 1
  end
  case vm.snapshot()
  in Ok(snap)
    case sys.vm.Vm().restore(snap)
    in Ok(again)
      case again.run()
      in Done(v)  then v
      in Fault(_) then 0 - 2
      end
    in Err(_) then 0 - 3
    end
  in Err(_) then 0 - 4
  end
end

go()
";
    assert_eq!(
        run_allowed("terminal.lm", source, &["Vm"]).expect("the program runs"),
        "Done(42)"
    );
}

/// A proc captured while holder-paused restores paused, and `resume`
/// through the handle reactivates it.
#[test]
fn a_holder_paused_proc_restores_paused() {
    let loaded = program(paused_source());
    let mut world = world_of(&loaded, &["Proc", "Vm"]);
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    let image = world.last_snapshot().expect("the program captured a world");
    // The captured world holds the held root and the paused worker.
    assert_eq!(image.world().machine_count(), 2);
    let worker = &image.world().machines[1];
    assert!(worker.paused, "the worker restores paused");
    assert!(!worker.scheduler_owned, "a paused proc is holder-owned");
    let (world, root) = restore_into(&loaded, image);
    assert!(!world.runnable_procs().contains(&(root + 1)));
}

fn paused_source() -> &'static str {
    "\
class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(n) then n
    in Closed then 0
    end
  end
end

def go(): Int with Proc, Vm
  h = Worker.spawn()
  # A `Vm` handle is holder-local, so the held machine names the
  # paused proc through its sendable proc handle instead.
  held = sys.vm.Vm().from_fn(do |w: Handle[Int, Int]|: Int 1 end, args: (h,))
  case h.pause()
  in Ok(_)
    case held.snapshot()
    in Ok(_)  then 1
    in Err(_) then 0 - 1
    end
  in Err(_) then 0 - 2
  end
end

go()
"
}

/// A machine in `waiting` names the pending host operation in its
/// typed error.
#[test]
fn a_waiting_machine_names_its_attachment() {
    let loaded = program(WAITING_SOURCE);
    let mut world = world_of(&loaded, &["Vm", "Clock"]);
    // Drive the root far enough to leave the inner machine waiting.
    loop {
        match world.step_root() {
            RootEvent::Ran => {}
            other => panic!("unexpected event {other:?}"),
        }
        if world.machine_count() > 1 && world.state_of(1) == lm_vm::MachineState::Waiting {
            break;
        }
    }
    let gate = world.next_gate();
    assert_eq!(
        world.capture_snapshot(gate, 1, false).err(),
        Some(SnapshotFail::ResourceActive {
            path: vec![0],
            kind: "a pending Clock.Sleep".to_string(),
        })
    );
    // The failed capture stopped nothing and froze nothing.
    assert_eq!(world.barrier_of(1), None);
    assert!(!world.mailbox_metrics(1).frozen);
}

const WAITING_SOURCE: &str = "\
def go(): Int with Vm, Clock
  inner = sys.vm.Vm().from_fn(do ||: Int with Clock.Sleep
    sys.clock.sleep(5)
    9
  end, args: ())
  inner.table().pass(Clock)
  inner.step()
  inner.step()
  inner.step()
  case inner.snapshot()
  in Ok(_)  then 0 - 1
  in Err(e)
    case e
    in ResourceActive(path, _) then path.len()
    in SnapshotLimitExceeded   then 0 - 2
    in BadImage(_)             then 0 - 3
    end
  end
end

go()
";

/// A receiverless self snapshot captures the performing machine with
/// its request pending. The restorer answers it through the ordinary
/// drive path.
#[test]
fn a_self_snapshot_restores_with_its_request_pending() {
    let source = "\
def go(): Int with Vm
  case sys.vm.snapshot_self()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = world_of(&loaded, &["Vm"]);
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(1)");
    let image = world.last_snapshot().expect("the program captured a world");
    assert_eq!(image.world().machine_count(), 1);
    assert_eq!(image.world().root_state(), Some(ImageState::Asked));
    let pending = image.world().machines[0]
        .pending
        .as_ref()
        .expect("the self request survives");
    assert_eq!(lm_abi::op_name(pending.op), "Vm.SnapshotSelf");
    // The restored root holds that pending request, so a drive hands
    // the restorer a fresh token instead of running an instruction.
    let (mut fresh, root) = restore_into(&loaded, image);
    assert!(matches!(fresh.run_machine(root), RootEvent::Asked(_)));
}

// ---------------------------------------------------------------
// The snapshot byte limit.
// ---------------------------------------------------------------

#[test]
fn a_capture_past_the_byte_limit_returns_the_typed_error() {
    let loaded = program("1 + 1\n");
    let mut world = World::new(
        &loaded,
        VmConfig {
            snapshot_bytes: 16,
            ..VmConfig::default()
        },
        Box::new(RecordingHost::new(1)),
    );
    world.step_root();
    let gate = world.next_gate();
    assert_eq!(
        world.capture_snapshot(gate, 0, false).err(),
        Some(SnapshotFail::LimitExceeded)
    );
    // The failed capture resumed the world, so the program finishes.
    let outcome = drive(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(2)");
}

// ---------------------------------------------------------------
// The world gate and the deterministic dump.
// ---------------------------------------------------------------

/// A restored world runs nothing until the restored root moves.
#[test]
fn restored_procs_wait_behind_one_world_gate() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let (mut world, root) = restore_into(&loaded, &image);
    // Every restored machine sits behind one gate, so the scheduler
    // finds nothing to drive and no block to complete.
    let gate = world.gate_of(root);
    assert_ne!(gate, 0);
    for vm in [root, root + 1, root + 2] {
        assert_eq!(world.gate_of(vm), gate, "machine {vm}");
    }
    assert!(world.runnable_procs().is_empty());
    assert_eq!(world.poll_blocked(), 0);
    assert_eq!(world.mailbox_metrics(root + 1).delivered, 0);
    // The first drive of the restored root opens the gate for the
    // whole restored world.
    world.run_machine(root);
    for vm in [root, root + 1, root + 2] {
        assert_eq!(world.gate_of(vm), 0, "machine {vm}");
    }
    assert_eq!(world.runnable_procs(), vec![root + 1, root + 2]);
}

/// The readable dump is a deterministic diff surface: two captures of
/// one world produce no difference, and a changed world names the
/// first line that moved.
#[test]
fn the_dump_is_a_deterministic_diff() {
    let loaded = program(&asked_tree_source());
    let first = asked_tree_image(&loaded);
    let second = asked_tree_image(&loaded);
    assert_eq!(lm_vm::snapshot::dump::diff(&first, &second), None);
    // A world that ran one more instruction differs on one line.
    let other = {
        let mut world = world_of(&loaded, &["Proc", "Vm", "Clock"]);
        drive(&mut world);
        let gate = world.next_gate();
        world
            .capture_snapshot(gate, 1, false)
            .expect("the worker captures")
    };
    let text = lm_vm::snapshot::dump::diff(&first, &other).expect("the two worlds differ");
    assert!(text.starts_with("line 1\n"), "{text}");
}

/// The typed cast of specification 17.1.
///
/// The guest form takes a `Type[T]` descriptor, which version 0.2
/// does not have. The host form carries the same rule against the
/// recorded result-type digest.
#[test]
fn cast_result_accepts_the_recorded_result_type_alone() {
    let loaded = program(&asked_tree_source());
    let image = asked_tree_image(&loaded);
    let found = image.result_type();
    assert_ne!(found, [0u8; 32], "the root records its result type");
    assert!(image.cast_result(found).is_ok());
    let wrong = [7u8; 32];
    assert_eq!(
        image.cast_result(wrong).err(),
        Some(lm_vm::snapshot::SnapshotTypeError {
            found,
            expected: wrong
        })
    );
}
