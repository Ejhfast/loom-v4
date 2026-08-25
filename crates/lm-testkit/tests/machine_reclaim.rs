//! Machine reclamation: a record that no live machine names returns
//! its slot and its child budget.
//!
//! A machine is data (specification 1). A driver that builds one
//! machine for each branch of a search must therefore pay for the
//! branches it still holds, not for every branch it ever built.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, NullHost, VmConfig, World, WorldMetrics};

const CHILD_CHURN: &str = "def once(n: Int): Int with Vm
  vm = sys.vm.Vm().activate_or_fault(do |x: Int|: Int
    x + 1
  end, args: (n,))
  case vm.run()
  in Ok(value) then value
  in Err(_)    then -1
  end
end

total = 0
i = 0
while i < 4000
  total = total + once(i)
  i = i + 1
end
total";

fn run_with_config(
    name: &str,
    source: &str,
    config: VmConfig,
) -> (String, usize, usize, WorldMetrics) {
    let bytes = compile_to_bytes(name, source).expect("the source compiles");
    let loaded = load_bytes(&bytes).expect("the artifact loads");
    let mut world = World::new(&loaded, config, Box::new(NullHost));
    world.allow("Vm").expect("the Vm grant exists");
    let outcome = lm_proc::run_world(&mut world);
    (
        world.show_outcome(&outcome),
        world.machine_count(),
        world.vm_image_count(),
        world.metrics(),
    )
}

fn run_with_child_limit(name: &str, source: &str, max_children: u32) -> String {
    let config = VmConfig {
        max_children,
        ..VmConfig::default()
    };
    run_with_config(name, source, config).0
}

/// A driver that runs far more child machines than its child budget.
///
/// The test uses a 64-child budget and builds 4,000 machines.
/// It holds one machine at a time, so reclamation returns each slot.
#[test]
fn a_driver_outlives_its_child_budget() {
    // The sum of `1..=4000`.
    assert_eq!(
        run_with_child_limit("reclaim.lm", CHILD_CHURN, 64),
        "Done(8002000)"
    );
}

/// Generous limits do not become reclamation intervals.
#[test]
fn generous_limits_keep_machine_tables_bounded() {
    let (outcome, machines, images, metrics) =
        run_with_config("reclaim-soft.lm", CHILD_CHURN, VmConfig::default());
    assert_eq!(outcome, "Done(8002000)");
    assert!(
        machines <= 2_048,
        "the machine table has {machines} records"
    );
    assert!(images <= 2_048, "the image table has {images} records");
    assert!(metrics.reclamation_passes > 0, "{metrics:?}");
    assert!(metrics.machine_records_reclaimed > 0, "{metrics:?}");
    assert!(metrics.vm_image_records_reclaimed > 0, "{metrics:?}");
}

/// A growing live set increases the next reclamation threshold.
#[test]
fn live_machine_growth_uses_adaptive_reclamation() {
    let source = "held: [Run[Int]] = []
i = 0
while i < 3000
  held.push(sys.vm.Vm().activate_or_fault(do ||: Int
    7
  end, args: ()))
  i = i + 1
end
held.len()";
    let (outcome, machines, images, metrics) =
        run_with_config("reclaim-live.lm", source, VmConfig::default());
    assert_eq!(outcome, "Done(3000)");
    assert_eq!(machines, 3_001);
    assert_eq!(images, 3_000);
    assert!(metrics.reclamation_passes <= 2, "{metrics:?}");
    assert_eq!(metrics.machine_records_reclaimed, 0, "{metrics:?}");
    assert_eq!(metrics.vm_image_records_reclaimed, 0, "{metrics:?}");
}

/// The same shape through capture and restore.
///
/// This is the search step: stop one machine at a choice point, copy
/// the world, and restore one world for each candidate. Every restore
/// charges the driver one child, so the run proves that reclamation
/// covers the restore path too.
#[test]
fn a_search_driver_restores_past_its_child_budget() {
    let source = "def restore_run(snap: RunSnapshot[Int]): Int with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(restored)
    case restored.run()
    in Ok(value) then value
    in Err(_)    then -1
    end
  in Err(_) then -2
  end
end

vm = sys.vm.Vm().activate_or_fault(do ||: Int
  20 + 22
end, args: ())
vm.step()
case vm.snapshot()
in Ok(snap)
  total = 0
  i = 0
  while i < 3000
    total = total + restore_run(snap)
    i = i + 1
  end
  total
in Err(_) then -3
end";
    assert_eq!(
        run_with_child_limit("reclaim-restore.lm", source, 64),
        "Done(126000)"
    );
}

/// A machine a guest value still names survives the pass.
///
/// The program keeps every machine it builds in one list, so no
/// record is reclaimable. The run must therefore stop at the child
/// budget with `InvalidVmState`, exactly as it did before.
#[test]
fn a_held_machine_is_never_reclaimed() {
    let source = "held: [Run[Int]] = []
i = 0
while i < 2000
  held.push(sys.vm.Vm().activate_or_fault(do ||: Int
    7
  end, args: ()))
  i = i + 1
end
held.len()";
    assert_eq!(
        run_with_child_limit("reclaim-held.lm", source, 64),
        "Fault(InvalidVmState)"
    );
}
