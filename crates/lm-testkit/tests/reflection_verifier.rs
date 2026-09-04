//! Verifier checks for opaque reflection descriptors.

use lm_bytecode::{BcType, Func, Instr, Module};

fn type_index(module: &Module, ty: BcType) -> u32 {
    module
        .types
        .iter()
        .position(|candidate| *candidate == ty)
        .expect("the core primitive type exists") as u32
}

fn descriptor_type(module: &Module, class: u32) -> u32 {
    module
        .types
        .iter()
        .position(|ty| *ty == BcType::Class(class))
        .expect("the core descriptor type exists") as u32
}

fn add_function(module: &mut Module, params: Vec<u32>, ret: u32, block: Vec<Instr>) {
    module.funcs.push(Func {
        name: "forged descriptor access".to_string(),
        type_params: 0,
        effect_params: 0,
        param_names: (0..params.len()).map(|index| format!("p{index}")).collect(),
        param_muts: vec![false; params.len()],
        local_types: params.clone(),
        params,
        ret,
        row: Vec::new(),
        captures: Vec::new(),
        blocks: vec![block],
    });
    module.func_bounds.push(Vec::new());
}

fn assert_rejected(module: &Module, needle: &str) {
    let error = lm_verify::verify_module(module).expect_err("the forged descriptor code verifies");
    assert!(error.message.contains(needle), "{error}");
}

#[test]
fn bytecode_cannot_construct_a_module_descriptor() {
    let mut module = lm_hir::core_image();
    let class = module.core_roles[lm_bytecode::corepin::ROLE_MODULE_CODE];
    let ty = descriptor_type(&module, class);
    add_function(
        &mut module,
        Vec::new(),
        ty,
        vec![Instr::New(class), Instr::Return],
    );
    assert_rejected(&module, "New cannot allocate a native core class");
}

#[test]
fn bytecode_cannot_write_a_module_descriptor_field() {
    let mut module = lm_hir::core_image();
    let class = module.core_roles[lm_bytecode::corepin::ROLE_MODULE_CODE];
    let ty = descriptor_type(&module, class);
    let int = type_index(&module, BcType::Int);
    let unit = type_index(&module, BcType::Unit);
    add_function(
        &mut module,
        vec![ty, int],
        unit,
        vec![
            Instr::LoadLocal(0),
            Instr::LoadLocal(1),
            Instr::StoreField(0),
            Instr::ConstUnit,
            Instr::Return,
        ],
    );
    assert_rejected(&module, "a reflection descriptor has no visible fields");
}

#[test]
fn bytecode_cannot_read_a_module_descriptor_field() {
    let mut module = lm_hir::core_image();
    let class = module.core_roles[lm_bytecode::corepin::ROLE_MODULE_CODE];
    let ty = descriptor_type(&module, class);
    let unit = type_index(&module, BcType::Unit);
    add_function(
        &mut module,
        vec![ty],
        unit,
        vec![
            Instr::LoadLocal(0),
            Instr::LoadField(0),
            Instr::Pop,
            Instr::ConstUnit,
            Instr::Return,
        ],
    );
    assert_rejected(&module, "a reflection descriptor has no visible fields");
}
