//! The guest snapshot value names an admitted image of this world.
//!
//! An in-process capture writes no container and hashes nothing, and
//! a restore of that value reads the admitted world back. A container
//! appears only when a caller asks for the bytes, and it stays the
//! canonical container of specification 17.9.

use lm_heap::Object;
use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, LoadedModule, RecordingHost, VmConfig, World};

fn program(source: &str) -> LoadedModule {
    let bytes = compile_to_bytes("images.lm", source).expect("the program compiles");
    load_bytes(&bytes).expect("the program loads")
}

/// A capture holds its admitted world and writes no container.
///
/// The bytes appear at the first call that asks for them, and the
/// container the call writes is the canonical container.
#[test]
fn a_capture_writes_its_container_only_when_a_caller_asks() {
    let loaded = program("1 + 1\n");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.step_root();
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the capture succeeds");
    // No container yet, so the image charges the admitted world alone.
    let world_only = image.resident_bytes();
    let bytes = image.bytes().expect("the container writes").clone();
    assert!(!bytes.is_empty());
    assert!(image.resident_bytes() > world_only);
    // The second call returns the same container.
    assert_eq!(
        image.bytes().expect("the container writes").as_ref(),
        bytes.as_ref()
    );
    // The hash follows the bytes.
    assert_eq!(
        image.hash().expect("the container writes"),
        lm_vm::snapshot::codec::container_hash(&bytes[..bytes.len() - 32])
    );
}

/// The guest value is a handle, not a container.
#[test]
fn a_guest_snapshot_value_names_an_admitted_image() {
    let loaded = program(
        "vm = sys.vm.Vm().activate(do ||: Int
  20 + 22
end, args: ())
vm.step()
vm.snapshot()
",
    );
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    let outcome = lm_proc::run_world(&mut world);
    let value = match outcome {
        lm_vm::Outcome::Done(value) => value,
        other => panic!("expected a value, got {other:?}"),
    };
    // The result is `Ok(snap)`, so the handle sits one field in.
    let root = value.as_obj().expect("the result is an object");
    let Object::Instance { fields, .. } = world.heap_of(0).get(root) else {
        panic!("expected the result instance");
    };
    let held = fields[0].as_obj().expect("the arm holds one field");
    assert!(
        matches!(world.heap_of(0).get(held), Object::NativeSnapshotRef { .. }),
        "the guest value names an admitted image"
    );
}

/// A capture of a world that holds a snapshot states the nested
/// container, because that world can leave this process.
#[test]
fn a_captured_world_states_the_container_of_a_nested_image() {
    let loaded = program(
        "vm = sys.vm.Vm().activate(do ||: Int
  20 + 22
end, args: ())
vm.step()
vm.snapshot()
",
    );
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    // Machine 0 finished and its terminal value holds the handle. The
    // capture of machine 0 must state the nested container instead.
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the capture succeeds");
    let nested: Vec<&Object> = image.world().machines[0]
        .objects
        .iter()
        .map(|entry| &entry.object)
        .filter(|object| {
            matches!(
                object,
                Object::NativeSnapshot(_) | Object::NativeSnapshotRef { .. }
            )
        })
        .collect();
    assert_eq!(nested.len(), 1, "the capture holds one nested image");
    match nested[0] {
        Object::NativeSnapshot(bytes) => assert!(!bytes.is_empty()),
        other => panic!("a captured world states a container, got {other:?}"),
    }
}
