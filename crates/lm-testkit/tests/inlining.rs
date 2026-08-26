use lm_bytecode::Instr;
use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

#[test]
fn a_small_expression_body_inlines() {
    let source = "def add1(n: Int): Int\n  n + 1\nend\nadd1(41)\n";
    let module = compile_text("inline.lm", source).expect("the program compiles");
    assert!(module.funcs.iter().all(|func| func.name != "add1"));
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .all(|instr| !matches!(instr, Instr::Call(_))));
    let result = run_text("inline.lm", source, VmConfig::default()).unwrap();
    assert_eq!(result, "Done(42)");
}

#[test]
fn a_small_generic_body_inlines() {
    let source = "def id[T](value: T): T\n  value\nend\nid(42)\n";
    let module = compile_text("inline_generic.lm", source).expect("the program compiles");
    assert!(module.funcs.iter().all(|func| func.name != "id"));
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .all(|instruction| !matches!(instruction, Instr::CallG { .. })));
    assert_eq!(
        run_text("inline_generic.lm", source, VmConfig::default()).unwrap(),
        "Done(42)"
    );
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

#[test]
fn a_trivial_call_chain_inlines() {
    let source = "def add1(n: Int): Int\n  n + 1\nend\n\
                  def next(n: Int): Int\n  add1(n)\nend\nnext(41)\n";
    let module = compile_text("inline_chain.lm", source).expect("the program compiles");
    let targets: Vec<u32> = module
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, func)| matches!(func.name.as_str(), "add1" | "next"))
        .map(|(index, _)| index as u32)
        .collect();
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .all(|instr| !matches!(instr, Instr::Call(func) if targets.contains(func))));
    assert_eq!(
        run_text("inline_chain.lm", source, VmConfig::default()).unwrap(),
        "Done(42)"
    );
}

#[test]
fn a_recursive_expression_body_stays_a_call() {
    let source = "def again(n: Int): Int\n  again(n)\nend\nagain(0)\n";
    let module = compile_text("inline_recursive.lm", source).expect("the program compiles");
    let target = module
        .funcs
        .iter()
        .position(|func| func.name == "again")
        .expect("again exists") as u32;
    let entry = &module.funcs[module.entry as usize];
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .any(|instr| matches!(instr, Instr::Call(func) if *func == target)));
}
