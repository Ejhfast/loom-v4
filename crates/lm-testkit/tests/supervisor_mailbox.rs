//! A supervisor proc that owns a mailbox and drives a child.
//!
//! `week10_waits.rs` covers the interleaved case through `select`.
//! This case checks the plain combination: a proc with a mailbox
//! can hold and drive a machine.

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

/// The launcher: spawn, grant `Vm` through the paused machine, resume.
const LAUNCH: &str = r#"
h = Supervisor.spawn()
case h.pause()
in Ok(m)
  m.table().pass(Vm)
  m.table().pass(Io.Print)
  h.resume()
  h.send(Stop)
  case h.done()
  in Done(v)  then v
  in Fault(_) then -9
  end
in Err(_)
  -8
end
"#;

/// A: a proc with a mailbox drives a child and reads its command
/// afterwards. The command is already queued when it reads.
#[test]
fn a_mailbox_proc_drives_then_reads_a_queued_command() {
    let src = format!(
        r#"
enum Cmd
  Stop
end

class Supervisor < Proc[Cmd]
  def on_spawn(self): Int with Proc, Vm, Io.Print
    child = sys.vm.Vm().activate_or_fault(do ||: Int with Io.Print
      sys.io.print("a")
      sys.io.print("b")
      7
    end, args: ())
    child.table().pass(Io.Print)
    seen = 0
    loop do
      case child.drive()
      in Asked(q)
        seen = seen + 1
        child.dispatch(q)
      in Done(v)
        case self.receive()
        in Msg(_)
          return seen * 100 + v
        in Closed
          return seen * 10 + v
        end
      in Fault(_)
        return -1
      end
    end
  end
end
{LAUNCH}"#
    );
    let out = run(&src);
    println!("A drive then queued command: {out}");
    assert_eq!(out, "Done(207)");
}
