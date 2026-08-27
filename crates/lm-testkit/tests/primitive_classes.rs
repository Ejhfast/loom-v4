use lm_bytecode::{
    corepin::{ROLE_BOOL, ROLE_INT},
    Instr,
};
use lm_testkit::{compile_module_text, run_text};
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

fn primitive_method(module: &lm_bytecode::Module, role: usize, name: &str) -> (u32, u32) {
    let class = module.core_roles[role];
    assert_ne!(class, lm_bytecode::NO_ROLE);
    module.classes[class as usize]
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == name)
        .copied()
        .expect("the primitive method exists")
}

#[test]
fn int_abs_uses_the_final_core_method_table() {
    let source = "(-42).abs()\n";
    let module = compile_module_text("int_abs.lm", source).expect("the program compiles");
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
    let mut module =
        compile_module_text("int_virtual.lm", "1.abs()\n").expect("the program compiles");
    let (_, selector, _) = int_method(&module);
    module.funcs[module.entry as usize].blocks = vec![vec![
        Instr::ConstInt(-7),
        Instr::CallVirtual { selector, argc: 0 },
        Instr::Return,
    ]];
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let artifact = lm_testkit::artifact_with_core_from_module("int_virtual", module)
        .expect("the artifact builds");
    let (arena, namespace) = lm_testkit::publish_artifact(&artifact).expect("the artifact loads");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(7)");
}

#[test]
fn the_verifier_rejects_a_nonfinal_int_role() {
    let mut module = compile_module_text("int_role.lm", "1\n").expect("the program compiles");
    let class = module.core_roles[ROLE_INT];
    module.conformances.retain(|item| item.class != class);
    module.classes[class as usize].is_final = false;
    let error = lm_verify::verify_module(&module).expect_err("the role rejects");
    assert!(
        error
            .message
            .contains("core role `Int` does not name a final stateless class"),
        "{error}"
    );
}

#[test]
fn operators_inline_to_existing_instructions() {
    let source = "(-5, 8 + 3, 8 - 3, 8 * 3, 8 / 3, 8 % 3, \
                  1 == 1, 1 != 2, 1 < 2, 1 <= 1, 2 > 1, 2 >= 2, \
                  not false, true == false, true != false)\n";
    let module = compile_module_text("operators.lm", source).expect("the program compiles");
    let primitive_funcs: Vec<u32> = [ROLE_INT, ROLE_BOOL]
        .iter()
        .flat_map(|role| {
            let class = module.core_roles[*role];
            module.classes[class as usize]
                .methods
                .iter()
                .map(|(_, func)| *func)
        })
        .collect();
    let instructions: Vec<Instr> = module.funcs[module.entry as usize]
        .blocks
        .iter()
        .flatten()
        .copied()
        .collect();
    assert!(instructions.iter().all(|instr| {
        !matches!(instr, Instr::Call(func) if primitive_funcs.contains(func))
            && !matches!(instr, Instr::CallVirtual { .. })
    }));
    for expected in [
        Instr::Neg,
        Instr::Add,
        Instr::Sub,
        Instr::Mul,
        Instr::Div,
        Instr::Rem,
        Instr::EqInt,
        Instr::NeInt,
        Instr::LtInt,
        Instr::LeInt,
        Instr::GtInt,
        Instr::GeInt,
        Instr::Not,
        Instr::EqBool,
        Instr::NeBool,
    ] {
        assert!(instructions.contains(&expected), "missing {expected:?}");
    }
    assert_eq!(
        run_text("operator_method.lm", "1.__add__(2)\n", VmConfig::default()).unwrap(),
        "Done(3)"
    );
}

#[test]
fn a_bool_tag_supports_verified_virtual_dispatch() {
    let mut module =
        compile_module_text("bool_virtual.lm", "not false\n").expect("the program compiles");
    let (selector, _) = primitive_method(&module, ROLE_BOOL, "__not__");
    module.funcs[module.entry as usize].blocks = vec![vec![
        Instr::ConstBool(false),
        Instr::CallVirtual { selector, argc: 0 },
        Instr::Return,
    ]];
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let artifact = lm_testkit::artifact_with_core_from_module("bool_virtual", module)
        .expect("the artifact builds");
    let (arena, namespace) = lm_testkit::publish_artifact(&artifact).expect("the artifact loads");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(true)");
}

#[test]
fn the_verifier_rejects_a_stateful_bool_role() {
    let mut module = compile_module_text("bool_role.lm", "true\n").expect("the program compiles");
    let class = module.core_roles[ROLE_BOOL];
    let int_ty = module
        .types
        .iter()
        .position(|ty| *ty == lm_bytecode::BcType::Int)
        .expect("the Int type exists") as u32;
    module.classes[class as usize]
        .fields
        .push(("bad".to_string(), int_ty));
    let error = lm_verify::verify_module(&module).expect_err("the role rejects");
    assert!(
        error
            .message
            .contains("core role `Bool` does not name a final stateless class"),
        "{error}"
    );
}
