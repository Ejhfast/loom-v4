//! Checkpointing a supervised concurrent child.
//!
//! A snapshot of a driven world must succeed while procs of that
//! world hold requests for the driver.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

fn run(src: &str, grants: &[&str]) -> (String, bool) {
    let bytes = compile_to_bytes("probe.lm", src).expect("the probe compiles");
    let loaded = load_bytes(&bytes).expect("the probe loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for g in grants {
        world.allow(g).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    let text = world.show_outcome(&outcome);
    let captured = world.last_snapshot().is_some();
    for vm in 0..world.machine_count() as u32 {
        if let Some(rec) = world.fault_of(vm) {
            println!("  m{vm}: {:?} {}", rec.code, rec.message);
        }
    }
    (text, captured)
}

/// One proc under the child. The supervisor tries to capture at the
/// request point.
#[test]
fn a_supervisor_captures_a_child_with_one_proc() {
    let src = r#"
def app(): Int with Vm, Proc, Io.Print
  w = sys.vm.Vm().activate_or_fault(do ||: Int
    i = 0
    while i < 300
      i = i + 1
    end
    i
  end, args: ())
  h = sys.proc.run(w)
  sys.io.print("[app]")
  case h.done()
  in Done(v)  then v
  in Fault(_) then -1
  end
end

def supervise(vm: Run[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(request)
      case request
      in Call(Io.Print, call, (_,))
        took = vm.snapshot().is_ok()
        vm.answer(call, ())
        if not took
          return -7
        end
      in _
        vm.dispatch(request)
      end
    in Done(value)
      return value
    in Fault(_)
      return -2
    end
  end
end

c = sys.vm.Vm().activate_or_fault(app, args: ())
c.table().pass(Vm)
c.table().pass(Proc)
c.table().pass(Io.Print)
supervise(c)
"#;
    let (out, captured) = run(src, &["Vm", "Proc", "Io.Print"]);
    println!("one-proc capture: {out} captured={captured}");
    assert_eq!(out, "Done(300)");
    assert!(captured, "the supervisor captured nothing");
}

/// Two procs surface at once, so the surface holds a queue. This is
/// the case my new capture restriction refuses.
#[test]
fn a_supervisor_captures_a_child_with_two_surfacing_procs() {
    let src = r#"
def worker(): Int with Io.Print
  sys.io.print("[worker]")
  5
end

def app(): Int with Vm, Proc, Io.Print
  a = sys.vm.Vm().activate_or_fault(worker, args: ())
  a.table().pass(Io.Print)
  b = sys.vm.Vm().activate_or_fault(worker, args: ())
  b.table().pass(Io.Print)
  ha = sys.proc.run(a)
  hb = sys.proc.run(b)
  first = case ha.done()
  in Done(v)  then v
  in Fault(_) then -1
  end
  second = case hb.done()
  in Done(v)  then v
  in Fault(_) then -1
  end
  first + second
end

def supervise(vm: Run[Int], mut misses: [Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(request)
      case request
      in Call(Io.Print, call, (_,))
        if not vm.snapshot().is_ok()
          misses.push(1)
        end
        vm.answer(call, ())
      in _
        vm.dispatch(request)
      end
    in Done(value)
      return value
    in Fault(_)
      return -2
    end
  end
end

c = sys.vm.Vm().activate_or_fault(app, args: ())
c.table().pass(Vm)
c.table().pass(Proc)
c.table().pass(Io.Print)
misses: [Int] = []
r = supervise(c, misses)
r * 100 + misses.len()
"#;
    let (out, captured) = run(src, &["Vm", "Proc", "Io.Print"]);
    println!("two-proc capture: {out} captured={captured}");
    // 10 = 5 + 5. The trailing digit counts refused captures.
    assert_eq!(out, "Done(1000)", "captures were refused");
}
