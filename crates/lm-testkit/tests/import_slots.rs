//! Artifact import-slot identity, verification, and loading.

use lm_bytecode::identity::module_identity;
use lm_bytecode::{BcType, Func, Import, ImportKind, Instr, Module};

const PIN: [u8; 32] = [7u8; 32];

/// A module with one imported function and one entry that calls it.
/// The imported function carries a signature and no body.
fn importing_module(pin: [u8; 32]) -> Module {
    Module {
        strings: vec![],
        bytes: vec![],
        types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
        selectors: vec![],
        apps: vec![],
        interfaces: vec![],
        conformances: vec![],
        class_bounds: vec![],
        func_bounds: vec![vec![], vec![]],
        core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
        imports: vec![Import {
            module: "dep.math".to_string(),
            name: "add".to_string(),
            kind: ImportKind::Func,
            def: 0,
            hash: pin,
        }],
        slots: vec![],
        classes: vec![],
        funcs: vec![
            Func {
                name: "add".to_string(),
                param_names: vec!["value".to_string()],
                type_params: 0,
                effect_params: 0,
                params: vec![2],
                param_muts: vec![false],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![2],
                blocks: vec![],
            },
            Func {
                name: "<entry>".to_string(),
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(1), Instr::Call(0), Instr::Return]],
            },
        ],
        entry: 1,
        exports: vec![],
        bindings: vec![],
        debug: Vec::new(),
    }
}

/// An import slot takes the pinned interface hash as its identity, and
/// a caller covers the pin.
#[test]
fn an_import_slot_carries_the_pinned_identity() {
    let module = importing_module(PIN);
    let identity = module_identity(&module).expect("hashes");
    assert_eq!(identity.func_hashes[0], PIN);
    let other = importing_module([9u8; 32]);
    let other_identity = module_identity(&other).expect("hashes");
    assert_ne!(
        identity.func_hashes[1], other_identity.func_hashes[1],
        "the caller hash must cover the pinned interface hash"
    );
    assert_ne!(identity.semantic_hash, other_identity.semantic_hash);
}

/// An unbound unit verifies, but publication rejects its missing provider.
#[test]
fn an_unlinked_module_verifies_but_never_loads() {
    let module = importing_module(PIN);
    lm_verify::verify_module(&module).expect("an unlinked module verifies");
    let error =
        lm_testkit::unit_from_module(module).expect_err("publication admitted an unbound unit");
    assert!(
        error.contains("dependency") || error.contains("import"),
        "{error}"
    );
}

/// The artifact round-trips the import table and the export table.
#[test]
fn the_container_round_trips_imports_and_exports() {
    let mut module = importing_module(PIN);
    module.exports.push(lm_bytecode::Export {
        kind: lm_bytecode::ExportKind::Function,
        name: "<entry>".to_string(),
        def: 1,
        ctor: lm_bytecode::NO_CTOR,
        constant: None,
    });
    let bytes = lm_bytecode::encode(&module);
    let back = lm_bytecode::decode(&bytes).expect("decodes");
    assert_eq!(back, module);
}

/// A constant pin round-trips without one runtime definition.
#[test]
fn a_constant_pin_round_trips_without_a_definition() {
    let mut module = importing_module(PIN);
    module.imports.push(Import {
        module: "dep.values".to_string(),
        name: "LIMIT".to_string(),
        kind: ImportKind::Constant,
        def: lm_bytecode::NO_IMPORT_DEF,
        hash: [8; 32],
    });
    let bytes = lm_bytecode::encode(&module);
    let decoded = lm_bytecode::decode(&bytes).expect("the module decodes");
    assert_eq!(decoded.imports, module.imports);
    lm_verify::verify_module(&decoded).expect("the pin verifies");
}

/// An import slot outside the definition tables rejects at the
/// decoder, before any later pass reads it.
#[test]
fn an_out_of_range_import_rejects_at_the_decoder() {
    let mut module = importing_module(PIN);
    module.imports[0].def = 99;
    let bytes = lm_bytecode::encode(&module);
    assert_eq!(
        lm_bytecode::decode(&bytes),
        Err(lm_bytecode::DecodeError::BadImport)
    );
}

/// Two slots cannot claim one definition: the map from slot to
/// definition must stay injective.
#[test]
fn two_slots_cannot_claim_one_definition() {
    let mut module = importing_module(PIN);
    let copy = module.imports[0].clone();
    module.imports.push(copy);
    let bytes = lm_bytecode::encode(&module);
    assert_eq!(
        lm_bytecode::decode(&bytes),
        Err(lm_bytecode::DecodeError::BadImport)
    );
    // The identity preflight repeats the rule for a hand-built module.
    assert!(module_identity(&module).is_err());
}

/// An imported function has no body and no extra local slot.
#[test]
fn an_imported_function_must_carry_no_body() {
    for mutate in [
        |m: &mut Module| m.funcs[0].blocks = vec![vec![Instr::ConstInt(1), Instr::Return]],
        |m: &mut Module| m.funcs[0].local_types = vec![2, 2],
    ] {
        let mut module = importing_module(PIN);
        mutate(&mut module);
        assert!(
            lm_verify::verify_module(&module).is_err(),
            "the verifier admitted an imported function with a body"
        );
    }
}

/// An imported closure body keeps its capture contract.
#[test]
fn an_imported_function_can_declare_captures() {
    let mut module = importing_module(PIN);
    module.funcs[0].captures = vec![2];
    module.funcs[1].blocks = vec![vec![Instr::ConstInt(1), Instr::Return]];
    lm_verify::verify_module(&module).expect("the capture contract verifies");
}

/// A method of an imported class must be imported too, and a local
/// class must not borrow an imported method function.
#[test]
fn a_class_and_its_methods_share_the_import_state() {
    let mut module = importing_module(PIN);
    module.types.push(BcType::Class(0));
    module.selectors.push("add".to_string());
    module.classes.push(lm_bytecode::BcClass {
        name: "Local".to_string(),
        parent_args: Vec::new(),
        key: "Local".to_string(),
        is_final: false,
        is_frozen: false,
        parent: lm_bytecode::NO_PARENT,
        type_params: 0,
        kind: lm_bytecode::BcClassKind::Normal,
        fields: vec![],
        field_defaults: vec![],
        own_start: 0,
        has_init: false,
        methods: vec![(0, 0)],
    });
    module.class_bounds.push(Vec::new());
    // Function 0 is imported, so a local class must not answer with it.
    module.funcs[0].params = vec![4];
    module.funcs[0].local_types = vec![4];
    let error = lm_verify::verify_module(&module).expect_err("the verifier admitted the module");
    assert!(error.message.contains("import state"), "{error:?}");
}
