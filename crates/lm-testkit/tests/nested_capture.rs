//! A snapshot of a world that holds a nested activation stack.
//!
//! Specification 17.4 names two conditions that block a copy, and a
//! stored activation stack is neither. The machines of a stored stack
//! execute nothing, and their nested control edges stay in the machine
//! records, so a restore rebuilds the chain.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, TaskKey, VmConfig, World};

const SRC: &str = r#"
w = sys.vm.Vm().activate_or_fault(do ||: Int
  i = 0
  while i < 100000
    i = i + 1
  end
  i
end, args: ())
h = sys.proc.run(w)
c = sys.vm.Vm().activate_or_fault(do |hh: Handle[Never, Int]|: Int with Proc
  case hh.done()
  in Done(v)  then v
  in Fault(_) then 0 - 1
  end
end, args: (h,))
c.table().pass(Proc)
case c.run()
in Done(v)  then v
in Fault(_) then 0 - 2
end
"#;

#[test]
fn a_nested_suspended_stack_captures_and_restores() {
    let bytes = compile_to_bytes("nested.lm", SRC).expect("the probe compiles");
    let loaded = load_bytes(&bytes).expect("the probe loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for g in ["Vm", "Proc"] {
        world.allow(g).expect("the grant exists");
    }

    // Drive the root until its stack holds a nested activation.
    let root = TaskKey {
        vm: 0,
        generation: 0,
    };
    let mut turns = 0;
    while world.suspended_len(0) < 2 && turns < 200 {
        world.drive_slice(root, 64);
        turns += 1;
    }
    println!(
        "after {turns} turns: suspended_len(0)={} ",
        world.suspended_len(0)
    );
    assert!(
        world.suspended_len(0) >= 2,
        "the probe did not reach a nested suspended stack"
    );

    let barrier = world.next_gate();
    let image = match world.capture_snapshot(barrier, 0, false) {
        Ok(image) => {
            println!(
                "CAPTURED: {} bytes",
                image.bytes().expect("the image encodes").len()
            );
            image
        }
        Err(fail) => panic!("REFUSED: {fail:?}"),
    };

    // The original world must still finish after the capture resumed
    // it. The nested child answers the worker result.
    let outcome = lm_proc::run_world(&mut world);
    println!("ORIGINAL: {}", world.show_outcome(&outcome));
    assert_eq!(world.show_outcome(&outcome), "Done(100000)");

    // The restored world must rebuild the nested control chain from
    // the stored edges and reach the same result.
    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the restore target exists");
    let restored = fresh
        .restore_image(0, target, &image)
        .expect("the image restores");
    for vm in 0..fresh.machine_count() as u32 {
        let _ = fresh.allow_on(vm, "Vm");
        let _ = fresh.allow_on(vm, "Proc");
    }
    let mut turns = 0;
    loop {
        match fresh.run_machine(restored) {
            lm_vm::RootEvent::Done(_) | lm_vm::RootEvent::Fault(_) => break,
            lm_vm::RootEvent::Blocked => {}
            other => panic!("unexpected restored event {other:?}"),
        }
        if lm_proc::drain_procs(&mut fresh) == 0 {
            break;
        }
        turns += 1;
        assert!(turns < 5000, "the restored world did not settle");
    }
    let key = fresh.task_key(restored).expect("the restored root exists");
    let outcome = fresh.task_outcome(key);
    println!(
        "RESTORED: {} after {turns} turns",
        fresh.show_outcome(&outcome)
    );
    assert_eq!(fresh.show_outcome(&outcome), "Done(100000)");
}
