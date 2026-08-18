use lm_bytecode::Instr;
use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

#[test]
fn a_small_expression_body_inlines() {
    let source = "def add1(n: Int): Int\n  n + 1\nend\nadd1(41)\n";
    let module = compile_text("inline.lm", source).expect("the program compiles");
    let target = module
        .funcs
        .iter()
        .position(|func| func.name == "add1")
        .expect("add1 exists") as u32;
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .all(|instr| { !matches!(instr, Instr::Call(func) if *func == target) }));
    let result = run_text("inline.lm", source, VmConfig::default()).unwrap();
    assert_eq!(result, "Done(42)");
}

#[test]
fn a_repeated_parameter_prevents_inlining() {
    let source = "def twice(n: Int): Int\n  n + n\nend\ntwice(21)\n";
    let module = compile_text("inline.lm", source).expect("the program compiles");
    let target = module
        .funcs
        .iter()
        .position(|func| func.name == "twice")
        .expect("twice exists") as u32;
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .any(|instr| matches!(instr, Instr::Call(func) if *func == target)));
}
