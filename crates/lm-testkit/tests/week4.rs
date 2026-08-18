//! Week-4 suites: operations and rows, policy tables, and the three
//! VM driving modes.

use lm_proc::{Scheduler, SchedulerMode};
use lm_testkit::{compile_text, compile_to_bytes, run_allowed, run_text, run_world};
use lm_vm::{RecordingHost, VmConfig, World};

fn code_of(source: &str) -> String {
    let rendered = compile_text("t.lm", source).unwrap_err();
    rendered[6..11].to_string()
}

fn runs(source: &str) -> String {
    run_text("t.lm", source, VmConfig::default()).unwrap()
}

fn allowed(source: &str, allow: &[&str]) -> String {
    run_allowed("t.lm", source, allow).unwrap()
}

// ---------------------------------------------------------------
// Rows against the real operation manifest.
// ---------------------------------------------------------------

#[test]
fn rows_validate_against_the_manifest() {
    // A fabricated operation name is a diagnostic now.
    assert_eq!(code_of("def f() with Io.Blast\nend\n1\n"), "E1050");
    assert_eq!(code_of("def f() with Web\nend\n1\n"), "E1050");
    assert_eq!(code_of("def f() with Zzz.Op\nend\n1\n"), "E1050");
    // Manifest groups without week-4 operations stay valid row names.
    assert_eq!(runs("def f() with Fs, Net\nend\nf()\n"), "Done(())");
    // Rows inside function types validate too.
    assert_eq!(
        code_of("def f(g: () -> Int with Io.Blast): Int\n  1\nend\n1\n"),
        "E1050"
    );
}

#[test]
fn direct_performs_charge_the_exact_operation() {
    // Perform without the row.
    assert_eq!(code_of("def f()\n  sys.io.print(\"x\")\nend\n1\n"), "E1046");
    // The exact row admits the perform; the group admits it too.
    assert!(compile_text(
        "t.lm",
        "def f() with Io.Print\n  sys.io.print(\"x\")\nend\n1\n"
    )
    .is_ok());
    assert!(compile_text("t.lm", "def f() with Io\n  sys.io.print(\"x\")\nend\n1\n").is_ok());
    // A sibling exact operation does not admit it.
    assert_eq!(
        code_of("def f() with Io.Error\n  sys.io.print(\"x\")\nend\n1\n"),
        "E1046"
    );
}

#[test]
fn sys_misuse_is_precise() {
    assert_eq!(code_of("x = sys\n1\n"), "E1051");
    assert_eq!(code_of("x = sys.io\n1\n"), "E1051");
    assert_eq!(code_of("x = sys.web\n1\n"), "E1051");
    assert_eq!(
        code_of("def f() with Io\n  sys.io.blast(\"x\")\nend\n1\n"),
        "E1051"
    );
    // A VM control operation is not a first-class value.
    assert_eq!(code_of("x = sys.vm.Vm\n1\n"), "E1051");
    // A local named `sys` shadows the ABI root.
    assert_eq!(runs("sys = 5\nsys + 1\n"), "Done(6)");
}

#[test]
fn first_class_operation_values_carry_their_identity() {
    // Storing and calling through a variable charges the identity.
    assert_eq!(
        code_of("def f()\n  p = sys.io.print\n  p(\"x\")\nend\n1\n"),
        "E1046"
    );
    assert!(compile_text(
        "t.lm",
        "def f() with Io.Print\n  p = sys.io.print\n  p(\"x\")\nend\n1\n"
    )
    .is_ok());
    // Passing the value as an argument: the parameter type is the
    // identity-indexed Op type, which no plain function type accepts.
    assert_eq!(
        code_of("def call_it(g: (String) -> ()): ()\n  g(\"x\")\nend\ncall_it(sys.io.print)\n"),
        "E1004"
    );
    // The perform through the value reaches the host.
    let (out, host) = run_world(
        "t.lm",
        "def f() with Io.Print\n  p = sys.io.print\n  p(\"one\")\n  p(\"two\")\nend\nf()\n",
        &["Io.Print"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(())");
    assert_eq!(host.borrow().printed, vec!["one", "two"]);
}

#[test]
fn effect_variables_bind_from_explicitly_rowed_closures() {
    // A closure with an explicit row binds the effect variable of a
    // higher-order callee; the caller is charged the bound row.
    let source = "def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e\n  f(x)\nend\n\
                  def go(): Int with Io.Print\n  \
                  apply(1, do |n: Int|: Int with Io.Print\n    sys.io.print(\"n\\n\")\n    n\n  end)\n\
                  end\ngo()\n";
    let (out, host) = run_world("t.lm", source, &["Io.Print"], VmConfig::default()).unwrap();
    assert_eq!(out, "Done(1)");
    assert_eq!(host.borrow().printed, vec!["n\n"]);
    // Without the charge in the enclosing pure row it is rejected.
    assert_eq!(
        code_of(
            "def apply[T, U, effect e](x: T, f: (T) -> U with e): U with e\n  f(x)\nend\n\
             def go(): Int\n  \
             apply(1, do |n: Int|: Int with Io.Print\n    sys.io.print(\"n\\n\")\n    n\n  end)\n\
             end\n1\n"
        ),
        "E1046"
    );
}

#[test]
fn table_pass_charges_the_granter_row() {
    // Passing a group the granter does not carry is rejected.
    assert_eq!(
        code_of("def f(vm: Vm[Int]) with Vm\n  vm.table().pass(Io)\nend\n1\n"),
        "E1046"
    );
    // Passing the exact operation under the exact row compiles.
    assert!(compile_text(
        "t.lm",
        "def f(vm: Vm[Int]) with Vm, Io.Print\n  vm.table().pass(Io.Print)\nend\n1\n"
    )
    .is_ok());
    // An exact grant does not admit a whole-group pass.
    assert_eq!(
        code_of("def f(vm: Vm[Int]) with Vm, Io.Print\n  vm.table().pass(Io)\nend\n1\n"),
        "E1046"
    );
    // block and mock charge nothing.
    assert!(compile_text(
        "t.lm",
        "def f(vm: Vm[Int]) with Vm\n  vm.table().block(Io)\n  \
         vm.table().mock(Clock.Now, do ||: Int 1 end)\n  vm.table().clear(Io)\nend\n1\n"
    )
    .is_ok());
}

#[test]
fn mock_needs_the_exact_pure_signature() {
    // Wrong result type.
    assert_eq!(
        code_of(
            "def f(vm: Vm[Int]) with Vm\n  vm.table().mock(Clock.Now, do ||: Bool true end)\nend\n1\n"
        ),
        "E1004"
    );
    // A handler with a row is rejected: the expected type has the
    // empty row.
    assert_eq!(
        code_of(
            "def f(vm: Vm[Int]) with Vm\n  \
             vm.table().mock(Clock.Now, do ||: Int with Io.Print 1 end)\nend\n1\n"
        ),
        "E1004"
    );
    // A group target cannot be mocked.
    assert_eq!(
        code_of("def f(vm: Vm[Int]) with Vm\n  vm.table().mock(Clock, do ||: Int 1 end)\nend\n1\n"),
        "E1051"
    );
}

#[test]
fn typed_answer_mismatch_is_static() {
    assert_eq!(
        code_of(
            "def bad(vm: Vm[Int]) with Vm\n  case vm.drive()\n  in Asked(q)\n    \
             case q\n    in Call(Io.Print, call, (_,)) then vm.answer(call, 5)\n    \
             in _ then ()\n    end\n  in Done(_) then ()\n  in Fault(_) then ()\n  end\nend\n1\n"
        ),
        "E1004"
    );
}

#[test]
fn a_label_must_name_a_parameter_of_the_target() {
    // `args` is a parameter of `from_fn` alone. Another target does
    // not gain the name.
    assert_eq!(
        code_of("def f(x: Int): Int\n  x\nend\nf(args: 3)\n"),
        "E1006"
    );
    assert_eq!(
        code_of("e = sys.vm.Vm()\nv = e.from_fn(do || 1 end, wrong: ())\n1\n"),
        "E1006"
    );
}

// ---------------------------------------------------------------
// Class constructor patterns.
// ---------------------------------------------------------------

#[test]
fn class_constructor_patterns_destructure_core_pair() {
    assert_eq!(
        runs("p = Pair(2, \"x\")\ncase p\nin Pair(a, b) then \"{b}{a}\"\nend\n"),
        "Done(\"x2\")"
    );
    // Nested patterns inside a class constructor pattern.
    assert_eq!(
        runs(
            "p: Pair[Option[Int], Int] = Pair(Some(4), 1)\ncase p\nin Pair(Some(v), n) then v + n\n\
             in Pair(None, n) then n\nend\n"
        ),
        "Done(5)"
    );
    // A user class destructures in declaration order.
    assert_eq!(
        runs(
            "class Point\n  x: Int = 1\n  y: Int = 2\nend\n\
             case Point()\nin Point(a, b) then a * 10 + b\nend\n"
        ),
        "Done(12)"
    );
}

// ---------------------------------------------------------------
// Policy tables.
// ---------------------------------------------------------------

#[test]
fn default_deny_blocks_at_the_root() {
    let source = "def f() with Io.Print\n  sys.io.print(\"x\")\nend\nf()\n";
    assert_eq!(allowed(source, &[]), "Fault(PolicyDenied)");
    assert_eq!(allowed(source, &["Io.Print"]), "Done(())");
    // A group grant covers the exact operation.
    assert_eq!(allowed(source, &["Io"]), "Done(())");
    // A different exact grant does not.
    assert_eq!(allowed(source, &["Io.Error"]), "Fault(PolicyDenied)");
}

const CHILD_PRINT: &str = "def spawn_print(): Vm[()] with Vm\n  \
    sys.vm.Vm().from_fn(do || with Io.Print\n    sys.io.print(\"c\\n\")\n  end, args: ())\n\
    end\n";

#[test]
fn exact_beats_group_in_one_table() {
    // Group pass plus exact block: the exact entry wins.
    let source = format!(
        "{CHILD_PRINT}\
         def go(): String with Vm, Io\n  vm = spawn_print()\n  \
         vm.table().pass(Io)\n  vm.table().block(Io.Print)\n  \
         case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n"
    );
    assert_eq!(allowed(&source, &["Vm", "Io"]), "Done(\"PolicyDenied\")");
    // Group block plus exact pass: the exact entry wins again.
    let source = format!(
        "{CHILD_PRINT}\
         def go(): String with Vm, Io.Print\n  vm = spawn_print()\n  \
         vm.table().block(Io)\n  vm.table().pass(Io.Print)\n  \
         case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n"
    );
    let (out, host) = run_world("t.lm", &source, &["Vm", "Io.Print"], VmConfig::default()).unwrap();
    assert_eq!(out, "Done(\"done\")");
    assert_eq!(host.borrow().printed, vec!["c\n"]);
}

#[test]
fn clear_returns_the_target_to_the_default_block() {
    let source = format!(
        "{CHILD_PRINT}\
         def go(): String with Vm, Io\n  vm = spawn_print()\n  \
         vm.table().pass(Io)\n  vm.table().clear(Io)\n  \
         case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n"
    );
    assert_eq!(allowed(&source, &["Vm", "Io"]), "Done(\"PolicyDenied\")");
}

#[test]
fn pass_chain_reaches_the_root_and_fails_closed() {
    // Grandchild -> child -> root: every level passes Io.
    let source = "def go(): String with Vm, Io\n  \
        vm = sys.vm.Vm().from_fn(do || with Vm, Io\n    \
        inner = sys.vm.Vm().from_fn(do || with Io.Print\n      sys.io.print(\"deep\\n\")\n    end, args: ())\n    \
        inner.table().pass(Io)\n    \
        case inner.run()\n    in Done(_) then \"done\"\n    in Fault(f) then f.code()\n    end\n  end, args: ())\n  \
        vm.table().pass(Io)\n  vm.table().pass(Vm)\n  \
        case vm.run()\n  in Done(s) then s\n  in Fault(f) then f.code()\n  end\nend\ngo()\n";
    let (out, host) = run_world("t.lm", source, &["Vm", "Io"], VmConfig::default()).unwrap();
    assert_eq!(out, "Done(\"done\")");
    assert_eq!(host.borrow().printed, vec!["deep\n"]);
    // Without the root grant the same chain fails closed.
    assert_eq!(allowed(source, &["Vm"]), "Done(\"PolicyDenied\")");
}

#[test]
fn mock_runs_pure_and_bounded() {
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now() + 1\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int 41 end)\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(42)");
    // A faulting mock faults the controlled guest with HostFault.
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now() + 1\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int 1 / 0 end)\n  \
        case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"HostFault\")");
    // A mock that exhausts its work budget faults the guest too.
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now() + 1\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int\n    while 0 == 0\n    end\n    1\n  end)\n  \
        case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"HostFault\")");
    // A mock with captures uses the frozen captured values.
    let source = "def go(): Int with Vm\n  \
        base = 40\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now() + 2\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int base end)\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(42)");
    // Installation boundary-copies the handler, so a mutable capture
    // crosses and a later write into the source misses the copy.
    let source = "def go(): Int with Vm\n  \
        xs = [7]\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int xs.len() + 40 end)\n  \
        xs.push(1)\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(41)");
}

#[test]
fn live_table_edits_affect_future_lookups() {
    // The holder steps the child, then revokes Io.Print between the
    // first and second print. The second perform faults.
    let source = "def go(): String with Vm, Io\n  \
        vm = sys.vm.Vm().from_fn(do || with Io.Print\n    \
        sys.io.print(\"a\\n\")\n    sys.io.print(\"b\\n\")\n  end, args: ())\n  \
        vm.table().pass(Io)\n  \
        steps = 0\n  \
        revoked = false\n  \
        result = \"run\"\n  \
        while steps < 100000\n    \
        steps = steps + 1\n    \
        case vm.step()\n    \
        in Ran\n      \
        if not revoked\n        \
        vm.table().block(Io.Print)\n        \
        revoked = true\n      \
        end\n    \
        in Waiting then ()\n    \
        in Done(_)\n      result = \"done\"\n      break\n    \
        in Fault(f)\n      result = f.code()\n      break\n    \
        end\n  \
        end\n  \
        result\nend\ngo()\n";
    let (out, host) = run_world("t.lm", source, &["Vm", "Io"], VmConfig::default()).unwrap();
    assert_eq!(out, "Done(\"PolicyDenied\")");
    // The revocation lands after the first retired instruction, well
    // before the first perform, so nothing prints.
    assert_eq!(host.borrow().printed, Vec::<String>::new());
}

// ---------------------------------------------------------------
// VM states and transitions.
// ---------------------------------------------------------------

#[test]
fn aliased_empty_vm_rejects_a_second_load() {
    let source = "def go(): Int with Vm\n  e = sys.vm.Vm()\n  \
        a = e.from_fn(do || 1 end, args: ())\n  \
        b = e.from_fn(do || 2 end, args: ())\n  3\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidVmState)");
}

#[test]
fn terminal_execution_calls_are_idempotent() {
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || 21 end, args: ())\n  \
        first = case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\n  \
        second = case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\n  \
        third = case vm.drive()\n  in Done(v) then v\n  in Asked(_) then 0 - 2\n  in Fault(_) then 0 - 1\n  end\n  \
        first + second + third\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(63)");
}

#[test]
fn asked_rejects_run_and_step_and_recovers_tokens_through_drive() {
    // run() while asked faults the caller.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        case vm.drive()\n  in Asked(q)\n    \
        case vm.run()\n    in Done(_) then 1\n    in Fault(_) then 2\n    end\n  \
        in Done(_) then 3\n  in Fault(_) then 4\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidVmState)");
    // Token recovery: a second drive mints a fresh token; the stale
    // call token faults the caller with InvalidRequestToken.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        case vm.drive()\n  in Asked(q1)\n    \
        case q1\n    in Call(Clock.Now, stale, ())\n      \
        case vm.drive()\n      in Asked(q2)\n        vm.answer(stale, 9)\n        1\n      \
        in Done(_) then 2\n      in Fault(_) then 3\n      end\n    \
        in _ then 4\n    end\n  \
        in Done(_) then 5\n  in Fault(_) then 6\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidRequestToken)");
    // The fresh token answers.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        case vm.drive()\n  in Asked(q1)\n    \
        case vm.drive()\n    in Asked(q2)\n      \
        case q2\n      in Call(Clock.Now, call, ())\n        \
        vm.answer(call, 40)\n        \
        case vm.run()\n        in Done(v) then v + 2\n        in Fault(_) then 0 - 1\n        end\n      \
        in _ then 0 - 2\n      end\n    \
        in Done(_) then 0 - 3\n    in Fault(_) then 0 - 4\n    end\n  \
        in Done(_) then 0 - 5\n  in Fault(_) then 0 - 6\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(42)");
}

#[test]
fn continuation_methods_need_an_asked_machine() {
    // dispatch with a consumed token: after the answer the machine
    // has no pending request, so the second continuation faults
    // InvalidRequestToken (specification 12.3).
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        case vm.drive()\n  in Asked(q)\n    \
        case q\n    in Call(Clock.Now, call, ())\n      \
        vm.answer(call, 7)\n      vm.dispatch(q)\n      1\n    \
        in _ then 2\n    end\n  \
        in Done(_) then 3\n  in Fault(_) then 4\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidRequestToken)");
}

#[test]
fn cross_vm_tokens_fault_safely() {
    let source = "def spawn_now(): Vm[Int] with Vm\n  \
        sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\nend\n\
        def go(): Int with Vm\n  \
        a = spawn_now()\n  b = spawn_now()\n  \
        case a.drive()\n  in Asked(qa)\n    \
        case b.drive()\n    in Asked(qb)\n      \
        case qa\n      in Call(Clock.Now, call_a, ())\n        \
        b.answer(call_a, 1)\n        1\n      \
        in _ then 2\n      end\n    \
        in Done(_) then 3\n    in Fault(_) then 4\n    end\n  \
        in Done(_) then 5\n  in Fault(_) then 6\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidRequestToken)");
    // A request token of one machine cannot dispatch another.
    let source = "def spawn_now(): Vm[Int] with Vm\n  \
        sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\nend\n\
        def go(): Int with Vm\n  \
        a = spawn_now()\n  b = spawn_now()\n  \
        case a.drive()\n  in Asked(qa)\n    \
        case b.drive()\n    in Asked(qb)\n      b.dispatch(qa)\n      1\n    \
        in Done(_) then 2\n    in Fault(_) then 3\n    end\n  \
        in Done(_) then 4\n  in Fault(_) then 5\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Fault(InvalidRequestToken)");
}

#[test]
fn reject_installs_the_supplied_fault() {
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now()\n  end, args: ())\n  \
        blocked = sys.vm.Vm().from_fn(do || with Io.Print\n    sys.io.print(\"x\")\n  end, args: ())\n  \
        case blocked.run()\n  in Done(_) then \"no-fault\"\n  in Fault(fault)\n    \
        case vm.drive()\n    in Asked(q)\n      \
        vm.reject(q, fault)\n      \
        case vm.run()\n      in Done(_) then \"done\"\n      in Fault(f2) then f2.code()\n      end\n    \
        in Done(_) then \"early-done\"\n    in Fault(_) then \"early-fault\"\n    end\n  \
        end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"PolicyDenied\")");
}

/// A driver denies one request without a second machine.
///
/// `Clock.Now` replies `Int`, so no error arm exists. Before
/// `Fault.denied`, the driver had to invent a time.
const DENY_CLOCK: &str = "def child(): Int with Clock.Now\n  sys.clock.now()\nend\n\
    def go(): String with Vm\n  \
    vm = sys.vm.Vm().from_fn(child, args: ())\n  \
    case vm.drive()\n  in Asked(request)\n    \
    vm.reject(request, Fault.denied(\"the clock is not permitted\"))\n    \
    case vm.run()\n    in Done(_) then \"done\"\n    in Fault(f) then f.code()\n    end\n  \
    in Done(_) then \"early-done\"\n  in Fault(_) then \"early-fault\"\n  end\nend\ngo()\n";

#[test]
fn a_denied_fault_stops_a_request_with_no_error_reply() {
    assert_eq!(allowed(DENY_CLOCK, &["Vm"]), "Done(\"PolicyDenied\")");
}

/// The denied fault keeps its reason, and `reject` fills the
/// operation from the pending record of the target.
#[test]
fn a_denied_fault_carries_its_reason_and_operation() {
    let bytes = compile_to_bytes("t.lm", DENY_CLOCK).expect("the program compiles");
    let loaded = lm_vm::load_bytes(&bytes).expect("the program verifies");
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a group");
    let mut scheduler = Scheduler::new(SchedulerMode::Deterministic);
    scheduler.run(&mut world);
    let fault = world.fault_of(1).expect("the child faulted");
    assert_eq!(fault.code, lm_abi::FaultCode::PolicyDenied);
    assert_eq!(fault.message, "the clock is not permitted");
    // `reject` discards the operation of the supplied value and
    // names the operation the target actually performed.
    assert_eq!(fault.op, Some(lm_abi::OP_CLOCK_NOW));
}

/// A wildcard arm can deny. It holds a `Request` and no reply type,
/// so `answer` is not available there.
#[test]
fn a_wildcard_arm_can_deny_a_request() {
    let source = "def child(): Int with Clock.Now\n  sys.clock.now()\nend\n\
        def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(child, args: ())\n  \
        case vm.drive()\n  in Asked(request)\n    \
        case request\n    in Call(Io.Print, call, (_,))\n      vm.answer(call, ())\n    \
        in _\n      vm.reject(request, Fault.denied(\"denied\"))\n    end\n    \
        case vm.run()\n    in Done(_) then \"done\"\n    in Fault(f) then f.code()\n    end\n  \
        in Done(_) then \"early-done\"\n  in Fault(_) then \"early-fault\"\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"PolicyDenied\")");
}

#[test]
fn fault_declares_no_other_constructor() {
    let source = "f = Fault.overflow(\"x\")\nf.code()\n";
    assert_eq!(code_of(source), "E1026");
}

#[test]
fn a_denial_reason_must_be_a_string() {
    let source = "f = Fault.denied(3)\nf.code()\n";
    assert_eq!(code_of(source), "E1004");
}

#[test]
fn a_denied_fault_takes_its_reason_by_label() {
    let source = "Fault.denied(reason: \"no\").code()\n";
    assert_eq!(runs(source), "Done(\"PolicyDenied\")");
}

#[test]
fn dispatch_applies_the_controlled_table() {
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Now\n    sys.clock.now() + 1\n  end, args: ())\n  \
        vm.table().mock(Clock.Now, do ||: Int 10 end)\n  \
        case vm.drive()\n  in Asked(q)\n    \
        vm.dispatch(q)\n    \
        case vm.run()\n    in Done(v) then v\n    in Fault(_) then 0 - 1\n    end\n  \
        in Done(_) then 0 - 2\n  in Fault(_) then 0 - 3\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(11)");
}

#[test]
fn drive_receives_a_passed_descendant_request() {
    let source = "def drive_loop(vm: Vm[Int], mut seen: [String]): Int with Vm\n  \
        loop do\n    \
        case vm.drive()\n    in Asked(q)\n      \
        case q\n      in Call(Io.Print, call, (text,))\n        \
        seen.push(text)\n        vm.answer(call, ())\n      \
        in _\n        vm.dispatch(q)\n      end\n    \
        in Done(value)\n      return seen.len() * 10 + value\n    \
        in Fault(_)\n      return 0 - 1\n    end\n  end\nend\n\
        inner = do ||: Int with Vm, Io.Print\n  \
        sys.io.print(\"from A\")\n  \
        b = sys.vm.Vm().from_fn(do ||: Int with Io.Print\n    \
        sys.io.print(\"from B\")\n    7\n  end, args: ())\n  \
        b.table().pass(Io.Print)\n  \
        case b.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\n\
        a = sys.vm.Vm().from_fn(inner, args: ())\n\
        a.table().pass(Vm)\n\
        a.table().pass(Io.Print)\n\
        seen: [String] = []\n\
        drive_loop(a, seen)\n";
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the routed request program runs");
    assert_eq!(out, "Done(27)");
    assert!(host.borrow().printed.is_empty());
}

#[test]
fn routed_dispatch_continues_after_the_driver_table() {
    let source = r#"
def drive_all(vm: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q) then vm.dispatch(q)
    in Done(value)
      return value
    in Fault(_)
      return 0 - 1
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
    sys.io.print("from B")
    7
  end, args: ())
  b.table().pass(Io.Print)
  case b.run()
  in Done(value) then value
  in Fault(_) then 0 - 3
  end
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)
drive_all(a)
"#;
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the routed dispatch program runs");
    assert_eq!(out, "Done(7)");
    assert_eq!(host.borrow().printed, vec!["from B"]);
}

#[test]
fn routed_reject_faults_the_performing_descendant() {
    let source = r#"
def reject_print(vm: Vm[String], source_fault: Fault): String with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, _, (_,))
        vm.reject(q, source_fault)
      in _
        vm.dispatch(q)
      end
    in Done(value)
      return value
    in Fault(_)
      return "outer fault"
    end
  end
end

blocked = sys.vm.Vm().from_fn(do || with Io.Print
  sys.io.print("blocked")
end, args: ())

case blocked.run()
in Done(_) then "no source fault"
in Fault(source_fault)
  inner = do ||: String with Vm, Io.Print
    b = sys.vm.Vm().from_fn(do ||: String with Io.Print
      sys.io.print("from B")
      "done"
    end, args: ())
    b.table().pass(Io.Print)
    case b.run()
    in Done(value) then value
    in Fault(fault) then fault.code()
    end
  end

  a = sys.vm.Vm().from_fn(inner, args: ())
  a.table().pass(Vm)
  a.table().pass(Io.Print)
  reject_print(a, source_fault)
end
"#;
    let (out, host) = run_world("t.lm", source, &["Vm"], VmConfig::default())
        .expect("the routed rejection program runs");
    assert_eq!(out, "Done(\"PolicyDenied\")");
    assert!(host.borrow().printed.is_empty());
}

#[test]
fn every_ancestor_still_authorizes_a_routed_request() {
    let source = r#"
def drive_without_print(vm: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, _, (_,))
        return 0 - 2
      in _ then vm.dispatch(q)
      end
    in Done(value)
      return value
    in Fault(_)
      return 0 - 3
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
    sys.io.print("from B")
    7
  end, args: ())
  b.table().pass(Io.Print)
  case b.run()
  in Done(value) then value
  in Fault(_) then 0 - 1
  end
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
drive_without_print(a)
"#;
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the ancestor denial program runs");
    assert_eq!(out, "Done(-1)");
    assert!(host.borrow().printed.is_empty());
}

#[test]
fn an_ancestor_mock_resolves_before_its_driver() {
    let source = r#"
def drive_without_clock(vm: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Clock.Now, _, ())
        return 0 - 1
      in _
        vm.dispatch(q)
      end
    in Done(value)
      return value
    in Fault(_)
      return 0 - 2
    end
  end
end

inner = do ||: Int with Vm, Clock.Now
  b = sys.vm.Vm().from_fn(do ||: Int with Clock.Now
    sys.clock.now()
  end, args: ())
  b.table().pass(Clock.Now)
  case b.run()
  in Done(value) then value
  in Fault(_) then 0 - 4
  end
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().mock(Clock.Now, do ||: Int 9 end)
drive_without_clock(a)
"#;
    assert_eq!(allowed(source, &["Vm"]), "Done(9)");
}

#[test]
fn a_routed_token_rejects_another_surface_machine() {
    let source = r#"
def answer_through_wrong_vm(vm: Vm[Int], wrong: Vm[Int]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (_,))
        wrong.answer(call, ())
        return 1
      in _
        vm.dispatch(q)
      end
    in Done(_)
      return 0 - 3
    in Fault(_)
      return 0 - 4
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
    sys.io.print("from B")
    7
  end, args: ())
  b.table().pass(Io.Print)
  case b.run()
  in Done(value) then value
  in Fault(_) then 0 - 1
  end
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)
c = sys.vm.Vm().from_fn(do ||: Int 0 end, args: ())
answer_through_wrong_vm(a, c)
"#;
    assert_eq!(
        allowed(source, &["Vm", "Io.Print"]),
        "Fault(InvalidRequestToken)"
    );
}

/// The nested control edge carries `step` as well as `run`. The
/// driver of the surface receives the descendant request either way.
#[test]
fn a_nested_step_surfaces_a_descendant_request() {
    let source = r#"
def drive_loop(vm: Vm[Int], mut seen: [String]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (text,))
        seen.push(text)
        vm.answer(call, ())
      in _ then vm.dispatch(q)
      end
    in Done(value)
      return seen.len() * 10 + value
    in Fault(_)
      return 0 - 1
    end
  end
end

def step_all(b: Vm[Int]): Int with Vm
  loop do
    case b.step()
    in Ran     then ()
    in Waiting then ()
    in Done(value)
      return value
    in Fault(_)
      return 0 - 3
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  sys.io.print("from A")
  b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
    sys.io.print("from B")
    7
  end, args: ())
  b.table().pass(Io.Print)
  step_all(b)
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)
seen: [String] = []
drive_loop(a, seen)
"#;
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the nested step program runs");
    assert_eq!(out, "Done(27)");
    assert!(host.borrow().printed.is_empty());
}

/// Two drivers stand above one machine. The policy walk stops at the
/// first one, so the outer driver never sees the request.
#[test]
fn the_nearest_driver_receives_a_descendant_request() {
    let source = r#"
def drive_loop(vm: Vm[Int], mut seen: [String]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (text,))
        seen.push(text)
        vm.answer(call, ())
      in _ then vm.dispatch(q)
      end
    in Done(value)
      return seen.len() * 10 + value
    in Fault(_)
      return 0 - 1
    end
  end
end

def a_drives_b(b: Vm[Int]): Int with Vm
  loop do
    case b.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (_,)) then b.answer(call, ())
      in _                          then b.dispatch(q)
      end
    in Done(value)
      return value
    in Fault(_)
      return 0 - 3
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  sys.io.print("from A")
  b = sys.vm.Vm().from_fn(do ||: Int with Io.Print
    sys.io.print("from B")
    7
  end, args: ())
  b.table().pass(Io.Print)
  a_drives_b(b)
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)
seen: [String] = []
drive_loop(a, seen)
"#;
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the two-driver program runs");
    // The outer driver captured the print of A alone. The inner
    // driver answered the print of B, so neither reached the host.
    assert_eq!(out, "Done(17)");
    assert!(host.borrow().printed.is_empty());
}

/// Routing is transitive. A driver receives a request that passed two
/// machines below it.
#[test]
fn a_driver_receives_a_request_from_two_levels_below() {
    let source = r#"
def drive_loop(vm: Vm[Int], mut seen: [String]): Int with Vm
  loop do
    case vm.drive()
    in Asked(q)
      case q
      in Call(Io.Print, call, (text,))
        seen.push(text)
        vm.answer(call, ())
      in _ then vm.dispatch(q)
      end
    in Done(value)
      return seen.len() * 10 + value
    in Fault(_)
      return 0 - 1
    end
  end
end

inner = do ||: Int with Vm, Io.Print
  sys.io.print("from A")
  b = sys.vm.Vm().from_fn(do ||: Int with Vm, Io.Print
    sys.io.print("from B")
    c = sys.vm.Vm().from_fn(do ||: Int with Io.Print
      sys.io.print("from C")
      7
    end, args: ())
    c.table().pass(Io.Print)
    case c.run()
    in Done(value) then value
    in Fault(_) then 0 - 3
    end
  end, args: ())
  b.table().pass(Vm)
  b.table().pass(Io.Print)
  case b.run()
  in Done(value) then value
  in Fault(_) then 0 - 4
  end
end

a = sys.vm.Vm().from_fn(inner, args: ())
a.table().pass(Vm)
a.table().pass(Io.Print)
seen: [String] = []
drive_loop(a, seen)
"#;
    let (out, host) = run_world("t.lm", source, &["Vm", "Io.Print"], VmConfig::default())
        .expect("the three-level program runs");
    assert_eq!(out, "Done(37)");
    assert!(host.borrow().printed.is_empty());
}

#[test]
fn run_step_and_drive_agree_on_one_program() {
    let program = "do || with Clock.Now\n    \
        total = 0\n    i = 0\n    while i < 3\n      \
        total = total + sys.clock.now()\n      i = i + 1\n    end\n    total\n  end";
    // run() with a mocked clock.
    let by_run = format!(
        "def go(): Int with Vm\n  vm = sys.vm.Vm().from_fn({program}, args: ())\n  \
         vm.table().mock(Clock.Now, do ||: Int 5 end)\n  \
         case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n"
    );
    // step() to the terminal with the same mock.
    let by_step = format!(
        "def go(): Int with Vm\n  vm = sys.vm.Vm().from_fn({program}, args: ())\n  \
         vm.table().mock(Clock.Now, do ||: Int 5 end)\n  \
         guard = 0\n  \
         while guard < 100000\n    guard = guard + 1\n    \
         case vm.step()\n    in Ran then ()\n    in Waiting then ()\n    \
         in Done(v)\n      return v\n    in Fault(_)\n      return 0 - 1\n    end\n  end\n  \
         0 - 2\nend\ngo()\n"
    );
    // drive() with manual answers.
    let by_drive = format!(
        "def go(): Int with Vm\n  vm = sys.vm.Vm().from_fn({program}, args: ())\n  \
         guard = 0\n  \
         while guard < 100000\n    guard = guard + 1\n    \
         case vm.drive()\n    in Asked(q)\n      \
         case q\n      in Call(Clock.Now, call, ()) then vm.answer(call, 5)\n      \
         in _ then vm.dispatch(q)\n      end\n    \
         in Done(v)\n      return v\n    in Fault(_)\n      return 0 - 1\n    end\n  end\n  \
         0 - 2\nend\ngo()\n"
    );
    assert_eq!(allowed(&by_run, &["Vm"]), "Done(15)");
    assert_eq!(allowed(&by_step, &["Vm"]), "Done(15)");
    assert_eq!(allowed(&by_drive, &["Vm"]), "Done(15)");
}

#[test]
fn waiting_state_appears_through_step_and_completes() {
    // Clock.Sleep goes through the asynchronous completion channel of
    // the recording host: the accepting step reports Waiting, a poll
    // completes it, and the machine continues to its terminal.
    let source = "def go(): Int with Vm, Clock.Sleep\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Sleep\n    \
        sys.clock.sleep(5)\n    27\n  end, args: ())\n  \
        vm.table().pass(Clock.Sleep)\n  \
        waits = 0\n  \
        guard = 0\n  \
        while guard < 100000\n    guard = guard + 1\n    \
        case vm.step()\n    in Ran then ()\n    in Waiting\n      waits = waits + 1\n    \
        in Done(v)\n      return v * 100 + waits\n    in Fault(_)\n      return 0 - 1\n    end\n  end\n  \
        0 - 2\nend\ngo()\n";
    // At least one Waiting event is observed before completion.
    let out = allowed(source, &["Vm", "Clock.Sleep"]);
    let value: i64 = out
        .trim_start_matches("Done(")
        .trim_end_matches(')')
        .parse()
        .expect("an integer outcome");
    assert_eq!(value / 100, 27);
    assert!(value % 100 >= 1, "no Waiting event was observed: {out}");
    // run() waits out the completion without holder involvement.
    let source = "def go(): Int with Vm, Clock.Sleep\n  \
        vm = sys.vm.Vm().from_fn(do || with Clock.Sleep\n    \
        sys.clock.sleep(5)\n    27\n  end, args: ())\n  \
        vm.table().pass(Clock.Sleep)\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm", "Clock.Sleep"]), "Done(27)");
}

#[test]
fn terminal_results_cross_the_boundary_as_a_copy() {
    // A frozen list crosses.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do ||\n    xs = [1, 2, 3]\n    xs.freeze()\n  end, args: ())\n  \
        case vm.run()\n  in Done(xs) then xs.len()\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(3)");
    // A mutable list crosses as a mutable copy, so the holder writes
    // into the copy it received.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do ||\n    [1, 2, 3]\n  end, args: ())\n  \
        case vm.run()\n  in Done(xs)\n    xs.push(4)\n    xs.len()\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(4)");
    // A holder-local value still converts the machine to
    // Fault(UnsendableValue).
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do ||: EmptyVm with Vm\n    sys.vm.Vm()\n  end, args: ())\n  \
        vm.table().pass(Vm)\n  \
        case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"UnsendableValue\")");
    // The conversion is sticky: a second observation returns the
    // same stable code.
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do ||: EmptyVm with Vm\n    sys.vm.Vm()\n  end, args: ())\n  \
        vm.table().pass(Vm)\n  \
        first = case vm.run()\n  in Done(_) then \"done\"\n  in Fault(f) then f.code()\n  end\n  \
        case vm.run()\n  in Done(_) then first\n  in Fault(f2) then f2.code()\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"UnsendableValue\")");
}

#[test]
fn program_captures_and_arguments_copy_at_the_loader_boundary() {
    // A mutable list capture copies at the load. A later write into
    // the source misses the copy the child machine holds.
    let source = "def go(): Int with Vm\n  \
        xs = [1]\n  \
        vm = sys.vm.Vm().from_fn(do ||: Int\n    xs.len()\n  end, args: ())\n  \
        xs.push(2)\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(1)");
    // A frozen capture crosses the same way.
    let source = "def go(): Int with Vm\n  \
        xs = [1, 2]\n  xs.freeze()\n  \
        vm = sys.vm.Vm().from_fn(do ||\n    xs.len()\n  end, args: ())\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then 0 - 1\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(2)");
    // Arguments transfer through the control envelope, strings
    // included.
    let source = "def go(): String with Vm\n  \
        vm = sys.vm.Vm().from_fn(do |a: Int, b: String|: String\n    \"{b}{a}\"\n  end, args: (42, \"x\"))\n  \
        case vm.run()\n  in Done(v) then v\n  in Fault(_) then \"fault\"\n  end\nend\ngo()\n";
    assert_eq!(allowed(source, &["Vm"]), "Done(\"x42\")");
}

#[test]
fn nested_towers_stay_off_the_rust_stack() {
    // A 60-level tower runs on a small Rust stack.
    let source = "def tower(n: Int): Int with Vm\n  \
        if n <= 0\n    41\n  else\n    \
        vm = sys.vm.Vm().from_fn(do || with Vm\n      tower(n - 1)\n    end, args: ())\n    \
        vm.table().pass(Vm)\n    \
        case vm.run()\n    in Done(v) then v\n    in Fault(_) then 0 - 1\n    end\n  \
        end\nend\ntower(60) + 1\n";
    let out = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(move || allowed(source, &["Vm"]))
        .expect("thread starts")
        .join()
        .expect("no Rust stack growth with nested VM depth");
    assert_eq!(out, "Done(42)");
}

#[test]
fn read_line_reply_uses_the_pinned_core_result() {
    let source = "def go(): String with Io.ReadLine\n  \
        case sys.io.read_line()\n  in Ok(line)\n    \
        case line\n    in Some(text) then text\n    in None then \"<eof>\"\n    end\n  \
        in Err(e) then e.message()\n  end\nend\ngo()\n";
    let (out, host) = run_world("t.lm", source, &["Io.ReadLine"], VmConfig::default()).unwrap();
    assert_eq!(out, "Done(\"<eof>\")");
    host.borrow_mut().input.push("hello".to_string());
    let (out, _) = {
        // A fresh world with one queued line.
        let bytes = lm_testkit::compile_to_bytes("t.lm", source).unwrap();
        let loaded = lm_vm::load_bytes(&bytes).unwrap();
        let host = std::rc::Rc::new(std::cell::RefCell::new(lm_vm::RecordingHost::new(1)));
        host.borrow_mut().input.push("hello".to_string());
        let mut world = lm_vm::World::new(&loaded, VmConfig::default(), Box::new(host.clone()));
        world.allow("Io.ReadLine").unwrap();
        let outcome = world.run_root();
        (world.show_outcome(&outcome), host)
    };
    assert_eq!(out, "Done(\"hello\")");
}

#[test]
fn rand_int_is_deterministic_and_validated() {
    let source = "def go(): Bool with Rand.Int\n  \
        a = sys.rand.int(0, 10)\n  \
        b = sys.rand.int(0, 10)\n  \
        a >= 0 and a < 10 and b >= 0 and b < 10\nend\ngo()\n";
    assert_eq!(allowed(source, &["Rand.Int"]), "Done(true)");
    let source = "def go(): Int with Rand.Int\n  sys.rand.int(5, 5)\nend\ngo()\n";
    assert_eq!(allowed(source, &["Rand.Int"]), "Fault(HostFault)");
}

#[test]
fn week4_examples_have_checked_output() {
    let read = |path: &str| {
        std::fs::read_to_string(lm_testkit::repo_root().join(path)).expect("example reads")
    };
    let (out, host) = run_world(
        "hello.lm",
        &read("examples/04-effects/hello.lm"),
        &["Io.Print"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(())");
    assert_eq!(host.borrow().printed, vec!["Hello Ada!\n"]);
    assert_eq!(
        run_allowed(
            "blocked.lm",
            &read("examples/04-effects/blocked.lm"),
            &["Vm"]
        )
        .unwrap(),
        "Done(\"PolicyDenied\")"
    );
    assert_eq!(
        run_allowed(
            "mock-clock.lm",
            &read("examples/04-effects/mock-clock.lm"),
            &["Vm"]
        )
        .unwrap(),
        "Done(123)"
    );
    let (out, host) = run_world(
        "manual-drive.lm",
        &read("examples/04-effects/manual-drive.lm"),
        &["Vm"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(([\"tick\\n\"], 123))");
    // The prints were captured by the holder, not the host.
    assert_eq!(host.borrow().printed, Vec::<String>::new());
}

#[test]
fn week4_examples_compile_twice_to_identical_bytes() {
    for example in [
        "examples/04-effects/hello.lm",
        "examples/04-effects/blocked.lm",
        "examples/04-effects/mock-clock.lm",
        "examples/04-effects/manual-drive.lm",
    ] {
        let source = std::fs::read_to_string(lm_testkit::repo_root().join(example)).unwrap();
        let a = lm_bytecode::encode(&compile_text(example, &source).unwrap());
        let b = lm_bytecode::encode(&compile_text(example, &source).unwrap());
        assert_eq!(a, b, "bytecode bytes differ for {example}");
    }
}
