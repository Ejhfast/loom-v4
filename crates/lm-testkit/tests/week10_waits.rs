//! Typed waits and selectable proc control.

use lm_heap::Object;
use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::snapshot::{codec, ImageBlock, ImageReason};
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn run(source: &str, grants: &[&str]) -> String {
    let bytes = compile_to_bytes("waits.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in grants {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn a_host_operation_can_supply_a_wait_source() {
    let source = r#"
sys.clock.sleep.wait(0).wait()
"ready"
"#;

    assert_eq!(run(source, &["Clock", "Wait"]), "Done(\"ready\")");
}

#[test]
fn a_losing_console_read_keeps_its_bytes() {
    let source = r#"
selected = select
in sys.clock.sleep.wait(0) -> _
  "timer"
in sys.io.read_bytes.wait(3) -> _
  "input"
end
hex = case sys.io.read_bytes(3)
in Ok(bytes) then bytes.hex()
in Err(_) then "error"
end
(selected, hex)
"#;
    let bytes = compile_to_bytes("host-wait-race.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().input_bytes.extend_from_slice(b"abc");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(Rc::clone(&host)));
    for grant in ["Clock", "Io", "Wait"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((\"timer\", \"616263\"))"
    );
}

#[test]
fn one_select_combines_host_mailbox_and_drive_sources() {
    let source = r#"
enum Command
  Stop
end

def spin(): Never
  loop do
    ()
  end
end

class Supervisor < Proc[Command]
  def on_spawn(self): String with Proc, Vm, Wait, Clock
    child = sys.vm.Vm().activate_or_fault(spin, args: ())
    select
    in child.drive_wait() -> _
      "drive"
    in sys.clock.sleep.wait(0) -> _
      "clock"
    in self.receive_wait() -> _
      "mailbox"
    end
  end
end

supervisor = Supervisor.spawn()
supervisor.send(Stop)
case supervisor.done()
in Done(value) then value
in Fault(fault) then fault.code()
end
"#;

    assert_eq!(
        run(source, &["Proc", "Vm", "Wait", "Clock"]),
        "Done(\"mailbox\")"
    );
}

#[test]
fn machine_failure_cancels_mixed_prepared_sources() {
    let source = r#"
def spin(): Never
  loop do
    ()
  end
end

class Probe < Proc
  def on_spawn(self): Never with Proc, Vm, Io, Clock
    child = sys.vm.Vm().activate_or_fault(spin, args: ())
    input = sys.io.read_bytes.wait(3)
    timer = sys.clock.sleep.wait(10)
    mailbox = self.receive_wait()
    drive = child.drive_wait()
    panic("stop")
  end
end

probe = Probe.spawn()
probe.done()
case sys.io.read_bytes(3)
in Ok(bytes) then bytes.hex()
in Err(_) then "error"
end
"#;
    let bytes = compile_to_bytes("mixed-wait-cleanup.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().input_bytes.extend_from_slice(b"abc");
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(Rc::clone(&host)));
    for grant in ["Proc", "Vm", "Io", "Clock"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(\"616263\")");
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn a_nonwaitable_operation_rejects_wait_syntax() {
    let source = r#"
sys.io.write.wait(b"x")
"#;
    let error = compile_to_bytes("bad-operation-wait.lm", source)
        .expect_err("the nonwaitable operation rejects");
    assert!(error.contains("is not a wait source"), "{error}");
}

#[test]
fn a_live_operation_wait_blocks_snapshot_capture() {
    let source = r#"
def capture(): String with Io, Vm, Wait
  input = sys.io.read_bytes.wait(1)
  blocker = case sys.vm.snapshot_self()
  in Ok(_) then "captured"
  in Err(ResourceActive(_, name)) then name
  in Err(_) then "wrong error"
  end
  input.cancel()
  blocker
end

capture()
"#;

    assert_eq!(
        run(source, &["Io", "Vm", "Wait"]),
        "Done(\"a pending Io.ReadBytes\")"
    );
}

#[test]
fn every_select_arm_needs_a_wait_value() {
    let source = r#"
def value(): Int
  1
end

child = sys.vm.Vm().activate_or_fault(value, args: ())
select
in child.drive_wait() -> _
  1
in 2 -> number
  number
end
"#;

    let error = compile_to_bytes("bad-select.lm", source).expect_err("the second arm is invalid");
    assert!(error.contains("`choose` needs a wait"), "{error}");
}

#[test]
fn three_select_arms_preserve_the_selected_result() {
    let source = r#"
def spin(): Never
  loop do
    ()
  end
end

def answer(): Int
  7
end

first = sys.vm.Vm().activate_or_fault(spin, args: ())
second = sys.vm.Vm().activate_or_fault(answer, args: ())
third = sys.vm.Vm().activate_or_fault(spin, args: ())
second.run()

select
in first.drive_wait() -> _
  "first"
in second.drive_wait() -> event
  case event
  in Done(7) then "second"
  in Done(_) then "wrong value"
  in Asked(_) then "unexpected request"
  in Fault(_) then "unexpected fault"
  end
in third.drive_wait() -> _
  "third"
end
"#;

    assert_eq!(run(source, &["Vm", "Wait"]), "Done(\"second\")");
}

#[test]
fn a_mailbox_command_interrupts_an_active_drive_wait() {
    let source = std::fs::read_to_string(
        repo_root().join("examples/09-handles-and-supervision/12-selectable-supervisor.lm"),
    )
    .expect("the example reads");

    assert_eq!(
        run(&source, &["Proc", "Vm", "Wait"]),
        "Done(\"the supervisor stopped the child\")"
    );
}

#[test]
fn a_losing_receive_keeps_its_mailbox_message() {
    let source = r#"
enum Command
  Ping
end

def answer(): Int
  7
end

class Supervisor < Proc[Command]
  def on_spawn(self): Int with Proc, Vm, Wait
    child = sys.vm.Vm().activate_or_fault(answer, args: ())
    child.run()
    select
    in child.drive_wait() -> event
      case event
      in Done(value)
        case self.receive()
        in Msg(Ping) then value
        in Closed then -1
        end
      in Asked(_) then -2
      in Fault(_) then -3
      end
    in self.receive_wait() -> _
      -4
    end
  end
end

supervisor = Supervisor.spawn()
case supervisor.pause()
in Ok(vm)
  vm.table().pass(Vm)
  vm.table().pass(Wait)
  supervisor.resume()
  ()
in Err(_) then ()
end
supervisor.send(Ping)
case supervisor.done()
in Done(value) then value
in Fault(_) then -5
end
"#;

    assert_eq!(run(source, &["Proc", "Vm", "Wait"]), "Done(7)");
}

#[test]
fn a_cancelled_wait_token_is_stale() {
    let source = r#"
class Probe < Proc
  def on_spawn(self): Int with Proc, Wait
    pending = self.receive_wait()
    pending.cancel()
    pending.wait()
    1
  end
end

probe = Probe.spawn()
case probe.pause()
in Ok(vm)
  vm.table().pass(Wait)
  probe.resume()
  ()
in Err(_) then ()
end
case probe.done()
in Done(_) then "unexpected success"
in Fault(fault) then fault.code()
end
"#;

    assert_eq!(
        run(source, &["Proc", "Vm", "Wait"]),
        "Done(\"InvalidVmState\")"
    );
}

#[test]
fn an_active_wait_survives_snapshot_restore() {
    let source = r#"
enum Command
  Ping
end

class Waiting < Proc[Command]
  def on_spawn(self): Int with Proc, Wait
    self.receive_wait().wait()
    1
  end
end

class Gate < Proc
  def on_spawn(self): Int
    1
  end
end

waiting = Waiting.spawn()
case waiting.pause()
in Ok(vm)
  vm.table().pass(Wait)
  waiting.resume()
  ()
in Err(_) then ()
end

# The gate lets the waiting proc park before capture.
gate = Gate.spawn()
gate.done()
case waiting.snapshot_wait(0)
in Ok(_) then "captured"
in Err(_) then "capture failed"
end
"#;
    let bytes = compile_to_bytes("wait-snapshot.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Proc", "Vm", "Wait"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(\"captured\")");
    let image = world.last_snapshot().expect("the snapshot exists").clone();
    assert_eq!(image.world().machines[0].waits.len(), 1);
    assert!(matches!(
        image.world().machines[0].block,
        Some(ImageBlock::Wait { .. })
    ));

    let mut invalid = image.world().clone();
    let next_wait = invalid.machines[0].next_wait;
    let held = invalid.machines[0]
        .objects
        .iter_mut()
        .find_map(|entry| match &mut entry.object {
            Object::NativeWait { token, .. } => Some(token),
            _ => None,
        })
        .expect("the image holds its active wait value");
    *held = next_wait;
    let bytes = codec::encode(&invalid, usize::MAX).expect("the damaged image encodes");
    let error = codec::load_external(&bytes, &loaded, lm_vm::snapshot::LoadLimits::default())
        .expect_err("a future wait token rejects");
    assert_eq!(error.reason, ImageReason::State);

    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let slot = fresh.new_child(0).expect("the restore target exists");
    let restored = fresh
        .restore_image(0, slot, &image)
        .expect("the wait image restores");
    let barrier = fresh.next_gate();
    let recaptured = fresh
        .capture_snapshot(barrier, restored, false)
        .expect("the restored wait captures");
    assert_eq!(recaptured.world().machines[0].waits.len(), 1);
}
