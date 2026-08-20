//! Week-5 surface suites: the `sys` casing rule, the `use` keyword
//! with fixed-binding aliases, and the request pattern.

use lm_testkit::{compile_text, run_allowed, run_text, run_world};
use lm_vm::VmConfig;

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
// The sys casing rule.
// ---------------------------------------------------------------

#[test]
fn callable_sys_members_are_snake_case() {
    assert_eq!(
        allowed(
            "def f() with Io.Print\n  sys.io.print(\"x\\n\")\nend\nf()\n",
            &["Io.Print"]
        ),
        "Done(())"
    );
    // The multi-word mapping is mechanical: read_line -> ReadLine.
    assert_eq!(code_of("def f()\n  sys.io.read_line()\nend\n1\n"), "E1046");
}

#[test]
fn capitalized_callable_members_get_the_casing_rule() {
    let rendered = compile_text(
        "t.lm",
        "def f() with Io.Print\n  sys.io.Print(\"x\")\nend\n1\n",
    )
    .unwrap_err();
    assert!(rendered.starts_with("error[E1051]"), "{rendered}");
    assert!(rendered.contains("write `sys.io.print`"), "{rendered}");
    let rendered = compile_text(
        "t.lm",
        "def f() with Io.ReadLine\n  sys.io.ReadLine()\nend\n1\n",
    )
    .unwrap_err();
    assert!(rendered.contains("write `sys.io.read_line`"), "{rendered}");
}

#[test]
fn the_vm_constructor_keeps_its_capital() {
    assert_eq!(
        allowed(
            "def go(): Int with Vm\n  m = sys.vm.Vm().activate(do || 21 end, args: ())\n  \
             case m.run()\n  in Done(v) then v\n  in Fault(_) then 0\n  end\nend\ngo()\n",
            &["Vm"]
        ),
        "Done(21)"
    );
    // The snake spelling of the constructor is not a member.
    assert_eq!(
        code_of("def go() with Vm\n  sys.vm.vm()\nend\n1\n"),
        "E1051"
    );
}

#[test]
fn descriptors_keep_initial_capitals() {
    // Rows, policy targets, and --allow names are unchanged.
    let (out, host) = run_world(
        "t.lm",
        "def go(): Int with Vm, Io.Print\n  \
         m = sys.vm.Vm().activate(do || with Io.Print\n    sys.io.print(\"in\\n\")\n    7\n  \
         end, args: ())\n  m.table().pass(Io.Print)\n  \
         case m.run()\n  in Done(v) then v\n  in Fault(_) then 0\n  end\nend\ngo()\n",
        &["Vm", "Io.Print"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(7)");
    assert_eq!(host.borrow().printed, vec!["in\n".to_string()]);
}

// ---------------------------------------------------------------
// The `use` keyword: fixed-binding aliases.
// ---------------------------------------------------------------

#[test]
fn use_binds_a_sys_group() {
    assert_eq!(
        allowed(
            "use sys.vm\n\ndef go(): Int with Vm\n  \
             m = vm.Vm().activate(do || 21 end, args: ())\n  \
             case m.run()\n  in Done(v) then v\n  in Fault(_) then 0\n  end\nend\ngo()\n",
            &["Vm"]
        ),
        "Done(21)"
    );
    assert_eq!(
        allowed(
            "use sys.io\n\ndef f() with Io.Print\n  io.print(\"x\\n\")\nend\nf()\n",
            &["Io.Print"]
        ),
        "Done(())"
    );
}

#[test]
fn use_binds_the_vm_constructor() {
    assert_eq!(
        allowed(
            "use sys.vm.Vm\n\ndef go(): Int with Vm\n  \
             m = Vm().activate(do || 42 end, args: ())\n  \
             case m.run()\n  in Done(v) then v\n  in Fault(_) then 0\n  end\nend\ngo()\n",
            &["Vm"]
        ),
        "Done(42)"
    );
}

#[test]
fn use_binds_a_callable_member() {
    let (out, host) = run_world(
        "t.lm",
        "use sys.io.print\n\ndef f() with Io.Print\n  print(\"hello\\n\")\nend\nf()\n",
        &["Io.Print"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(())");
    assert_eq!(host.borrow().printed, vec!["hello\n".to_string()]);
    // The bound member is also a first-class operation value.
    assert_eq!(
        allowed(
            "use sys.io.print\n\ndef f() with Io.Print\n  p = print\n  p(\"x\\n\")\nend\nf()\n",
            &["Io.Print"]
        ),
        "Done(())"
    );
}

#[test]
fn a_use_aliased_perform_still_charges_the_row() {
    // The alias grants nothing: a perform through it still needs the
    // declared row.
    assert_eq!(
        code_of("use sys.io.print\n\ndef f()\n  print(\"x\")\nend\n1\n"),
        "E1046"
    );
    assert_eq!(
        code_of("use sys.clock.now\n\ndef f(): Int\n  now()\nend\n1\n"),
        "E1046"
    );
}

#[test]
fn a_use_aliased_perform_still_needs_policy() {
    // The alias grants no authority: the root policy still decides.
    assert_eq!(
        runs("use sys.io.print\n\ndef f() with Io.Print\n  print(\"x\")\nend\nf()\n"),
        "Fault(PolicyDenied)"
    );
}

#[test]
fn use_binding_of_a_group_is_not_a_value_or_callable() {
    assert_eq!(code_of("use sys.io\n\nx = io\n1\n"), "E1051");
    assert_eq!(code_of("use sys.io\n\nio(1)\n"), "E1051");
}

#[test]
fn use_rejects_non_fixed_paths() {
    // A single file carries no compile environment, so a module
    // import has no root to resolve against. Week 6 changed the
    // wording: the fix is a package, not a later week.
    let rendered = compile_text("t.lm", "use mathlib.matrix\n1\n").unwrap_err();
    assert!(rendered.starts_with("error[E1052]"), "{rendered}");
    assert!(rendered.contains("package"), "{rendered}");
    // `use sys` alone binds nothing.
    assert_eq!(code_of("use sys\n1\n"), "E1052");
    // Unknown group and unknown member.
    assert_eq!(code_of("use sys.nope\n1\n"), "E1052");
    assert_eq!(code_of("use sys.io.blast\n1\n"), "E1052");
    // A path with too many segments.
    assert_eq!(code_of("use sys.io.print.extra\n1\n"), "E1052");
    // The casing rule applies inside `use` paths.
    let rendered = compile_text("t.lm", "use sys.io.Print\n1\n").unwrap_err();
    assert!(rendered.contains("use sys.io.print"), "{rendered}");
}

#[test]
fn use_lines_come_first_and_bind_once() {
    // A `use` after a definition or statement is a parse error.
    assert_eq!(code_of("x = 1\nuse sys.io\nx\n"), "E1052");
    assert_eq!(code_of("def f(): Int\n  1\nend\nuse sys.io\n1\n"), "E1052");
    // One name binds once inside the `use` layer.
    assert_eq!(code_of("use sys.io.print\nuse sys.io.print\n1\n"), "E1052");
}

#[test]
fn use_bindings_sit_below_locals_and_module_definitions() {
    // A module function shadows the binding; no row is needed.
    assert_eq!(
        runs("use sys.io.print\n\ndef print(x: Int): Int\n  x\nend\nprint(3)\n"),
        "Done(3)"
    );
    // A local assignment declares a local; the binding does not make
    // the name assigned.
    assert_eq!(runs("use sys.clock.now\n\nnow = 5\nnow\n"), "Done(5)");
}

#[test]
fn use_alias_example_has_checked_output() {
    let text =
        std::fs::read_to_string(lm_testkit::repo_root().join("examples/04-effects/use-alias.lm"))
            .expect("example reads");
    let (out, host) = run_world(
        "use-alias.lm",
        &text,
        &["Io.Print", "Vm"],
        VmConfig::default(),
    )
    .unwrap();
    assert_eq!(out, "Done(42)");
    assert_eq!(host.borrow().printed, vec!["Hello Ada!\n".to_string()]);
}

// ---------------------------------------------------------------
// The request pattern.
// ---------------------------------------------------------------

#[test]
fn a_request_pattern_answers_through_its_call() {
    assert_eq!(
        allowed(
            "def go(): Int with Vm\n  \
             m = sys.vm.Vm().activate(do || with Clock.Now\n    sys.clock.now()\n  \
             end, args: ())\n  case m.drive()\n  in Asked(q)\n    \
             case q\n    in Call(Clock.Now, call, ())\n      m.answer(call, 99)\n      \
             case m.run()\n      in Done(v) then v\n      in Fault(_) then 0\n      end\n    \
             in _ then 0\n    end\n  in Done(v) then v\n  in Fault(_) then 0\n  end\nend\ngo()\n",
            &["Vm"]
        ),
        "Done(99)"
    );
}
