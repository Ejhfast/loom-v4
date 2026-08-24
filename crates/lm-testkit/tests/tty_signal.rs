//! Terminal and process signal effects.

use lm_testkit::{compile_text, compile_to_bytes, repo_root};
use lm_vm::{HostSignalKind, HostStdStream, Object, RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

fn world(source: &str) -> (World, Rc<RefCell<RecordingHost>>) {
    let bytes = compile_to_bytes("tty-signal.lm", source).expect("the program compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let world = World::new(&loaded, VmConfig::default(), Box::new(Rc::clone(&host)));
    (world, host)
}

fn allow(world: &mut World, names: &[&str]) {
    for name in names {
        world.allow(name).expect("the grant exists");
    }
}

#[test]
fn terminal_operations_use_typed_values_and_one_raw_resource() {
    let source = r##"
def go(): (Bool, Int, Int, String, String) with Tty
  tty = Tty()
  size = tty.size(StdStream.Output).expect("the terminal has a size")
  raw = tty.enter_raw().expect("raw mode opens")
  busy = case tty.enter_raw()
  in Err(Busy) then "busy"
  in Err(error) then display(error)
  in Ok(_) then "opened twice"
  end
  raw.exit().expect("raw mode closes")
  closed = case raw.exit()
  in Err(Closed) then "closed"
  in Err(error) then display(error)
  in Ok(_) then "closed twice"
  end
  (tty.is_terminal(StdStream.Input), size.columns, size.rows, busy, closed)
end
go()
"##;
    let (mut world, host) = world(source);
    host.borrow_mut().set_terminal_size(132, 43);
    allow(&mut world, &["Tty"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done((true, 132, 43, \"busy\", \"closed\"))"
    );
    assert!(!host.borrow().raw_mode_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn a_nonterminal_stream_returns_a_typed_error() {
    let source = r#"
case Tty().size(StdStream.Error)
in Err(NotTerminal) then "not a terminal"
in Err(error) then display(error)
in Ok(_) then "unexpected size"
end
"#;
    let (mut world, host) = world(source);
    host.borrow_mut().set_terminal(HostStdStream::Error, false);
    allow(&mut world, &["Tty"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"not a terminal\")");
}

#[test]
fn normal_completion_restores_raw_terminal_mode() {
    let source = r#"
def go(): Int with Tty
  raw = Tty().enter_raw().expect("raw mode opens")
  7
end
go()
"#;
    let (mut world, host) = world(source);
    allow(&mut world, &["Tty"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(7)");
    assert!(!host.borrow().raw_mode_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn a_machine_fault_restores_raw_terminal_mode() {
    let source = r#"
def go(): Never with Tty
  raw = Tty().enter_raw().expect("raw mode opens")
  panic("stop")
end
go()
"#;
    let (mut world, host) = world(source);
    allow(&mut world, &["Tty"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Fault(UserPanic)");
    assert!(!host.borrow().raw_mode_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn raw_terminal_mode_blocks_snapshot_capture() {
    let source = r#"
def go(): String with Tty, Vm
  raw = Tty().enter_raw().expect("raw mode opens")
  blocked = case sys.vm.snapshot_self()
  in Err(ResourceActive(_, kind)) then kind
  in Err(error) then display(error)
  in Ok(_) then "snapshot succeeded"
  end
  raw.exit().expect("raw mode closes")
  blocked
end
go()
"#;
    let (mut world, host) = world(source);
    allow(&mut world, &["Tty", "Vm"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"raw terminal mode\")");
    assert!(!host.borrow().raw_mode_active());
}

#[test]
fn a_losing_signal_wait_preserves_its_signal() {
    let source = r#"
def go(): (String, String, String) with Signal, Clock, Wait, Vm
  stream = Signal().open([SignalKind.Interrupt]).expect("the stream opens")
  blocked = case sys.vm.snapshot_self()
  in Err(ResourceActive(_, kind)) then kind
  in Err(error) then display(error)
  in Ok(_) then "snapshot succeeded"
  end
  selected = select
  in sys.clock.sleep.wait(0) -> _
    "timer"
  in stream.next_wait() -> _
    "signal"
  end
  kind = case stream.next().expect("the signal remains")
  in SignalKind.Interrupt then "interrupt"
  in SignalKind.Terminate then "terminate"
  end
  stream.close().expect("the stream closes")
  (blocked, selected, kind)
end
go()
"#;
    let (mut world, host) = world(source);
    host.borrow_mut()
        .queue_signal_on_open(HostSignalKind::Interrupt);
    allow(&mut world, &["Signal", "Clock", "Wait", "Vm"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done((\"signal stream\", \"timer\", \"interrupt\"))"
    );
    assert!(!host.borrow().signal_stream_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn a_machine_fault_closes_its_signal_stream() {
    let source = r#"
def go(): Never with Signal
  stream = Signal().open([SignalKind.Terminate]).expect("the stream opens")
  panic("stop")
end
go()
"#;
    let (mut world, host) = world(source);
    allow(&mut world, &["Signal"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Fault(UserPanic)");
    assert!(!host.borrow().signal_stream_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn signal_stream_limits_and_closed_aliases_return_typed_errors() {
    let source = r#"
def go(): (String, String, String) with Signal
  invalid = case Signal().open([])
  in Err(InvalidInput(_)) then "invalid"
  in Err(error) then display(error)
  in Ok(_) then "opened empty"
  end
  stream = Signal().open([SignalKind.Terminate, SignalKind.Terminate]).expect("open")
  busy = case Signal().open([SignalKind.Interrupt])
  in Err(Busy) then "busy"
  in Err(error) then display(error)
  in Ok(_) then "opened twice"
  end
  stream.close().expect("close")
  closed = case stream.close()
  in Err(Closed) then "closed"
  in Err(error) then display(error)
  in Ok(_) then "closed twice"
  end
  (invalid, busy, closed)
end
go()
"#;
    let (mut world, host) = world(source);
    allow(&mut world, &["Signal"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done((\"invalid\", \"busy\", \"closed\"))"
    );
    assert!(!host.borrow().signal_stream_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn a_child_vm_uses_passed_terminal_and_signal_authority() {
    let source = r#"
def child(): String with Tty, Signal
  raw = Tty().enter_raw().expect("raw mode opens")
  stream = Signal().open([SignalKind.Terminate]).expect("the stream opens")
  kind = stream.next().expect("the signal arrives")
  stream.close().expect("the stream closes")
  raw.exit().expect("raw mode closes")
  case kind
  in SignalKind.Interrupt then "interrupt"
  in SignalKind.Terminate then "terminate"
  end
end

def go(): String with Vm, Tty, Signal
  run = sys.vm.Vm().activate_or_fault(child, args: ())
  run.table().pass(Tty)
  run.table().pass(Signal)
  case run.run()
  in Ok(value) then value
  in Err(fault) then fault.code()
  end
end
go()
"#;
    let (mut world, host) = world(source);
    host.borrow_mut()
        .queue_signal_on_open(HostSignalKind::Terminate);
    allow(&mut world, &["Vm", "Tty", "Signal"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"terminate\")");
    assert!(!host.borrow().raw_mode_active());
    assert!(!host.borrow().signal_stream_active());
    assert_eq!(world.world_resource_count(), 0);
}

#[test]
fn terminal_helpers_encode_controls_and_decode_bounded_keys() {
    let source = r##"
use std.term.clear_screen
use std.term.cursor_to
use std.term.decode_key
use std.term.KeyDecode
use std.term.TermKey

arrow = case decode_key(b"\x1b[A", false)
in Decoded(TermKey.Up, count) then count
in Decoded(_, _) then -1
in NeedMore then -2
end
text = case decode_key(b"\xc3\xa9", false)
in Decoded(TermKey.Text(value), count) then "#{value}:#{count}"
in Decoded(_, _) then "wrong"
in NeedMore then "missing"
end
escape = case decode_key(b"\x1b", true)
in Decoded(TermKey.Escape, count) then count
in Decoded(_, _) then -1
in NeedMore then -2
end
(
  clear_screen().hex(),
  cursor_to(4, 2).expect("the cursor position is valid").hex(),
  arrow,
  text,
  escape
)
"##;
    let (mut world, _) = world(source);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(
        world.show_outcome(&outcome),
        "Done((\"1b5b324a\", \"1b5b323b3448\", 3, \"é:2\", 1))"
    );
}

#[test]
fn the_checker_rejects_wrong_terminal_and_signal_arguments() {
    let terminal =
        compile_to_bytes("bad-tty.lm", "Tty().size(1)\n").expect_err("an integer stream rejects");
    assert!(
        terminal.contains("expected StdStream, found Int"),
        "{terminal}"
    );

    let signal = compile_to_bytes("bad-signal.lm", "Signal().open([1])\n")
        .expect_err("an integer signal rejects");
    assert!(
        signal.contains("expected SignalKind, found Int"),
        "{signal}"
    );
}

#[test]
fn terminal_and_signal_operations_need_rows_and_policy_grants() {
    let missing_tty = "def go(): Bool\n  Tty().is_terminal(StdStream.Input)\nend\ngo()\n";
    let error = compile_to_bytes("tty-row.lm", missing_tty).expect_err("the row is required");
    assert!(error.contains("Tty.IsTerminal"), "{error}");

    let missing_signal =
        "def go(): SignalStream\n  Signal().open([SignalKind.Interrupt]).expect(\"open\")\nend\ngo()\n";
    let error = compile_to_bytes("signal-row.lm", missing_signal).expect_err("the row is required");
    assert!(error.contains("Signal.Open"), "{error}");

    for source in [
        "def go(): Bool with Tty\n  Tty().is_terminal(StdStream.Input)\nend\ngo()\n",
        "def go(): Int with Signal\n  Signal().open([SignalKind.Interrupt])\n  1\nend\ngo()\n",
    ] {
        let (mut world, _) = world(source);
        let outcome = lm_proc::run_world(&mut world);
        assert_eq!(world.show_outcome(&outcome), "Fault(PolicyDenied)");
    }
}

#[test]
fn the_verifier_rejects_a_forged_terminal_size_role() {
    let mut module = compile_text("tty-role.lm", "1\n").expect("the program compiles");
    let role = lm_bytecode::corepin::role_index("TtySize").expect("the role exists");
    let class = module.core_roles[role];
    module.classes[class as usize].fields.pop();

    let error = lm_verify::verify_module(&module).expect_err("the forged role rejects");

    assert!(
        error
            .message
            .contains("the TtySize role does not name its frozen value class"),
        "{error}"
    );
}

#[test]
fn closed_terminal_and_signal_resources_admit_in_snapshots() {
    let source = r#"
def go(): String with Tty, Signal, Vm
  raw = Tty().enter_raw().expect("raw mode opens")
  stream = Signal().open([SignalKind.Terminate]).expect("the stream opens")
  raw.exit().expect("raw mode closes")
  stream.close().expect("the stream closes")
  case sys.vm.snapshot_self()
  in Ok(_) then "captured"
  in Err(error) then display(error)
  end
end
go()
"#;
    let bytes = compile_to_bytes("closed-tty-signal.lm", source).expect("the program compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    allow(&mut world, &["Tty", "Signal", "Vm"]);
    let outcome = lm_proc::run_world(&mut world);
    assert_eq!(world.show_outcome(&outcome), "Done(\"captured\")");

    let image = world.last_snapshot().expect("the snapshot exists").clone();
    assert!(image.world().machines.iter().any(|machine| {
        machine
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeRawMode { resource: 0 }))
    }));
    assert!(image.world().machines.iter().any(|machine| {
        machine
            .objects
            .iter()
            .any(|entry| matches!(entry.object, Object::NativeSignalStream { resource: 0 }))
    }));

    let mut fresh = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = fresh.new_child(0).expect("the restore target exists");
    let restored = fresh
        .restore_image(0, target, &image)
        .expect("the snapshot restores");
    let barrier = fresh.next_gate();
    fresh
        .capture_snapshot(barrier, restored, false)
        .expect("the restored image captures");
}

#[test]
fn the_terminal_event_loop_example_selects_one_key() {
    let path = repo_root().join("examples/04-effects/terminal-event-loop.lm");
    let source = std::fs::read_to_string(path).expect("the example reads");
    let (mut world, host) = world(&source);
    host.borrow_mut().input_bytes = b"\x1b[A".to_vec();
    host.borrow_mut().set_terminal_size(132, 43);
    allow(&mut world, &["Tty", "Io", "Clock", "Wait"]);

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"132x43 up\")");
    assert_eq!(host.borrow().written_bytes, b"\x1b[2J\x1b[1;1H");
    assert!(!host.borrow().raw_mode_active());
}

#[test]
fn the_termination_example_writes_an_admitted_snapshot() {
    let path =
        repo_root().join("examples/09-handles-and-supervision/14-checkpoint-on-termination.lm");
    let source = std::fs::read_to_string(path).expect("the example reads");
    let bytes =
        compile_to_bytes("checkpoint-on-termination.lm", &source).expect("the example compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .queue_signal_on_open(HostSignalKind::Terminate);
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(Rc::clone(&host)));
    allow(&mut world, &["Signal", "Wait", "Vm", "Fs"]);

    let outcome = lm_proc::run_world(&mut world);
    let snapshot = host
        .borrow()
        .file("shutdown.lms")
        .expect("the snapshot file exists")
        .to_vec();

    assert_eq!(
        world.show_outcome(&outcome),
        format!("Done(\"saved {} bytes after termination\")", snapshot.len())
    );
    lm_vm::snapshot::codec::load_external(
        &snapshot,
        &loaded,
        lm_vm::snapshot::LoadLimits::default(),
    )
    .expect("the written snapshot admits");
    let recorded = host.borrow();
    let sync = recorded
        .operations
        .iter()
        .position(|operation| *operation == lm_abi::OP_FS_SYNC)
        .expect("the example syncs the snapshot file");
    let close = recorded
        .operations
        .iter()
        .position(|operation| *operation == lm_abi::OP_FS_CLOSE)
        .expect("the example closes the snapshot file");
    assert!(sync < close);
    assert!(!recorded.signal_stream_active());
    assert_eq!(world.world_resource_count(), 0);
}
