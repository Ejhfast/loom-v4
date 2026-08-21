//! `Vm.DriveFor`: a drive turn with an instruction bound.
//!
//! `drive` returns when the machine asks or stops. A machine that
//! performs nothing therefore keeps its holder waiting. A bounded turn
//! returns `None` instead, so the holder can do other work.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

fn run(src: &str) -> String {
    let bytes = compile_to_bytes("probe.lm", src).expect("the probe compiles");
    let loaded = load_bytes(&bytes).expect("the probe loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for g in ["Vm", "Proc", "Io.Print"] {
        world.allow(g).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    let text = world.show_outcome(&outcome);
    for vm in 0..world.machine_count() as u32 {
        if let Some(rec) = world.fault_of(vm) {
            println!("  m{vm}: {:?} {}", rec.code, rec.message);
        }
    }
    text
}

/// A child that never performs still returns control to its holder.
#[test]
fn a_bounded_turn_returns_control_from_a_silent_child() {
    let src = r#"
def spin(): Int
  i = 0
  while i < 2000
    i = i + 1
  end
  i
end

def supervise(vm: Run[Int]): Int with Vm
  turns = 0
  loop do
    case vm.drive_for(100)
    in None
      turns = turns + 1
    in Some(Asked(q))
      vm.dispatch(q)
    in Some(Done(v))
      return turns * 100000 + v
    in Some(Fault(_))
      return -1
    end
  end
end

supervise(sys.vm.Vm().activate_or_fault(spin, args: ()))
"#;
    let out = run(src);
    println!("bounded turns: {out}");
    // The child returns 2000. The turn count is the leading digits and
    // must be greater than zero, because the child never performs.
    let value: i64 = out
        .trim_start_matches("Done(")
        .trim_end_matches(')')
        .parse()
        .unwrap_or(-1);
    assert!(value > 100000, "no bounded turn happened: {out}");
    assert_eq!(value % 100000, 2000, "wrong child result: {out}");
}

/// An unbounded `drive` over the same child returns only at the end.
#[test]
fn an_unbounded_drive_sees_no_turn() {
    let src = r#"
def spin(): Int
  i = 0
  while i < 2000
    i = i + 1
  end
  i
end

def supervise(vm: Run[Int]): Int with Vm
  case vm.drive()
  in Asked(_)  then -1
  in Done(v)   then v
  in Fault(_)  then -2
  end
end

supervise(sys.vm.Vm().activate_or_fault(spin, args: ()))
"#;
    let out = run(src);
    println!("unbounded: {out}");
    assert_eq!(out, "Done(2000)");
}
