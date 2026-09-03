//! Host fidelity: the test host must show the same asynchronous
//! boundary as the command-line host.
//!
//! `CliHost` serves files and streams on worker threads, so a reply
//! arrives at a later poll. A test host that answers inside `start`
//! hides timing errors. A file becomes open only after completion.

use lm_source::SourceFile;
use lm_testkit::{compile_to_bytes, publish_artifact_bytes};
use lm_vm::{
    CompletionKey, Host, HostArg, HostCompletion, HostStart, HostValue, RecordingHost, VmConfig,
    World,
};
use std::cell::RefCell;
use std::rc::Rc;

const SRC: &str = r##"
def worker(): String with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open(Path("message.txt", PathStyle.Posix), ReadOnly)
  in Ok(file)
    case file.read(1024)
    in Ok(b)
      file.close()
      b.text()
    in Err(e) then display(e)
    end
  in Err(e) then display(e)
  end
end

def supervise(child: Run[String]): String with Vm
  case child.drive()
  in Asked(open_request)
    child.dispatch(open_request)
    after_dispatch = child.handles().len()
    case child.drive()
    in Asked(second)
      child.dispatch(second)
      "after open dispatch=#{after_dispatch}, at next request=#{child.handles().len()}"
    in Done(_)  then "child finished"
    in Fault(_) then "child faulted"
    end
  in Done(_)  then "finished early"
  in Fault(_) then "faulted early"
  end
end

child = sys.vm.Vm().activate_or_fault(worker, args: ())
child.table().pass(Fs)
supervise(child)
"##;

#[test]
fn the_test_host_defers_a_file_open_like_the_command_line_host() {
    let bytes = compile_to_bytes("async.lm", SRC).expect("the probe compiles");
    let (arena, namespace) = publish_artifact_bytes(&bytes).expect("the probe loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"hello from memory".to_vec());
    let mut world = World::new(arena, namespace, VmConfig::default(), Box::new(host));
    for g in ["Vm", "Fs"] {
        world.allow(g).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    let text = world.show_outcome(&outcome);
    println!("{text}");
    assert_eq!(
        text, "Done(\"after open dispatch=0, at next request=1\")",
        "the test host answered the open inside `start`"
    );
}

struct FloatHost {
    operation: u32,
}

impl Host for FloatHost {
    fn start(&mut self, _key: CompletionKey, op: u32, args: Vec<HostArg>) -> HostStart {
        assert_eq!(op, self.operation);
        assert_eq!(args, vec![HostArg::Float(1.5f64.to_bits())]);
        HostStart::Completed(HostValue::Float(2.5f64.to_bits()))
    }

    fn poll(&mut self) -> Option<HostCompletion> {
        None
    }

    fn wait(&mut self) -> Option<HostCompletion> {
        None
    }
}

#[test]
fn extension_operations_carry_float_values_across_the_host_boundary() {
    let mut builder = lm_abi::AbiBundle::builder();
    builder.add_group(lm_abi::GroupSpec::namespace("Telemetry"));
    builder.add_operation(lm_abi::OperationSpec::fixed(
        "Telemetry",
        "Scale",
        vec![lm_abi::AbiType::FLOAT],
        lm_abi::AbiType::FLOAT,
    ));
    let bundle = builder.build().expect("the extension bundle is valid");
    let operation = bundle
        .op_by_name("Telemetry.Scale")
        .expect("the operation exists");
    let group = bundle
        .group_by_name("Telemetry")
        .expect("the extension group exists");
    let source = SourceFile::new(
        "float_host.lm",
        "def go(): Float with Telemetry\n  sys.telemetry.scale(1.5)\nend\n\ngo()\n",
    );
    let compiled = lm_compiler::compile_module_with_bundle(
        "test.entry",
        &source,
        &lm_compiler::CompileEnv::new().freeze(),
        true,
        &bundle,
    )
    .expect("the program compiles");
    let go = compiled
        .module
        .funcs
        .iter()
        .find(|function| function.name == "go")
        .expect("the program has go");
    assert_eq!(go.row, vec![lm_bytecode::BcRow::Group(group)]);
    assert!(!compiled
        .module
        .strings
        .iter()
        .any(|text| text == "Telemetry"));
    let root = compiled.path.clone();
    let core = lm_compiler::core_link_unit_with_bundle(&bundle).expect("the core unit builds");
    let mut link_env =
        lm_compiler::core_link_env_with_bundle(&bundle).expect("the core environment builds");
    let unit = compiled
        .into_link_unit_with_bundle(&link_env, &bundle)
        .expect("the program unit builds");
    link_env.bind_unit(unit).expect("the program binds");
    let artifact = link_env
        .freeze()
        .complete_artifact(&root)
        .expect("the program artifact builds");
    let mut arena = lm_link::CodeArena::with_bundle(bundle.clone());
    let namespace = arena
        .publish(artifact, Some(core))
        .expect("the program publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(FloatHost { operation }),
    );
    world.allow("Telemetry").expect("the grant exists");
    let outcome = world.run_root();
    assert_eq!(world.show_outcome(&outcome), "Done(2.5)");
}
