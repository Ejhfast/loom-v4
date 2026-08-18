//! Request patterns.
//!
//! `Call(Op, call, args)` tests one operation identity, binds the
//! pending call, and destructures its arguments. One `case` therefore
//! serves several operations without one nested `case` per operation.

use lm_testkit::run_allowed;

const CHILD: &str = "do ||: String with Fs.Open, Fs.Read, Fs.Close\n\
                     \x20 case sys.fs.open(\"memory.txt\", ReadOnly)\n\
                     \x20 in Ok(f)\n\
                     \x20   text = case f.read(6)\n\
                     \x20   in Ok(b)  then b.text()\n\
                     \x20   in Err(e) then e.message()\n\
                     \x20   end\n\
                     \x20   f.close()\n\
                     \x20   text\n\
                     \x20 in Err(e) then e.message()\n\
                     \x20 end\n\
                     end";

fn serve(body: &str) -> String {
    let source = format!(
        "def serve(vm: Vm[String], contents: Bytes, mut seen: [Int]): String with Vm\n\
         \x20 loop do\n\
         \x20   case vm.drive()\n\
         \x20   in Asked(request)\n\
         {body}\
         \x20   in Done(value)\n\
         \x20     return \"{{value}}/{{seen.len()}}\"\n\
         \x20   in Fault(_)\n\
         \x20     return \"the child faulted\"\n\
         \x20   end\n\
         \x20 end\n\
         end\n\
         seen: [Int] = []\n\
         serve(sys.vm.Vm().from_fn({CHILD}, args: ()), Bytes(\"abcdef\"), seen)\n"
    );
    run_allowed("request.lm", &source, &["Vm"]).expect("the program compiles")
}

const FLAT: &str = "\x20     case request\n\
                    \x20     in Call(Fs.Open, call, (_, _))\n\
                    \x20       seen.push(1)\n\
                    \x20       vm.serve_file(call)\n\
                    \x20       ()\n\
                    \x20     in Call(Fs.Read, call, (_, count))\n\
                    \x20       seen.push(count)\n\
                    \x20       vm.answer(call, Ok(contents))\n\
                    \x20     in Call(Fs.Close, call, (_,))\n\
                    \x20       seen.push(3)\n\
                    \x20       vm.answer(call, Ok(()))\n\
                    \x20     in _\n\
                    \x20       vm.dispatch(request)\n\
                    \x20     end\n";

#[test]
fn one_case_serves_several_operations() {
    // Three operations, one `case`, and the read count arrives through
    // the argument pattern.
    assert_eq!(serve(FLAT), "Done(\"abcdef/3\")");
}

/// A minimal driver body, so a negative case reads its own error.
fn drive_body(arms: &str) -> String {
    format!(
        "def serve(vm: Vm[String]): String with Vm\n\
         \x20 case vm.drive()\n\
         \x20 in Asked(request)\n\
         \x20   case request\n\
         {arms}\
         \x20   end\n\
         \x20 in Done(v)  then v\n\
         \x20 in Fault(_) then \"f\"\n\
         \x20 end\n\
         end\n\
         serve(sys.vm.Vm().from_fn({CHILD}, args: ()))\n"
    )
}

#[test]
fn a_request_case_needs_a_final_wildcard() {
    // The operation set is open, so no set of `Call` arms covers a
    // request.
    let source = drive_body(
        "\x20   in Call(Fs.Read, _, (_, _)) then \"read\"\n\
         \x20   in Call(Fs.Close, _, (_,))  then \"close\"\n",
    );
    let error = run_allowed("request.lm", &source, &["Vm"]).expect_err("no wildcard arm");
    assert!(error.contains("E1042"), "{error}");
}

#[test]
fn a_repeated_operation_arm_is_unreachable() {
    let source = drive_body(
        "\x20   in Call(Fs.Read, _, (_, _)) then \"first\"\n\
         \x20   in Call(Fs.Read, _, (_, _)) then \"second\"\n\
         \x20   in _                        then \"other\"\n",
    );
    let error = run_allowed("request.lm", &source, &["Vm"]).expect_err("the arm repeats");
    assert!(error.contains("E1043"), "{error}");
}

#[test]
fn a_call_pattern_names_a_manifest_operation() {
    let body = FLAT.replace(
        "in Call(Fs.Open, call, (_, _))",
        "in Call(Fs.Nope, call, (_, _))",
    );
    let source = format!(
        "def serve(vm: Vm[String], contents: Bytes, mut seen: [Int]): String with Vm\n\
         \x20 case vm.drive()\n\
         \x20 in Asked(request)\n\
         {body}\
         \x20   \"x\"\n\
         \x20 in Done(v)  then v\n\
         \x20 in Fault(_) then \"f\"\n\
         \x20 end\n\
         end\n\
         seen: [Int] = []\n\
         serve(sys.vm.Vm().from_fn({CHILD}, args: ()), Bytes(\"ab\"), seen)\n"
    );
    let error = run_allowed("request.lm", &source, &["Vm"]).expect_err("no such operation");
    assert!(error.contains("E1051"), "{error}");
}

#[test]
fn a_call_pattern_checks_its_argument_arity() {
    let body = FLAT.replace(
        "in Call(Fs.Read, call, (_, count))",
        "in Call(Fs.Read, call, (_,))",
    );
    let source = format!(
        "def serve(vm: Vm[String], contents: Bytes, mut seen: [Int]): String with Vm\n\
         \x20 case vm.drive()\n\
         \x20 in Asked(request)\n\
         {body}\
         \x20   \"x\"\n\
         \x20 in Done(v)  then v\n\
         \x20 in Fault(_) then \"f\"\n\
         \x20 end\n\
         end\n\
         seen: [Int] = []\n\
         serve(sys.vm.Vm().from_fn({CHILD}, args: ()), Bytes(\"ab\"), seen)\n"
    );
    let error = run_allowed("request.lm", &source, &["Vm"]).expect_err("wrong arity");
    assert!(error.contains("E1041"), "{error}");
}
