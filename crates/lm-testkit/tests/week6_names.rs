//! The definition names are verifier inputs, because identity reads
//! them and the verifier reads the identity-resolved core layout.

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
/// decoder. Identity still moves, because the canonical member order
/// of a component sorts by name.
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

/// A crafted rename of a core method drops a core slot. The key must
/// move with it, or a cached load admits what an uncached load
/// rejects.
#[test]
fn a_crafted_function_rename_cannot_split_the_cache() {
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
    let option = module
        .classes
        .iter()
        .position(|c| c.name == "Option")
        .expect("the core Option is embedded");
    let twin = renamed(&module, "Option.is_some", "Aaa");
    let identity = module_identity(&module).unwrap();
    let twin_identity = module_identity(&twin).unwrap();
    // The rename really moves the core class hash, so the layout of
    // the twin loses the `Option` slot.
    assert_ne!(
        identity.class_hashes[option], twin_identity.class_hashes[option],
        "the probe no longer moves the core class hash"
    );
    assert_ne!(
        verification_hash(&module),
        verification_hash(&twin),
        "a rename that moves the core layout must move the key"
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
    assert!(!plain, "the twin lost a core slot and must reject");
}

/// A resolved core family must be complete. The verifier proves the
/// parent slot where an instruction needs the family, and the runtime
/// allocates through the arm slots, so a parent without an arm would
/// reach a slot the layout does not hold.
#[test]
fn a_partial_core_family_rejects() {
    let bytes = compile_to_bytes("t.lm", SOURCE).unwrap();
    let module = lm_bytecode::decode(&bytes).unwrap();
    let identity = module_identity(&module).unwrap();
    let layout = lm_bytecode::corepin::core_layout(&module, &identity);
    assert!(layout.option.is_some() && layout.option_some.is_some());
    lm_verify::verify_with_layout(&module, layout).expect("the whole family verifies");
    let mut partial = layout;
    partial.option_some = None;
    let error =
        lm_verify::verify_with_layout(&module, partial).expect_err("a partial family must reject");
    assert!(error.message.contains("without every arm"), "{error:?}");
}

/// The cache never replays a definition hash of another module.
#[test]
fn a_cache_hit_never_replays_a_stale_definition_hash() {
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
    let truth = module_identity(&twin).unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    let loaded = lm_vm::load_bytes_cached(&twin_bytes, &mut cache).expect("loads");
    for idx in 0..twin.funcs.len() as u32 {
        assert_eq!(
            loaded.func_hash(idx),
            truth.func_hashes[idx as usize],
            "the cache replayed a definition hash the module does not have"
        );
    }
}
