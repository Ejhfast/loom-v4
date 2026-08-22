//! Command boundary tests for the standard ABI bundle.

use lm_testkit::compile_to_bytes;
use lm_vm::snapshot::SnapshotFail;
use lm_vm::{MachineState, RecordingHost, RootEvent, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

const SOURCE: &str = r#"
use std.io.write_all

def go(): (Int, String, String, Int, Int, Int) with Io.ReadBytes, Io.Write, Env.Get, Fs.CurrentDir, Entropy.Bytes, Args
  input = sys.io.read_bytes(8).expect("input works")
  name = case sys.env.get("LOOM_NAME").expect("environment works")
  in Some(value) then value
  in None then "missing"
  end
  directory = sys.fs.current_dir().expect("directory works")
  entropy = sys.entropy.bytes(12).expect("entropy works")
  arguments = sys.args()
  arguments.push("local")
  original = sys.args()
  write_all(input).expect("output works")
  (input.len(), name, directory, entropy.len(), arguments.len(), original.len())
end
go()
"#;

#[test]
fn standard_command_operations_cross_one_checked_boundary() {
    let bytes = compile_to_bytes("command.lm", SOURCE).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().input_bytes = vec![0xff, 0, 0xfe];
    host.borrow_mut().set_env("LOOM_NAME", "loom");
    host.borrow_mut().arguments = vec!["first".to_string(), "second".to_string()];
    host.borrow_mut().current_dir = "/work".to_string();
    host.borrow_mut().console_write_limit = 2;
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host.clone()));
    for grant in [
        "Io.ReadBytes",
        "Io.Write",
        "Env.Get",
        "Fs.CurrentDir",
        "Entropy.Bytes",
        "Args",
    ] {
        world.allow(grant).expect("the operation has a grant");
    }
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((3, \"loom\", \"/work\", 12, 3, 2))"
    );
    assert_eq!(host.borrow().written_bytes, [0xff, 0, 0xfe]);
}

#[test]
fn standard_io_buffers_lines_and_preserves_remaining_input() {
    let source = r#"
use std.io.ConsoleLineReader

def go(): (Option[String], Option[String], Option[String], Option[String]) with Io.ReadBytes
  reader = ConsoleLineReader()
  first = reader.read_line(8).expect("the first line reads")
  second = reader.read_line(8).expect("the second line reads")
  third = reader.read_line(8).expect("the final line reads")
  fourth = reader.read_line(8).expect("the input ends")
  (first, second, third, fourth)
end
go()
"#;
    let bytes = compile_to_bytes("lines.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().input_bytes = b"one\r\ntwo\nlast".to_vec();
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    world
        .allow("Io.ReadBytes")
        .expect("the operation has a grant");
    let outcome = lm_proc::run_world(&mut world);
    let context = world
        .root_fault()
        .map(|fault| world.fault_context(fault))
        .unwrap_or_default();
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((Some(\"one\"), Some(\"two\"), Some(\"last\"), None))",
        "{context:?}"
    );
}

#[test]
fn standard_io_reports_line_and_total_input_limits() {
    let source = r#"
use std.io.ConsoleLineReader
use std.io.read_to_end

def go(): (String, String) with Io.ReadBytes
  line_error = case ConsoleLineReader().read_line(3)
  in Err(error) then display(error)
  in Ok(_) then "missing line error"
  end
  total_error = case read_to_end(2)
  in Err(error) then display(error)
  in Ok(_) then "missing total error"
  end
  (line_error, total_error)
end
go()
"#;
    let bytes = compile_to_bytes("limits.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().input_bytes = b"long\nmore".to_vec();
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    world
        .allow("Io.ReadBytes")
        .expect("the operation has a grant");
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(
        world.show_outcome(&outcome),
        "Done((\"the input line exceeds its byte limit\", \"the input exceeds its byte limit\"))"
    );
}

#[test]
fn generic_stream_helpers_use_the_writer_effect_argument() {
    let source = r#"
use std.io.write_all_to

final class PartialSink implements ByteWriter
  type Error = String

  def write(self, bytes: Bytes): Result[Int, String]
    if bytes.len() > 2
      Ok(2)
    else
      Ok(bytes.len())
    end
  end
end

write_all_to(PartialSink(), Bytes("abcdef")).expect("the write completes")
"#;
    let bytes = compile_to_bytes("writer.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(())");
}

#[test]
fn command_operations_still_need_policy_grants() {
    let source = "def go(): Int with Entropy.Bytes\n  sys.entropy.bytes(1).expect(\"entropy works\").len()\nend\ngo()\n";
    let bytes = compile_to_bytes("denied.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(PolicyDenied)");
}

#[test]
fn args_needs_a_declared_row_and_policy_grant() {
    let missing_row = "def go(): Int\n  sys.args().len()\nend\ngo()\n";
    let error = compile_to_bytes("args-row.lm", missing_row)
        .expect_err("the operation needs its declared row");
    assert!(error.contains("Args.Get"), "{error}");

    let source = "def go(): Int with Args\n  sys.args().len()\nend\ngo()\n";
    let bytes = compile_to_bytes("args-policy.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Fault(PolicyDenied)");
}

#[test]
fn a_pending_byte_read_blocks_snapshot_creation() {
    let source = "def go(): Int with Io.ReadBytes\n  sys.io.read_bytes(1).expect(\"input works\").len()\nend\ngo()\n";
    let bytes = compile_to_bytes("pending-input.lm", source).expect("the source compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the artifact verifies");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world
        .allow("Io.ReadBytes")
        .expect("the operation has a grant");
    while world.state_of(0) != MachineState::Waiting {
        match world.step_root() {
            RootEvent::Ran | RootEvent::Waiting => {}
            other => panic!("expected the input request, found {other:?}"),
        }
    }
    let gate = world.next_gate();
    let error = world
        .capture_snapshot(gate, 0, false)
        .expect_err("the pending input blocks the snapshot");
    match error {
        SnapshotFail::ResourceActive { kind, .. } => {
            assert_eq!(kind, "a pending Io.ReadBytes");
        }
        other => panic!("expected a resource error, found {other:?}"),
    }
}
