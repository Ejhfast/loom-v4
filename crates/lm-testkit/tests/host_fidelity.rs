//! Host fidelity: the test host must show the same asynchronous
//! boundary as the command-line host.
//!
//! `CliHost` serves files and streams on worker threads, so a reply
//! arrives at a later poll. A test host that answers inside `start`
//! hides timing bugs: an example once passed here and failed under
//! `lm run`, because a file is not open until the open completes.

use lm_testkit::compile_to_bytes;
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

const SRC: &str = r#"
def worker(): String with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open("message.txt", ReadOnly)
  in Ok(file)
    case file.read(1024)
    in Ok(b)
      file.close()
      b.text()
    in Err(e) then e.message()
    end
  in Err(e) then e.message()
  end
end

def supervise(child: Vm[String]): String with Vm
  case child.drive()
  in Asked(open_request)
    child.dispatch(open_request)
    after_dispatch = child.handles().len()
    case child.drive()
    in Asked(second)
      child.dispatch(second)
      "after open dispatch={after_dispatch}, at next request={child.handles().len()}"
    in Done(_)  then "child finished"
    in Fault(_) then "child faulted"
    end
  in Done(_)  then "finished early"
  in Fault(_) then "faulted early"
  end
end

child = sys.vm.Vm().from_fn(worker, args: ())
child.table().pass(Fs)
supervise(child)
"#;

#[test]
fn the_test_host_defers_a_file_open_like_the_command_line_host() {
    let bytes = compile_to_bytes("async.lm", SRC).expect("the probe compiles");
    let loaded = load_bytes(&bytes).expect("the probe loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut()
        .set_file("message.txt", b"hello from memory".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
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
