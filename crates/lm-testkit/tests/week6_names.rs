//! Names, qualified keys, and the verifier.
//!
//! A rename moves no structural hash, so a rename cannot move the
//! core layout. A crafted qualified key still can, because the core
//! layout resolves on `(qualified key, structural hash)`. The keys are
//! therefore verifier inputs, and the cache key covers them.

use lm_bytecode::identity::{module_identity, verification_hash};
use lm_testkit::compile_to_bytes;

/// A program that needs the pinned core `Option` definition.
const SOURCE: &str = "def read(): String with Io.ReadLine\n\
                      \x20 case sys.io.read_line()\n\
                      \x20 in Ok(line)\n\
                      \x20   case line\n\
                      \x20   in Some(text) then text\n\
                      \x20   in None then \"eof\"\n\
                      \x20   end\n\
                      \x20 in Err(_) then \"error\"\n\
                      \x20 end\n\
                      end\n\
                      read()\n";

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
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
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
    let twin_bytes = lm_bytecode::encode(&twin);
    let plain = lm_vm::load_bytes(&twin_bytes).is_ok();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("the original loads");
    let cached = lm_vm::load_bytes_cached(&twin_bytes, &mut cache).is_ok();
    assert_eq!(
        plain, cached,
        "a cached load and an uncached load disagree on admission"
    );
    assert!(plain, "the rename changed nothing the verifier reads");
}

/// A crafted qualified key changes nothing the verifier reads. The
/// core layout comes from the declared role table, so a key edit
/// cannot drop a core slot.
#[test]
fn a_crafted_qualified_key_moves_no_verifier_input() {
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
    let some = module
        .classes
        .iter()
        .position(|c| c.key == "core.Option.Some")
        .expect("the core arm is embedded");
    let mut twin = module.clone();
    twin.classes[some].key = "core.Option.Aaa".to_string();
    assert_eq!(
        verification_hash(&module),
        verification_hash(&twin),
        "a key edit moved the cache key"
    );
    let twin_bytes = lm_bytecode::encode(&twin);
    let plain = lm_vm::load_bytes(&twin_bytes).is_ok();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("the original loads");
    let cached = lm_vm::load_bytes_cached(&twin_bytes, &mut cache).is_ok();
    assert_eq!(
        plain, cached,
        "a cached load and an uncached load disagree on admission"
    );
    assert!(plain, "the key edit changed nothing the verifier reads");
}

/// A crafted core role table rejects, and a cached load and an
/// uncached load agree on that. The role table lives inside the
/// semantic region, so it moves the cache key.
#[test]
fn a_crafted_core_role_cannot_split_the_cache() {
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
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
    let twin_bytes = lm_bytecode::encode(&twin);
    let plain = lm_vm::load_bytes(&twin_bytes).is_ok();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("the original loads");
    let cached = lm_vm::load_bytes_cached(&twin_bytes, &mut cache).is_ok();
    assert_eq!(
        plain, cached,
        "a cached load and an uncached load disagree on admission"
    );
    assert!(!plain, "the crafted role table was admitted");
}

/// A declared core family must be complete. The verifier proves the
/// parent slot where an instruction needs the family, and the runtime
/// allocates through the arm slots, so a parent without an arm would
/// reach a slot the layout does not hold.
#[test]
fn a_partial_core_family_rejects() {
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
    let layout = lm_bytecode::corepin::declared_layout(&module);
    assert!(layout.option.is_some() && layout.option_some.is_some());
    lm_verify::verify_module(&module).expect("the whole family verifies");
    let role = lm_bytecode::corepin::role_index("Option.Some").expect("the role exists");
    let mut partial = module.clone();
    partial.core_roles[role] = lm_bytecode::NO_ROLE;
    let error = lm_verify::verify_module(&partial).expect_err("a partial family must reject");
    assert!(error.message.contains("without every arm"), "{error:?}");
}

/// A rename never changes what the loader admits, and a cached load
/// and an uncached load always agree.
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
    let bytes = compile_to_bytes("t.lm", source).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
    let twin = renamed(&module, "even", "zzz");
    let twin_bytes = lm_bytecode::encode(&twin);
    assert_eq!(
        module_identity(&module).unwrap().func_hashes,
        module_identity(&twin).unwrap().func_hashes,
        "a rename inside a cycle moved a structural hash"
    );
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    let loaded = lm_vm::load_bytes_cached(&twin_bytes, &mut cache).expect("loads");
    let plain = lm_vm::load_bytes(&twin_bytes).expect("loads");
    assert_eq!(
        format!("{:?}", loaded.core_layout()),
        format!("{:?}", plain.core_layout()),
        "a cached load and an uncached load disagree on the layout"
    );
    // The rename moves no verifier input, so the twin rides the hit.
    assert_eq!(cache.verifications, 1, "the rename cost a verifier run");
}
