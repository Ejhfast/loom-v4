//! Names, qualified keys, and the verifier.

use lm_bytecode::identity::{module_identity, verification_hash};
fn core_module() -> lm_bytecode::Module {
    lm_compiler::core_link_unit()
        .expect("the core unit builds")
        .module()
        .clone()
}

/// Rename one definition in the export section only. The semantic
/// region does not move, so the module means the same program to the
/// decoder.
fn renamed(module: &lm_bytecode::Module, from: &str, to: &str) -> lm_bytecode::Module {
    let mut out = module.clone();
    let idx = out
        .funcs
        .iter()
        .position(|f| f.name == from)
        .unwrap_or_else(|| panic!("no function `{from}`"));
    out.funcs[idx].name = to.to_string();
    out
}

/// The attack this file replays no longer reaches identity. A
/// structural hash covers no name, so a crafted rename of a core
/// method moves no core class hash and drops no core slot.
#[test]
fn a_crafted_function_rename_moves_no_structural_hash() {
    let module = core_module();
    let twin = renamed(&module, "Option.is_some", "Aaa");
    let identity = module_identity(&module).unwrap();
    let twin_identity = module_identity(&twin).unwrap();
    assert_eq!(
        identity.class_hashes, twin_identity.class_hashes,
        "a rename moved a class structural hash"
    );
    assert_eq!(
        identity.func_hashes, twin_identity.func_hashes,
        "a rename moved a function structural hash"
    );
    let layout = lm_bytecode::corepin::declared_layout(&module);
    let twin_layout = lm_bytecode::corepin::declared_layout(&twin);
    assert_eq!(
        format!("{layout:?}"),
        format!("{twin_layout:?}"),
        "a rename moved the core layout"
    );
    lm_testkit::unit_from_module(twin).expect("the rename keeps valid code");
}

/// A crafted class key changes the constructor rule that the verifier reads.
#[test]
fn a_crafted_qualified_key_moves_the_verifier_input() {
    let module = core_module();
    let some = module
        .classes
        .iter()
        .position(|c| c.key == "core.Option.Some")
        .expect("the core arm is embedded");
    let mut twin = module.clone();
    twin.classes[some].key = "core.Option.Aaa".to_string();
    assert_ne!(
        verification_hash(&module),
        verification_hash(&twin),
        "a key edit did not move the cache key"
    );
    assert!(
        lm_testkit::unit_from_module(twin).is_err(),
        "the verifier admitted a mismatched constructor key"
    );
}

/// A crafted core role table rejects, and a cached load and an
/// uncached load agree on that. The role table lives inside the
/// semantic region, so it moves the cache key.
#[test]
fn a_crafted_core_role_cannot_split_the_cache() {
    let module = core_module();
    let some = lm_bytecode::corepin::role_index("Option.Some").expect("the role exists");
    let none = lm_bytecode::corepin::role_index("Option.None").expect("the role exists");
    let mut twin = module.clone();
    // Point the `Some` role at the `None` class. The shapes differ,
    // so the verifier must reject.
    twin.core_roles[some] = module.core_roles[none];
    twin.core_roles[none] = lm_bytecode::NO_ROLE;
    assert_ne!(
        verification_hash(&module),
        verification_hash(&twin),
        "a role edit must move the cache key"
    );
    assert!(
        lm_testkit::unit_from_module(twin).is_err(),
        "the crafted role table was admitted"
    );
}

/// A declared core family must be complete. The verifier proves the
/// parent slot where an instruction needs the family, and the runtime
/// allocates through the arm slots, so a parent without an arm would
/// reach a slot the layout does not hold.
#[test]
fn a_partial_core_family_rejects() {
    let module = core_module();
    let layout = lm_bytecode::corepin::declared_layout(&module);
    assert!(layout.option.is_some() && layout.option_some.is_some());
    lm_verify::verify_module(&module).expect("the whole family verifies");
    let role = lm_bytecode::corepin::role_index("Option.Some").expect("the role exists");
    let mut partial = module.clone();
    partial.core_roles[role] = lm_bytecode::NO_ROLE;
    let error = lm_verify::verify_module(&partial).expect_err("a partial family must reject");
    assert!(error.message.contains("without every arm"), "{error:?}");
}

/// A rename keeps admission and the exact core layout.
#[test]
fn a_rename_keeps_the_admission_and_the_core_layout() {
    let source = "def even(n: Int): Bool\n\
                  \x20 if n == 0\n\
                  \x20   true\n\
                  \x20 else\n\
                  \x20   odd(n - 1)\n\
                  \x20 end\n\
                  end\n\
                  def odd(n: Int): Bool\n\
                  \x20 if n == 0\n\
                  \x20   false\n\
                  \x20 else\n\
                  \x20   even(n - 1)\n\
                  \x20 end\n\
                  end\n\
                  even(4)\n";
    let module = lm_testkit::compile_module_text("t.lm", source).unwrap();
    let twin = renamed(&module, "even", "zzz");
    assert_eq!(
        module_identity(&module).unwrap().func_hashes,
        module_identity(&twin).unwrap().func_hashes,
        "a rename inside a cycle moved a structural hash"
    );
    lm_verify::verify_module(&module).expect("the original verifies");
    lm_verify::verify_module(&twin).expect("the rename verifies");
    assert_eq!(
        format!("{:?}", lm_bytecode::corepin::declared_layout(&module)),
        format!("{:?}", lm_bytecode::corepin::declared_layout(&twin))
    );
}
