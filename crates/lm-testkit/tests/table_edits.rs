//! Policy-table edits over several targets.
//!
//! `pass`, `block`, and `clear` state one decision over a set of
//! operations. `mock` still names one operation, because it carries a
//! handler for that exact signature.

use lm_testkit::run_allowed;

const CHILD: &str = "do ||: String with Fs.Open, Fs.Read, Fs.Close\n\
                     \x20 case sys.fs.open(\"message.txt\", ReadOnly)\n\
                     \x20 in Ok(f)\n\
                     \x20   text = case f.read(1024)\n\
                     \x20   in Ok(b)  then b.text()\n\
                     \x20   in Err(e) then e.message()\n\
                     \x20   end\n\
                     \x20   f.close()\n\
                     \x20   text\n\
                     \x20 in Err(e) then e.message()\n\
                     \x20 end\n\
                     end";

fn run_with_file(source: &str) -> String {
    use lm_testkit::{compile_to_bytes, repo_root};
    use lm_vm::{load_bytes, RecordingHost, VmConfig, World};
    use std::cell::RefCell;
    use std::rc::Rc;
    let _ = repo_root();
    let bytes = compile_to_bytes("table.lm", source).expect("the program compiles");
    let loaded = load_bytes(&bytes).expect("the program loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("message.txt", b"hello".to_vec());
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    for grant in ["Vm", "Fs"] {
        world.allow(grant).expect("the grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    world.show_outcome(&outcome)
}

#[test]
fn one_pass_states_several_targets() {
    let source = format!(
        "vm = sys.vm.Vm().from_fn({CHILD}, args: ())\n\
         vm.table().pass(Fs.Open, Fs.Read, Fs.Close)\n\
         case vm.run()\n\
         in Done(v)  then v\n\
         in Fault(f) then f.code()\n\
         end\n"
    );
    assert_eq!(run_with_file(&source), "Done(\"hello\")");
}

/// The edit answers one unit however many targets it names, so a call
/// inside a block leaves the stack as the block expects.
#[test]
fn a_multi_target_edit_answers_one_value() {
    let source = format!(
        "def launch(): String with Vm, Fs\n\
         \x20 vm = sys.vm.Vm().from_fn({CHILD}, args: ())\n\
         \x20 vm.table().pass(Fs.Open, Fs.Read, Fs.Close)\n\
         \x20 case vm.run()\n\
         \x20 in Done(v)  then v\n\
         \x20 in Fault(f) then f.code()\n\
         \x20 end\n\
         end\n\
         launch()\n"
    );
    assert_eq!(run_with_file(&source), "Done(\"hello\")");
}

#[test]
fn every_target_of_a_pass_is_charged() {
    let source = "def launch(vm: Vm[Int]): () with Vm, Fs.Open\n\
                  \x20 vm.table().pass(Fs.Open, Fs.Read)\n\
                  end\n\
                  vm = sys.vm.Vm().from_fn(do ||: Int 1 end, args: ())\n\
                  launch(vm)\n";
    let error = run_allowed("table.lm", source, &["Vm", "Fs"])
        .expect_err("the second target is not in the row");
    assert!(
        error.contains("E1046") && error.contains("Fs.Read"),
        "{error}"
    );
}

#[test]
fn a_pass_needs_one_target_or_more() {
    let source = "vm = sys.vm.Vm().from_fn(do ||: Int 1 end, args: ())\n\
                  vm.table().pass()\n\
                  0\n";
    let error = run_allowed("table.lm", source, &["Vm"]).expect_err("no target");
    assert!(error.contains("E1006"), "{error}");
}

#[test]
fn a_mock_still_names_one_operation() {
    let source = "vm = sys.vm.Vm().from_fn(do ||: Int with Clock.Now\n\
                  \x20 sys.clock.now()\n\
                  end, args: ())\n\
                  vm.table().mock(Clock.Now, Clock.Monotonic, do ||: Int 5 end)\n\
                  0\n";
    let error = run_allowed("table.lm", source, &["Vm", "Clock"]).expect_err("mock takes one");
    assert!(error.contains("E1006"), "{error}");
}
