//! Week-7 resource suites: the host resource registry, the snapshot
//! preflight, and the parent child-machine reservation.

use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::{load_bytes, MachineState, RecordingHost, RootEvent, VmConfig, World};

/// A machine that waits on a suspending operation holds one live host
/// attachment, and the completion closes it.
#[test]
fn a_suspended_operation_registers_and_closes_one_resource() {
    let source = "def go() with Clock.Sleep\n  sys.clock.sleep(1)\nend\ngo()\n";
    let bytes = compile_to_bytes("t.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Clock").expect("the grant names a group");
    // Step until the machine waits on the host.
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
    assert_eq!(world.state_of(world.root()), MachineState::Waiting);
    assert_eq!(world.resource_count(world.root()), 1);
    // A live host attachment blocks a snapshot.
    assert_eq!(
        world.snapshot_preflight(world.root()),
        Err(lm_vm::FaultCode::BoundaryViolation)
    );
    // Run to the end. The completion closes the record.
    loop {
        match world.step_root() {
            RootEvent::Done(_) | RootEvent::Fault(_) => break,
            _ => {}
        }
    }
    assert_eq!(world.resource_count(world.root()), 0);
    assert!(world.snapshot_preflight(world.root()).is_ok());
}

/// A host that suspends an operation the manifest declares machine
/// state breaks its contract, and the machine faults.
#[test]
fn a_host_that_suspends_a_machine_state_operation_faults() {
    struct BadHost;

    impl lm_vm::Host for BadHost {
        fn start(&mut self, _op: u32, _args: Vec<lm_vm::HostArg>) -> lm_vm::HostStart {
            lm_vm::HostStart::Waiting(1)
        }

        fn poll(&mut self, _token: u64) -> Option<lm_vm::HostValue> {
            Some(lm_vm::HostValue::Int(1))
        }

        fn wait(&mut self, _token: u64) -> lm_vm::HostValue {
            lm_vm::HostValue::Int(1)
        }
    }

    // `Clock.Now` declares machine state: it must complete inside the
    // host call.
    let source = "def go(): Int with Clock.Now\n  sys.clock.now()\nend\ngo()\n";
    let bytes = compile_to_bytes("t.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(BadHost));
    world.allow("Clock").expect("the grant names a group");
    let outcome = world.run_root();
    assert_eq!(
        world.show_outcome(&outcome),
        "Fault(HostFault)",
        "a suspended machine-state operation must fault"
    );
    assert_eq!(world.resource_count(world.root()), 0);
}

/// A parent reserves each child from its own budget. The reservation
/// is fail-atomic: the refused call creates no machine.
#[test]
fn the_parent_child_budget_is_fail_atomic() {
    let source = "\
def go(): Int with Vm
  a = sys.vm.Vm()
  b = sys.vm.Vm()
  c = sys.vm.Vm()
  1
end

go()
";
    let bytes = compile_to_bytes("t.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let config = VmConfig {
        max_children: 2,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, config, Box::new(lm_vm::NullHost));
    world.allow("Vm").expect("the grant names a group");
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Fault(InvalidVmState)");
    // Two children exist; the third call created nothing.
    assert_eq!(world.child_count(world.root()), 2);
    assert_eq!(world.machine_count(), 3);
}

/// The child receives the rest of the parent budget, so a machine
/// tower can never grow deeper than the budget the root minted.
#[test]
fn a_child_inherits_the_rest_of_the_budget() {
    let source = "\
def go(): Int with Vm
  outer = sys.vm.Vm().from_object(do ||: Int with Vm
    inner = sys.vm.Vm().from_object(do ||: Int
      7
    end, args: ())
    case inner.run()
    in Done(v)  then v
    in Fault(_) then 0 - 1
    end
  end, args: ())
  outer.table().pass(Vm)
  case outer.run()
  in Done(v)  then v
  in Fault(_) then 0 - 2
  end
end

go()
";
    let bytes = compile_to_bytes("t.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    // The root may mint one child, and that child may mint none.
    let config = VmConfig {
        max_children: 1,
        ..VmConfig::default()
    };
    let mut world = World::new(&loaded, config, Box::new(lm_vm::NullHost));
    world.allow("Vm").expect("the grant names a group");
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(-2)");
    // A wider budget lets the same tower run.
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(lm_vm::NullHost));
    world.allow("Vm").expect("the grant names a group");
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(7)");
}

/// A snapshot preflight of a machine without host work orders the
/// reachable graph.
#[test]
fn the_snapshot_preflight_orders_a_clean_machine() {
    let source = "xs = [1, 2, 3]\nxs.freeze()\nxs.len()\n";
    let bytes = compile_to_bytes("t.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(lm_vm::NullHost));
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(3)");
    let ordered = world
        .snapshot_preflight(world.root())
        .expect("a clean machine preflights");
    assert!(ordered <= world.heap_of(world.root()).live_count());
}

/// The nested sandbox example still runs on the production path.
#[test]
fn the_nested_sandbox_example_runs_unchanged() {
    let source =
        std::fs::read_to_string(repo_root().join("examples/04-effects/mock-clock.lm")).unwrap();
    assert_eq!(
        lm_testkit::run_allowed("mock-clock.lm", &source, &["Vm"]).unwrap(),
        "Done(123)"
    );
}
