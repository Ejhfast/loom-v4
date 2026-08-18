//! Supervision across the scheduler boundary.
//!
//! A driver serves the requests of every machine of the world it
//! drives. A proc of that world runs on its own scheduler task, so its
//! request reaches the driver through the parked-route path.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};

fn run(src: &str, grants: &[&str]) -> (String, Vec<String>) {
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
    let faults = (0..world.machine_count() as u32)
        .map(|vm| match world.fault_of(vm) {
            Some(rec) => format!("m{vm}: {:?} {}", rec.code, rec.message),
            None => format!("m{vm}: -"),
        })
        .collect();
    (text, faults)
}

/// BUG B: a supervised child that spawns a proc and waits on it.
#[test]
fn a_driver_serves_a_child_that_waits_on_a_proc() {
    let src = r#"
def child(): Int with Vm, Proc, Io.Print
  worker = sys.vm.Vm().from_fn(do ||: Int
    i = 0
    while i < 500
      i = i + 1
    end
    i
  end, args: ())
  h = sys.proc.run(worker)
  sys.io.print("[child]")
  case h.done()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
end

def supervise(vm: Vm[Int], mut seen: [String]): Int with Vm
  loop do
    case vm.drive()
    in Asked(request)
      case request
      in Call(Io.Print, call, (text,))
        seen.push(text)
        vm.answer(call, ())
      in _
        vm.dispatch(request)
      end
    in Done(value)
      return value
    in Fault(_)
      return 0 - 2
    end
  end
end

c = sys.vm.Vm().from_fn(child, args: ())
c.table().pass(Vm)
c.table().pass(Proc)
c.table().pass(Io.Print)
seen: [String] = []
r = supervise(c, seen)
r * 10 + seen.len()
"#;
    let (out, faults) = run(src, &["Vm", "Proc", "Io.Print"]);
    println!("BUG B outcome: {out}");
    for f in &faults {
        println!("  {f}");
    }
    // The worker returns 500, and the supervisor saw one print.
    assert_eq!(out, "Done(5001)", "faults: {faults:?}");
}

/// BUG B, deeper: the supervised child waits on a proc that itself
/// spawns a proc.
#[test]
fn a_driver_serves_two_levels_of_procs() {
    let src = r#"
def child(): Int with Vm, Proc, Io.Print
  outer = sys.vm.Vm().from_fn(do ||: Int with Vm, Proc
    inner = sys.vm.Vm().from_fn(do ||: Int
      i = 0
      while i < 100
        i = i + 1
      end
      i
    end, args: ())
    g = sys.proc.run(inner)
    case g.done()
    in Done(v)  then v + 1
    in Fault(_) then 0 - 1
    end
  end, args: ())
  outer.table().pass(Vm)
  outer.table().pass(Proc)
  h = sys.proc.run(outer)
  sys.io.print("[child]")
  case h.done()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
end

def supervise(vm: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(request)
      vm.dispatch(request)
    in Done(value)
      return value
    in Fault(_)
      return 0 - 2
    end
  end
end

c = sys.vm.Vm().from_fn(child, args: ())
c.table().pass(Vm)
c.table().pass(Proc)
c.table().pass(Io.Print)
supervise(c)
"#;
    let (out, faults) = run(src, &["Vm", "Proc", "Io.Print"]);
    println!("BUG B deep outcome: {out}");
    for f in &faults {
        println!("  {f}");
    }
    assert_eq!(out, "Done(101)", "faults: {faults:?}");
}

/// BUG A: a machine that reaches a terminal state outside its own
/// scheduler slice must wake the tasks blocked on `Done`.
#[test]
fn a_holder_driven_terminal_wakes_its_waiters() {
    let src = r#"
class Waiter < Proc[Handle[Never, Int]]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(h)
      case h.done()
      in Done(v)  then v
      in Fault(_) then 0 - 1
      end
    in Closed
      0 - 2
    end
  end
end

vm = sys.vm.Vm().from_fn(do ||: Int
  i = 0
  while i < 200000
    i = i + 1
  end
  i
end, args: ())
p = sys.proc.run(vm)
q = Waiter.spawn()
q.send(p)
j = 0
while j < 5000
  j = j + 1
end
case p.pause()
in Ok(machine)
  machine.run()
  case q.done()
  in Done(v)  then v
  in Fault(_) then 0 - 3
  end
in Err(_)
  0 - 4
end
"#;
    let (out, faults) = run(src, &["Vm", "Proc"]);
    println!("BUG A outcome: {out}");
    for f in &faults {
        println!("  {f}");
    }
    assert_eq!(out, "Done(200000)", "faults: {faults:?}");
}

/// BUG A, send variant: a sender blocked on a full mailbox of a proc
/// that the holder drives to its terminal state.
#[test]
fn a_holder_driven_terminal_wakes_a_blocked_sender() {
    let src = r#"
class Spinner < Proc[Int]
  def on_spawn(self): Int with Proc
    i = 0
    while i < 200000
      i = i + 1
    end
    i
  end
end

class Pusher < Proc[Handle[Int, Int]]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(h)
      k = 0
      while k < 100
        case h.send(k)
        in Sent
          k = k + 1
        in Closed
          return 0 - 1
        in Fault(_)
          return 0 - 2
        end
      end
      k
    in Closed
      0 - 3
    end
  end
end

p = Spinner.spawn()
q = Pusher.spawn()
q.send(p)
j = 0
while j < 5000
  j = j + 1
end
case p.pause()
in Ok(machine)
  machine.run()
  case q.done()
  in Done(v)  then v
  in Fault(_) then 0 - 4
  end
in Err(_)
  0 - 5
end
"#;
    let (out, faults) = run(src, &["Vm", "Proc"]);
    println!("BUG A send outcome: {out}");
    assert_eq!(out, "Done(-2)", "faults: {faults:?}");
}

/// The capability that was impossible: audit a child that uses procs.
#[test]
fn a_driver_sees_every_effect_of_every_proc() {
    let src = r#"
def worker(n: Int): Int with Io.Print
  sys.io.print("[worker]")
  n * 2
end

def app(): Int with Vm, Proc, Io.Print
  a = sys.vm.Vm().from_fn(worker, args: (10,))
  a.table().pass(Io.Print)
  b = sys.vm.Vm().from_fn(worker, args: (20,))
  b.table().pass(Io.Print)
  ha = sys.proc.run(a)
  hb = sys.proc.run(b)
  sys.io.print("[app]")
  first = case ha.done()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
  second = case hb.done()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
  first + second
end

def audit(vm: Vm[Int], mut seen: [String]): Int with Vm
  loop do
    case vm.drive()
    in Asked(request)
      case request
      in Call(Io.Print, call, (text,))
        seen.push(text)
        vm.answer(call, ())
      in _
        vm.dispatch(request)
      end
    in Done(value)
      return value
    in Fault(_)
      return 0 - 9
    end
  end
end

c = sys.vm.Vm().from_fn(app, args: ())
c.table().pass(Vm)
c.table().pass(Proc)
c.table().pass(Io.Print)
seen: [String] = []
r = audit(c, seen)
r * 100 + seen.len()
"#;
    let (out, faults) = run(src, &["Vm", "Proc", "Io.Print"]);
    println!("audit-with-procs outcome: {out}");
    // 60 = 10*2 + 20*2, and the supervisor saw all three prints:
    // the app print and one from each worker proc.
    assert_eq!(out, "Done(6003)", "faults: {faults:?}");
}
