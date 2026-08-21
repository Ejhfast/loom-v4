//! Week-10 file handles and snapshot behavior.

use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn source(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).expect("the example reads")
}

#[test]
fn a_recording_host_services_file_handles() {
    let text = source("examples/09-handles-and-supervision/01-read-file.lm");
    let bytes = compile_to_bytes("read-file.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"hello from memory".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    world.allow("Fs").expect("the filesystem grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"hello from memory\")");
    assert_eq!(world.resource_count(0), 0);
}

#[test]
fn all_six_file_effects_complete_a_round_trip() {
    let text = source("examples/09-handles-and-supervision/02-round-trip-file.lm");
    let bytes = compile_to_bytes("round-trip-file.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host.clone()));
    world.allow("Fs").expect("the filesystem grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"hello\")");
    assert_eq!(
        host.borrow().file("round-trip.txt"),
        Some(b"hello".as_slice())
    );
    assert_eq!(world.resource_count(0), 0);
}

#[test]
fn a_holder_closes_live_handles_before_a_snapshot() {
    let text = source("examples/09-handles-and-supervision/08-checkpoint-now.lm");
    let bytes = compile_to_bytes("checkpoint-now.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"snapshot data".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Vm", "Fs", "Io.Print"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done(\"live capture=false, closed 1, restored: recovered snapshot data\")",
        "child fault: {:?}",
        world.fault_of(1)
    );
    assert!(world.last_snapshot().is_some());
}

fn run_example(path: &str, allow: &[&str], file: Option<(&str, &[u8])>) -> String {
    let text = source(path);
    let bytes = compile_to_bytes(path, &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    if let Some((path, bytes)) = file {
        host.borrow_mut().set_file(path, bytes.to_vec());
    }
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in allow {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn a_driver_can_share_an_existing_file_handle() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/05-redirect.lm",
            &["Vm", "Fs"],
            Some(("message.txt", b"shared file")),
        ),
        "Done(\"shared file (the shared entry is closed)\")"
    );
}

#[test]
fn a_supervisor_records_every_open() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/03-audit.lm",
            &["Vm", "Fs"],
            Some(("message.txt", b"audited bytes")),
        ),
        "Done(\"1 open(s), first=message.txt, text=audited bytes\")"
    );
}

#[test]
fn a_supervisor_refuses_a_path_outside_its_allowlist() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/04-deny.lm",
            &["Vm", "Fs"],
            None,
        ),
        "Done(\"the child was refused: secret.txt is not permitted\")"
    );
}

/// The other refusal: the reply type carries no error value, so the
/// driver installs a fault instead of an error.
#[test]
fn a_supervisor_denies_a_request_with_no_error_reply() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/13-deny-with-a-fault.lm",
            &["Vm", "Io.Print"],
            None,
        ),
        "Done(\"the child stopped with PolicyDenied\")"
    );
}

#[test]
fn a_driver_observes_every_read_of_a_served_file() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/07-tee.lm",
            &["Vm", "Fs"],
            Some(("message.txt", b"hello from memory")),
        ),
        "Done(\"the child read 2 times and saw hello +from m\")"
    );
}

#[test]
fn a_successful_mock_close_retires_every_alias() {
    let text = r#"
case sys.fs.open("message.txt", ReadOnly)
in Ok(parent_file)
  child = sys.vm.Vm().activate_or_fault(do |file: FileHandle|: Bool with Fs.Close
    file.close().is_ok()
  end, args: (parent_file,))
  child.table().mock(Fs.Close, do |_: FileHandle|: Result[(), FsError]
    Ok(())
  end)
  child_closed = case child.run()
  in Done(value) then value
  in Fault(_)    then false
  end
  (child_closed, parent_file.read(1).is_err())
in Err(_) then (false, false)
end
"#;
    let bytes = compile_to_bytes("mock-close.lm", text).expect("the test compiles");
    let loaded = load_bytes(&bytes).expect("the test loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("message.txt", b"data".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    world.allow("Vm").expect("the VM grant exists");
    world.allow("Fs").expect("the filesystem grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done((true, true))");
    assert_eq!(world.resource_count(0), 0);
}

#[test]
fn a_driver_can_serve_a_file_handle() {
    assert_eq!(
        run_example(
            "examples/09-handles-and-supervision/06-serve-memory.lm",
            &["Vm"],
            None,
        ),
        "Done(\"settings from the supervisor\")"
    );
}

#[test]
fn a_supervisor_steps_the_child_to_a_quiet_capture_point() {
    let out = run_example(
        "examples/09-handles-and-supervision/09-checkpoint-when-quiet.lm",
        &["Vm", "Fs"],
        Some(("message.txt", b"quiet point data")),
    );
    // The step count depends on the lowering, so this reads the prefix.
    assert!(
        out.starts_with("Done(\"live capture=false, then captured after "),
        "unexpected outcome: {out}"
    );
}

#[test]
fn driver_termination_closes_its_served_files() {
    let text = r#"
child = sys.vm.Vm().activate_or_fault(do ||: Int with Fs.Open
  case sys.fs.open("memory.txt", ReadOnly)
  in Ok(_)  then 1
  in Err(_) then 0
  end
end, args: ())
case child.drive()
in Asked(request)
  case request
  in Call(Fs.Open, call, (_, _))
    control = child.serve_file(call)
    control.is_open()
  in _ then false
  end
in Done(_)  then false
in Fault(_) then false
end
"#;
    let bytes = compile_to_bytes("driver-exit.lm", text).expect("the test compiles");
    let loaded = load_bytes(&bytes).expect("the test loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the VM grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(true)");
    assert_eq!(world.resource_count(1), 0);
    let barrier = world.next_gate();
    assert!(world.capture_snapshot(barrier, 1, false).is_ok());
}

#[test]
fn snapshot_wait_advances_reachable_background_work() {
    let text = source("examples/09-handles-and-supervision/10-checkpoint-background-work.lm");
    let bytes =
        compile_to_bytes("checkpoint-background-work.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"transient".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Vm", "Fs", "Proc"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done(\"captured after the file closed; indexed 9 bytes\")",
        "root fault: {:?}; child fault: {:?}; worker fault: {:?}",
        world.fault_of(0),
        world.fault_of(1),
        world.fault_of(2)
    );
    assert!(world.last_snapshot().is_some());
}

#[test]
fn snapshot_wait_keeps_a_possible_mailbox_wake_live() {
    let text = r#"
enum Command
  Release
end

class Worker < Proc[Command]
  file: FileHandle

  def init(mut self, file: FileHandle)
    self.file = file
  end

  def on_spawn(self): Int with Proc, Fs.Close
    case self.receive()
    in Msg(Release)
      case self.file.close()
      in Ok(_)  then 1
      in Err(_) then 0 - 1
      end
    in Closed then 0 - 2
    end
  end
end

class Gate < Proc
  def on_spawn(self): Int
    turns = 0
    while turns < 3000
      turns = turns + 1
    end
    turns
  end
end

class Closer < Proc
  worker: Handle[Command, Int]

  def init(mut self, worker: Handle[Command, Int])
    self.worker = worker
  end

  def on_spawn(self): Bool with Proc
    self.worker.send(Release).is_sent()
  end
end

case sys.fs.open("message.txt", ReadOnly)
in Ok(file)
  worker = Worker.spawn(file)
  case worker.pause()
  in Ok(vm)
    vm.table().pass(Fs.Close)
    worker.resume()
    ()
  in Err(_) then ()
  end

  # Let the worker block on its mailbox while it holds the file.
  gate = Gate.spawn()
  gate.done()

  # The closer is runnable when snapshot_wait checks the blocked worker.
  Closer.spawn(worker)
  case worker.snapshot_wait(10000)
  in Ok(_)  then "captured after the mailbox wake"
  in Err(_) then "capture failed"
  end
in Err(_) then "open failed"
end
"#;
    let bytes = compile_to_bytes("mailbox-snapshot-wait.lm", text).expect("the test compiles");
    let loaded = load_bytes(&bytes).expect("the test loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("message.txt", b"data".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Vm", "Fs", "Proc"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done(\"captured after the mailbox wake\")"
    );
    assert!(world.last_snapshot().is_some());
}

#[test]
fn a_closed_resource_control_survives_restore() {
    let text = source("examples/09-handles-and-supervision/11-closed-control-snapshot.lm");
    let bytes =
        compile_to_bytes("closed-control-snapshot.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the VM grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"cached settings\")");
    let image = world
        .last_snapshot()
        .expect("the program captured a snapshot")
        .clone();
    assert_eq!(resource_controls(&image), vec![(1, 0)]);

    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the restore target exists");
    let restored = fresh
        .restore_image(0, target, &image)
        .expect("the control restores");
    let barrier = fresh.next_gate();
    let recaptured = fresh
        .capture_snapshot(barrier, restored, false)
        .expect("the restored control captures");
    assert_eq!(resource_controls(&recaptured), vec![(1, 0)]);
}

fn resource_controls(image: &lm_vm::snapshot::SnapshotImage) -> Vec<(u32, u64)> {
    image
        .world()
        .machines
        .iter()
        .flat_map(|machine| &machine.objects)
        .filter_map(|entry| match &entry.object {
            lm_heap::Object::NativeResourceHandle { surface, resource } => {
                Some((*surface, *resource))
            }
            _ => None,
        })
        .collect()
}
