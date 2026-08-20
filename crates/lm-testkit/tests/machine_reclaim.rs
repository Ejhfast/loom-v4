//! Machine reclamation: a record that no live machine names returns
//! its slot and its child budget.
//!
//! A machine is data (specification 1). A driver that builds one
//! machine for each branch of a search must therefore pay for the
//! branches it still holds, not for every branch it ever built.

use lm_testkit::run_allowed;
use lm_vm::VmConfig;

/// A driver that runs far more child machines than its child budget.
///
/// The budget is 1024 by default. This program builds 4000 machines,
/// and it holds one at a time. Each finished machine becomes
/// unreachable, so the world reclaims its record before the budget
/// runs out.
#[test]
fn a_driver_outlives_its_child_budget() {
    let source = "def once(n: Int): Int with Vm
  vm = sys.vm.Vm().activate(do |x: Int|: Int
    x + 1
  end, args: (n,))
  case vm.run()
  in Done(value) then value
  in Fault(_)    then 0 - 1
  end
end

total = 0
i = 0
while i < 4000
  total = total + once(i)
  i = i + 1
end
total";
    let budget = VmConfig::default().max_children;
    assert_eq!(budget, 1_024, "the test states the default budget");
    // The sum of `1..=4000`.
    assert_eq!(
        run_allowed("reclaim.lm", source, &["Vm"]).unwrap(),
        "Done(8002000)"
    );
}

/// The same shape through capture and restore.
///
/// This is the search step: stop one machine at a choice point, copy
/// the world, and restore one world for each candidate. Every restore
/// charges the driver one child, so the run proves that reclamation
/// covers the restore path too.
#[test]
fn a_search_driver_restores_past_its_child_budget() {
    let source = "def restore_run(snap: Snapshot[Int]): Int with Vm
  case sys.vm.Vm().restore(snap)
  in Ok(restored)
    case restored.run()
    in Done(value) then value
    in Fault(_)    then 0 - 1
    end
  in Err(_) then 0 - 2
  end
end

vm = sys.vm.Vm().activate(do ||: Int
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
in Err(_) then 0 - 3
end";
    assert_eq!(
        run_allowed("reclaim-restore.lm", source, &["Vm"]).unwrap(),
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
  held.push(sys.vm.Vm().activate(do ||: Int
    7
  end, args: ()))
  i = i + 1
end
held.len()";
    assert_eq!(
        run_allowed("reclaim-held.lm", source, &["Vm"]).unwrap(),
        "Fault(InvalidVmState)"
    );
}
