//! Verifier checks for opaque reflection descriptors.

use lm_bytecode::{
    BcType, ExtendedInstr, Func, Instr, Module, ReflectionBases, ReflectionKind, ReflectionModule,
    ReflectionPattern,
};

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

fn intern_type(module: &mut Module, ty: BcType) -> u32 {
    if let Some(index) = module.types.iter().position(|candidate| *candidate == ty) {
        return index as u32;
    }
    let index = module.types.len() as u32;
    module.types.push(ty);
    index
}

fn add_function(module: &mut Module, params: Vec<u32>, ret: u32, block: Vec<Instr>) -> u32 {
    let function = module.funcs.len() as u32;
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
    function
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
fn bytecode_cannot_construct_any_reflection_descriptor() {
    for role in [
        lm_bytecode::corepin::ROLE_MODULE_CODE,
        lm_bytecode::corepin::ROLE_DECLARATION_CODE,
        lm_bytecode::corepin::ROLE_MEMBER_CODE,
        lm_bytecode::corepin::ROLE_OPEN_CODE,
    ] {
        let mut module = lm_hir::core_image();
        let class = module.core_roles[role];
        let ty = descriptor_type(&module, class);
        add_function(
            &mut module,
            Vec::new(),
            ty,
            vec![Instr::New(class), Instr::Return],
        );
        assert_rejected(&module, "New cannot allocate a");
    }
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

#[test]
fn a_verified_module_name_verifies() {
    let mut module = lm_hir::core_image();
    let class = module.core_roles[lm_bytecode::corepin::ROLE_VERIFIED_MODULE];
    let descriptor = descriptor_type(&module, class);
    let string = type_index(&module, BcType::Str);
    add_function(
        &mut module,
        vec![descriptor],
        string,
        vec![
            Instr::LoadLocal(0),
            Instr::Extended(ExtendedInstr::ReflectionName),
            Instr::Return,
        ],
    );
    lm_verify::verify_module(&module).expect("the verified module name verifies");
}

#[test]
fn an_open_descriptor_has_no_name() {
    let mut module = lm_hir::core_image();
    let class = module.core_roles[lm_bytecode::corepin::ROLE_OPEN_CODE];
    let descriptor = descriptor_type(&module, class);
    let string = type_index(&module, BcType::Str);
    add_function(
        &mut module,
        vec![descriptor],
        string,
        vec![
            Instr::LoadLocal(0),
            Instr::Extended(ExtendedInstr::ReflectionName),
            Instr::Return,
        ],
    );
    assert_rejected(&module, "reflection name needs a descriptor");
}

fn reflection_scope_module() -> (Module, u32, u32) {
    let mut module = lm_hir::core_image();
    let open = module.core_roles[lm_bytecode::corepin::ROLE_OPEN_CODE];
    let descriptor = descriptor_type(&module, open);
    let unit = type_index(&module, BcType::Unit);
    let variable = intern_type(&mut module, BcType::Var(0));
    let refined = intern_type(
        &mut module,
        BcType::Fn(vec![variable], vec![false], variable, Vec::new()),
    );
    let pattern = module.funcs.len() as u32;
    module.funcs.push(Func {
        name: "reflection pattern".to_string(),
        type_params: 1,
        effect_params: 0,
        params: vec![refined],
        param_muts: vec![false],
        ret: unit,
        row: Vec::new(),
        captures: Vec::new(),
        local_types: vec![refined],
        blocks: vec![vec![Instr::Unreachable]],
        param_names: vec!["value".to_string()],
    });
    module.func_bounds.push(vec![Vec::new()]);
    let function = module.funcs.len() as u32;
    module.funcs.push(Func {
        name: "reflection scope".to_string(),
        type_params: 0,
        effect_params: 0,
        params: vec![descriptor],
        param_muts: vec![false],
        ret: unit,
        row: Vec::new(),
        captures: Vec::new(),
        local_types: vec![descriptor],
        blocks: vec![
            vec![
                Instr::LoadLocal(0),
                Instr::Extended(ExtendedInstr::ReflectionRefine {
                    pattern: ReflectionPattern::new(ReflectionKind::Function, pattern).unwrap(),
                    fail: 1,
                }),
                Instr::Pop,
                Instr::Extended(ExtendedInstr::ReflectionEnd {
                    pattern,
                    bases: ReflectionBases::new(0, 0).unwrap(),
                }),
                Instr::ConstUnit,
                Instr::Return,
            ],
            vec![Instr::ConstUnit, Instr::Return],
        ],
        param_names: vec!["descriptor".to_string()],
    });
    module.func_bounds.push(Vec::new());
    (module, function, pattern)
}

#[test]
fn a_valid_reflection_scope_verifies() {
    let (module, _, _) = reflection_scope_module();
    lm_verify::verify_module(&module).expect("the reflection scope verifies");
}

#[test]
fn a_valid_class_descriptor_scope_verifies() {
    let (mut module, function, pattern) = reflection_scope_module();
    let variable = type_index(&module, BcType::Var(0));
    module.funcs[pattern as usize].params[0] = variable;
    module.funcs[pattern as usize].local_types[0] = variable;
    let Instr::Extended(ExtendedInstr::ReflectionRefine {
        pattern: instruction,
        ..
    }) = &mut module.funcs[function as usize].blocks[0][1]
    else {
        panic!("the fixture has a refinement instruction");
    };
    *instruction = ReflectionPattern::new(ReflectionKind::ClassDescriptor, pattern).unwrap();
    lm_verify::verify_module(&module).expect("the class descriptor scope verifies");
}

#[test]
fn a_reflection_scope_needs_valid_pattern_metadata() {
    let (mut module, _, pattern) = reflection_scope_module();
    let int = type_index(&module, BcType::Int);
    module.funcs[pattern as usize].params[0] = int;
    module.funcs[pattern as usize].local_types[0] = int;
    assert_rejected(&module, "invalid refined signature");

    let (mut module, function, _) = reflection_scope_module();
    let Instr::Extended(ExtendedInstr::ReflectionRefine { pattern, .. }) =
        &mut module.funcs[function as usize].blocks[0][1]
    else {
        panic!("the fixture has a refinement instruction");
    };
    *pattern = ReflectionPattern::new(ReflectionKind::Function, u32::MAX >> 3).unwrap();
    assert_rejected(&module, "reflection pattern out of range");

    let (mut module, function, pattern) = reflection_scope_module();
    let Instr::Extended(ExtendedInstr::ReflectionRefine {
        pattern: instruction,
        ..
    }) = &mut module.funcs[function as usize].blocks[0][1]
    else {
        panic!("the fixture has a refinement instruction");
    };
    *instruction = ReflectionPattern::new(ReflectionKind::ClassDescriptor, pattern).unwrap();
    assert_rejected(
        &module,
        "class descriptor refinement has invalid type metadata",
    );
}

#[test]
fn reflection_scopes_must_end_in_order() {
    let (mut module, function, _) = reflection_scope_module();
    let Instr::Extended(ExtendedInstr::ReflectionEnd { pattern, .. }) =
        &mut module.funcs[function as usize].blocks[0][3]
    else {
        panic!("the fixture has a reflection end instruction");
    };
    *pattern = function;
    assert_rejected(&module, "reflection scopes end out of order");
}

#[test]
fn a_refined_value_cannot_escape_its_scope() {
    let (mut module, function, _) = reflection_scope_module();
    module.funcs[function as usize].blocks[0][2] = Instr::ConstUnit;
    assert_rejected(&module, "reflection-scoped value escapes");
}

#[test]
fn reflected_provider_names_are_unique() {
    let mut module = lm_hir::core_image();
    module.reflections.push(ReflectionModule {
        name: "same.module".to_string(),
        semantic_hash: [1; 32],
    });
    module.reflections.push(ReflectionModule {
        name: "same.module".to_string(),
        semantic_hash: [1; 32],
    });
    assert_rejected(&module, "repeats module");
}
