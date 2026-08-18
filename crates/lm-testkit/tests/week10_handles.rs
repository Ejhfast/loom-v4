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
    let text = source("examples/09-host/read-file.lm");
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
    let text = source("examples/09-host/round-trip-file.lm");
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
    let text = source("examples/09-host/managed-snapshot.lm");
    let bytes = compile_to_bytes("managed-snapshot.lm", &text).expect("the example compiles");
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
        "Done(\"reopened snapshot data\")",
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
            "examples/09-host/wrapped-handle.lm",
            &["Vm", "Fs"],
            Some(("message.txt", b"shared file")),
        ),
        "Done(\"shared file\")"
    );
}

#[test]
fn a_successful_mock_close_retires_every_alias() {
    let text = r#"
case sys.fs.open("message.txt", ReadOnly)
in Ok(parent_file)
  child = sys.vm.Vm().from_object(do |file: FileHandle|: Bool with Fs.Close
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
fn a_driver_can_mint_a_file_handle() {
    assert_eq!(
        run_example("examples/09-host/minted-handle.lm", &["Vm"], None),
        "Done(\"parent memory\")"
    );
}

#[test]
fn driver_termination_closes_its_minted_files() {
    let text = r#"
child = sys.vm.Vm().from_object(do ||: Int with Fs.Open
  case sys.fs.open("memory.txt", ReadOnly)
  in Ok(_)  then 1
  in Err(_) then 0
  end
end, args: ())
case child.drive()
in Asked(request)
  case request.as_call(Fs.Open)
  in Some(call)
    control = child.mint_file(call)
    control.is_open()
  in None then false
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
    let text = source("examples/09-host/wait-snapshot.lm");
    let bytes = compile_to_bytes("wait-snapshot.lm", &text).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"transient".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Vm", "Fs", "Proc", "Io.Print"] {
        world.allow(grant).expect("the grant exists");
    }

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done(\"indexed 9 bytes\")",
        "root fault: {:?}; child fault: {:?}; worker fault: {:?}",
        world.fault_of(0),
        world.fault_of(1),
        world.fault_of(2)
    );
    assert!(world.last_snapshot().is_some());
}

#[test]
fn a_closed_resource_control_survives_restore() {
    let text = source("examples/09-host/closed-control-snapshot.lm");
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
