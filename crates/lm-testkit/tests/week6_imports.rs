//! Week-6 import slots inside the artifact: identity, verification,
//! and the loader rule.

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

/// An unlinked module verifies, so `lm build` can check it, and never
/// loads, because an import slot has no body to run.
#[test]
fn an_unlinked_module_verifies_but_never_loads() {
    let module = importing_module(PIN);
    lm_verify::verify_module(&module).expect("an unlinked module verifies");
    let error = lm_vm::load(module).expect_err("the loader admitted an unlinked module");
    assert!(
        error.message.contains("unresolved import slot"),
        "{error:?}"
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
    });
    let bytes = lm_bytecode::encode(&module);
    let back = lm_bytecode::decode(&bytes).expect("decodes");
    assert_eq!(back, module);
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

/// An imported function is a declaration: a body, a capture, or an
/// extra local slot rejects at the verifier.
#[test]
fn an_imported_function_must_carry_no_body() {
    for mutate in [
        |m: &mut Module| m.funcs[0].blocks = vec![vec![Instr::ConstInt(1), Instr::Return]],
        |m: &mut Module| m.funcs[0].captures = vec![2],
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
        methods: vec![(0, 0)],
    });
    module.class_bounds.push(Vec::new());
    // Function 0 is imported, so a local class must not answer with it.
    module.funcs[0].params = vec![4];
    module.funcs[0].local_types = vec![4];
    let error = lm_verify::verify_module(&module).expect_err("the verifier admitted the module");
    assert!(error.message.contains("import state"), "{error:?}");
}
