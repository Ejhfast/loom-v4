//! Anonymous pipe and operating-system child effects.

use lm_testkit::{compile_to_bytes, publish_artifact_bytes, repo_root};
use lm_vm::{
    CompletionKey, Host, HostArg, HostChildEnv, HostCompletion, HostStart, RecordingHost, VmConfig,
    World,
};
use std::cell::RefCell;
use std::rc::Rc;

fn run(source: &str, grants: &[&str], configure: impl FnOnce(&mut RecordingHost)) -> String {
    execute(source, grants, configure).0
}

fn execute(
    source: &str,
    grants: &[&str],
    configure: impl FnOnce(&mut RecordingHost),
) -> (String, Rc<RefCell<RecordingHost>>) {
    let bytes = compile_to_bytes("process-effects.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    configure(&mut host.borrow_mut());
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(Rc::clone(&host)),
    );
    for grant in grants {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    (world.show_outcome(&outcome), host)
}

fn run_real(source: &str, grants: &[&str]) -> String {
    let bytes = compile_to_bytes("real-process-effects.lm", source).expect("the program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the program loads");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(lm_host::CliHost::new(1)),
    );
    for grant in grants {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn pipe_writes_are_partial_and_close_produces_end_of_input() {
    let source = r##"
def go(): (String, Int) with Pipe
  case Pipe().open()
  in Ok((reader, writer))
    writer.write_all(b"abcdefghij").expect("the write completes")
    writer.close().expect("the writer closes")
    bytes = reader.read(32).expect("the data reads")
    ending = reader.read(32).expect("the end reads")
    reader.close().expect("the reader closes")
    (bytes.hex(), ending.len())
  in Err(error)
    (display(error), -1)
  end
end
go()
"##;

    assert_eq!(
        run(source, &["Pipe"], |_| {}),
        "Done((\"6162636465666768696a\", 0))"
    );
}

#[test]
fn a_losing_pipe_read_preserves_its_bytes() {
    let source = r#"
def go(): (String, String) with Pipe, Clock, Wait
  case Pipe().open()
  in Ok((reader, writer))
    writer.write_all(b"abc").expect("the write completes")
    selected = select
    in sys.clock.sleep.wait(0) -> _
      "timer"
    in reader.read_wait(3) -> _
      "pipe"
    end
    bytes = reader.read(3).expect("the bytes remain")
    writer.close().expect("the writer closes")
    reader.close().expect("the reader closes")
    (selected, bytes.hex())
  in Err(error)
    (display(error), "")
  end
end
go()
"#;

    assert_eq!(
        run(source, &["Pipe", "Clock", "Wait"], |_| {}),
        "Done((\"timer\", \"616263\"))"
    );
}

#[test]
fn a_spawn_consumes_child_pipe_ends_and_a_losing_wait_keeps_status() {
    let source = r##"
def go(): (String, String, Int) with Pipe, Exec, Clock, Wait
  case Pipe().open()
  in Ok((reader, writer))
    directory: Option[String] = None
    spec = ExecSpec(
      "emit",
      List[String](),
      directory,
      ChildEnv.Inherit,
      ChildInput.Null,
      ChildOutput.Pipe(writer),
      ChildOutput.Null
    )
    child = Exec().spawn(spec).expect("the child starts")
    closed = case writer.write(b"later")
    in Err(PipeError.Closed) then "closed"
    in Err(error) then display(error)
    in Ok(_) then "open"
    end
    bytes = reader.read(32).expect("the child output reads")
    selected = select
    in sys.clock.sleep.wait(0) -> _
      "timer"
    in child.wait_source() -> _
      "child"
    end
    code = case child.wait().expect("the child status remains")
    in ChildStatus.Exited(value) then value
    in ChildStatus.Terminated then -1
    end
    reader.close().expect("the reader closes")
    (closed, "#{selected}:#{bytes.hex()}", code)
  in Err(error)
    (display(error), "", -1)
  end
end
go()
"##;

    assert_eq!(
        run(source, &["Pipe", "Exec", "Clock", "Wait"], |host| {
            host.set_child_program("emit", 7, b"hello".to_vec(), Vec::new());
        }),
        "Done((\"closed\", \"timer:68656c6c6f\", 7))"
    );
}

#[test]
fn a_child_environment_overlay_keeps_inherited_values() {
    let source = r#"
def go(): String with Exec
  values = Map[String, String]()
  values.put("PATH", "/child/bin")
  values.put("MODE", "release")
  directory: Option[String] = None
  spec = ExecSpec(
    "inspect",
    List[String](),
    directory,
    ChildEnv.Overlay(values),
    ChildInput.Null,
    ChildOutput.Null,
    ChildOutput.Null
  )
  child = Exec().spawn(spec).expect("the child starts")
  child.close().expect("the child closes")
  "started"
end
go()
"#;

    struct EnvironmentHost {
        inner: RecordingHost,
        seen: Rc<RefCell<Option<HostChildEnv>>>,
    }

    impl Host for EnvironmentHost {
        fn start(&mut self, key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
            if op == lm_abi::OP_EXEC_SPAWN {
                let [HostArg::ExecSpec(spec)] = args.as_slice() else {
                    return HostStart::Failed("Exec.Spawn needs one child specification".into());
                };
                *self.seen.borrow_mut() = Some(spec.environment.clone());
            }
            self.inner.start(key, op, args)
        }

        fn poll(&mut self) -> Option<HostCompletion> {
            self.inner.poll()
        }

        fn wait(&mut self) -> Option<HostCompletion> {
            self.inner.wait()
        }

        fn close_child(&mut self, token: u64) -> bool {
            self.inner.close_child(token)
        }
    }

    let bytes = compile_to_bytes("child-environment-overlay.lm", source)
        .expect("the overlay program compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the overlay program loads");
    let seen = Rc::new(RefCell::new(None));
    let mut inner = RecordingHost::new(1);
    inner.set_child_program("inspect", 0, Vec::new(), Vec::new());
    let host = EnvironmentHost {
        inner,
        seen: Rc::clone(&seen),
    };
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    world.allow("Exec").expect("the Exec grant exists");
    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(\"started\")");
    assert_eq!(
        *seen.borrow(),
        Some(HostChildEnv::Overlay(vec![
            ("PATH".into(), "/child/bin".into()),
            ("MODE".into(), "release".into()),
        ]))
    );
}

#[test]
fn live_process_resources_name_their_snapshot_blockers() {
    let source = r#"
def blocker(): (String, Bool) with Pipe, Vm
  case Pipe().open()
  in Ok((reader, writer))
    kind = case sys.vm.snapshot_self()
    in Err(SnapshotError.ResourceActive(_, name)) then name
    in Err(error) then display(error)
    in Ok(_) then "no blocker"
    end
    writer.close().expect("the writer closes")
    reader.close().expect("the reader closes")
    after = case sys.vm.snapshot_self()
    in Ok(_) then true
    in Err(_) then false
    end
    (kind, after)
  in Err(error)
    (display(error), false)
  end
end
blocker()
"#;

    assert_eq!(
        run(source, &["Pipe", "Vm"], |_| {}),
        "Done((\"pipe reader\", true))"
    );
}

#[test]
fn a_live_child_names_its_snapshot_blocker() {
    let source = r#"
def blocker(): (String, Bool) with Exec, Vm
  directory: Option[String] = None
  spec = ExecSpec(
    "idle",
    List[String](),
    directory,
    ChildEnv.Inherit,
    ChildInput.Null,
    ChildOutput.Null,
    ChildOutput.Null
  )
  child = Exec().spawn(spec).expect("the child starts")
  kind = case sys.vm.snapshot_self()
  in Err(SnapshotError.ResourceActive(_, name)) then name
  in Err(error) then display(error)
  in Ok(_) then "no blocker"
  end
  child.close().expect("the child closes")
  after = case sys.vm.snapshot_self()
  in Ok(_) then true
  in Err(_) then false
  end
  (kind, after)
end
blocker()
"#;

    assert_eq!(
        run(source, &["Exec", "Vm"], |host| {
            host.set_child_program("idle", 0, Vec::new(), Vec::new());
        }),
        "Done((\"child handle\", true))"
    );
}

#[test]
fn a_machine_fault_closes_its_pipe_ends_and_child() {
    let source = r#"
def stop(): Never with Pipe, Exec
  pair = Pipe().open().expect("the pipe opens")
  directory: Option[String] = None
  spec = ExecSpec(
    "idle",
    List[String](),
    directory,
    ChildEnv.Inherit,
    ChildInput.Null,
    ChildOutput.Null,
    ChildOutput.Null
  )
  child = Exec().spawn(spec).expect("the child starts")
  panic("stop")
end
stop()
"#;
    let (outcome, host) = execute(source, &["Pipe", "Exec"], |host| {
        host.set_child_program("idle", 0, Vec::new(), Vec::new());
    });

    assert_eq!(outcome, "Fault(UserPanic)");
    assert_eq!(host.borrow().pipe_end_count(), 0);
    assert_eq!(host.borrow().child_count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_real_child_writes_through_a_pipe_without_an_implicit_shell() {
    let source = std::fs::read_to_string(repo_root().join("examples/04-effects/pipeline.lm"))
        .expect("the example reads");

    assert_eq!(
        run_real(&source, &["Pipe", "Exec"]),
        "Done(\"real-child:0\")"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_missing_real_child_returns_a_typed_error() {
    let source = r#"
def go(): String with Exec
  directory: Option[String] = None
  spec = ExecSpec(
    "loom-command-that-does-not-exist",
    List[String](),
    directory,
    ChildEnv.Inherit,
    ChildInput.Null,
    ChildOutput.Null,
    ChildOutput.Null
  )
  case Exec().spawn(spec)
  in Err(ExecError.NotFound(_)) then "not found"
  in Err(error) then display(error)
  in Ok(child)
    child.close().expect("the child closes")
    "started"
  end
end
go()
"#;

    assert_eq!(run_real(source, &["Exec"]), "Done(\"not found\")");
}
