use lm_bytecode::{corepin::ROLE_INT, Instr};
use lm_testkit::{compile_text, run_text};
use lm_vm::{Vm, VmConfig};

fn int_method(module: &lm_bytecode::Module) -> (u32, u32, u32) {
    let class = module.core_roles[ROLE_INT];
    assert_ne!(class, lm_bytecode::NO_ROLE);
    let (selector, func) = module.classes[class as usize]
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == "abs")
        .copied()
        .expect("Int.abs exists");
    (class, selector, func)
}

#[test]
fn int_abs_uses_the_final_core_method_table() {
    let source = "(0 - 42).abs()\n";
    let module = compile_text("int_abs.lm", source).expect("the program compiles");
    let (class, _, func) = int_method(&module);
    assert!(module.classes[class as usize].is_final);
    let entry = &module.funcs[module.entry as usize];
    assert!(entry.blocks.iter().flatten().all(|instr| {
        !matches!(instr, Instr::Call(target) if *target == func)
            && !matches!(instr, Instr::CallVirtual { .. })
    }));
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .any(|instr| matches!(instr, Instr::Neg)));
    assert_eq!(
        run_text("int_abs.lm", source, VmConfig::default()).unwrap(),
        "Done(42)"
    );
}

#[test]
fn an_int_tag_supports_verified_virtual_dispatch() {
    let mut module = compile_text("int_virtual.lm", "1.abs()\n").expect("the program compiles");
    let (_, selector, _) = int_method(&module);
    module.funcs[module.entry as usize].blocks = vec![vec![
        Instr::ConstInt(-7),
        Instr::CallVirtual { selector, argc: 0 },
        Instr::Return,
    ]];
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(7)");
}

#[test]
fn the_verifier_rejects_a_nonfinal_int_role() {
    let mut module = compile_text("int_role.lm", "1\n").expect("the program compiles");
    let class = module.core_roles[ROLE_INT];
    module.classes[class as usize].is_final = false;
    let error = lm_verify::verify_module(&module).expect_err("the role rejects");
    assert!(
        error
            .message
            .contains("core role `Int` does not name a final stateless class"),
        "{error}"
    );
}
