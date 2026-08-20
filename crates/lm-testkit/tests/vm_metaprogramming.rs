//! Reified compiler and VM integration.

use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_source::SourceFile;
use lm_testkit::compile_to_bytes;
use lm_vm::snapshot::{codec, LoadLimits, SnapshotFail};
use lm_vm::{load_bytes, RecordingHost, RootEvent, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn run_with_files(source: &str, files: &[(&str, Vec<u8>)]) -> String {
    let bytes = compile_to_bytes("meta.lm", source).expect("the test program compiles");
    let loaded = load_bytes(&bytes).expect("the test program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    for (name, bytes) in files {
        host.borrow_mut().set_file(*name, bytes.clone());
    }
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn loom_verifies_installs_and_activates_an_artifact() {
    let artifact = compile_to_bytes("installed.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("installed.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm
  artifact = sys.vm.artifact(artifact_bytes())
  case artifact.verify()
  in Err(_) then 0 - 1
  in Ok(module)
    image = sys.vm.Vm()
    case image.install(module)
    in Err(_) then 0 - 2
    in Ok(instance)
      case instance.entry[(), Int]()
      in Err(_) then 0 - 3
      in Ok(entry)
        case image.activate(entry, args: ()).run()
        in Done(value) then value
        in Fault(_) then 0 - 4
        end
      end
    end
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("installed.lmbc", artifact)]),
        "Done(42)"
    );
}

fn revision_artifact(body: &str) -> Vec<u8> {
    let source = format!("def step(value: Int): Int\n  {body}\nend\nstep(1)\n");
    let compiled = compile_module_with_options(
        "revision",
        &SourceFile::new("revision.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().late_function("step"),
    )
    .expect("the revision compiles");
    lm_bytecode::encode(&compiled.module)
}

#[test]
fn a_slot_replacement_changes_later_calls_only() {
    let first = revision_artifact("value + 1");
    let second = revision_artifact("value + 10");
    let source = r#"
def read_artifact(path: String): Artifact with Fs.Open, Fs.Read, Fs.Close, Vm
  bytes = case sys.fs.open(path, ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(data) then data
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
  sys.vm.artifact(bytes)
end

def execute(): (Int, Int) with Fs.Open, Fs.Read, Fs.Close, Vm
  image = sys.vm.Vm()
  first_module = case read_artifact("first.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (0 - 1, 0 - 1)
  end
  second_module = case read_artifact("second.lmbc").verify()
  in Ok(module) then module
  in Err(_)
    return (0 - 2, 0 - 2)
  end
  first = case image.install(first_module)
  in Ok(instance) then instance
  in Err(_)
    return (0 - 3, 0 - 3)
  end
  second = case image.install(second_module)
  in Ok(instance) then instance
  in Err(_)
    return (0 - 4, 0 - 4)
  end
  entry = case first.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return (0 - 5, 0 - 5)
  end
  before = case image.activate(entry, args: ()).run()
  in Done(value) then value
  in Fault(_)
    return (0 - 6, 0 - 6)
  end
  slot = case first.slot(0)
  in Ok(value) then value
  in Err(_)
    return (0 - 7, 0 - 7)
  end
  target = case second.function[(Int,), Int]("step")
  in Ok(value) then value
  in Err(_)
    return (0 - 8, 0 - 8)
  end
  case image.replace(slot, target)
  in Err(_)
    return (0 - 9, 0 - 9)
  in Ok(_)
    after = case image.activate(entry, args: ()).run()
    in Done(value) then value
    in Fault(_) then 0 - 10
    end
    (before, after)
  end
end

execute()
"#;
    assert_eq!(
        run_with_files(source, &[("first.lmbc", first), ("second.lmbc", second)]),
        "Done((2, 11))"
    );
}

#[test]
fn installed_code_and_handles_survive_an_external_snapshot() {
    let artifact = compile_to_bytes("installed.lm", "42\n").expect("the artifact compiles");
    let source = r#"
def artifact_bytes(): Bytes with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("installed.lmbc", ReadOnly)
  in Ok(file)
    value = case file.read(1048576)
    in Ok(bytes) then bytes
    in Err(_) then Bytes()
    end
    file.close()
    value
  in Err(_) then Bytes()
  end
end

def execute(): Int with Fs.Open, Fs.Read, Fs.Close, Vm
  image = sys.vm.Vm()
  module = case sys.vm.artifact(artifact_bytes()).verify()
  in Ok(value) then value
  in Err(_)
    return 0 - 1
  end
  instance = case image.install(module)
  in Ok(value) then value
  in Err(_)
    return 0 - 2
  end
  entry = case instance.entry[(), Int]()
  in Ok(value) then value
  in Err(_)
    return 0 - 3
  end
  case image.activate(entry, args: ()).run()
  in Done(value) then value
  in Fault(_) then 0 - 4
  end
end

execute()
"#;
    let bytes = compile_to_bytes("snapshot-code.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("installed.lmbc", artifact.clone());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Fs", "Vm"] {
        world.allow(grant).expect("the grant exists");
    }

    let mut captured = None;
    for _ in 0..2000 {
        match world.step_root() {
            RootEvent::Ran => {}
            RootEvent::Waiting | RootEvent::Blocked => {
                world.poll_blocked();
                continue;
            }
            event => panic!("the source stopped before installation: {event:?}"),
        }
        let gate = world.next_gate();
        match world.capture_snapshot(gate, 0, false) {
            Ok(image) if !image.world().installations.is_empty() => {
                captured = Some(image);
                break;
            }
            Ok(_) | Err(SnapshotFail::ResourceActive { .. }) => {}
            Err(error) => panic!("the snapshot failed: {error:?}"),
        }
    }
    let captured = captured.expect("a boundary follows installation");
    assert_eq!(captured.world().installations.len(), 1);
    assert_eq!(captured.world().vm_images.len(), 1);
    assert_eq!(captured.world().vm_images[0].instances.len(), 1);

    let admitted = codec::load_external(
        captured.bytes().expect("the snapshot encodes"),
        &loaded,
        LoadLimits::default(),
    )
    .expect("the external snapshot admits");
    let mut restored = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    restored.allow("Vm").expect("the grant exists");
    let target = restored.new_child(0).expect("the restore target exists");
    let root = restored
        .restore_image(0, target, &admitted)
        .expect("the code image restores");
    restored.allow_on(root, "Vm").expect("the grant exists");
    loop {
        match restored.run_machine(root) {
            RootEvent::Done(value) => {
                assert_eq!(restored.show_result_of(root, value), "42");
                break;
            }
            RootEvent::Ran => {}
            event => panic!("the restored run stopped: {event:?}"),
        }
    }
}
