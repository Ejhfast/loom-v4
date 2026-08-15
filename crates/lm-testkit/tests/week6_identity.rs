//! Identity suites: the qualified key, the core-pin lookup, and the
//! identity replay on a cache hit.

use lm_bytecode::identity::module_identity;
use lm_testkit::compile_text;

/// The idiomatic user enum that week 5 could not separate from the
/// core `IoError`: the same arm name, the same field, and the same
/// accessor method.
const MY_ERR: &str = "enum MyErr\n\
                      \x20 Failed(message: String)\n\
                      \n\
                      \x20 def message(self): String\n\
                      \x20   case self\n\
                      \x20   in Failed(m) then m\n\
                      \x20   end\n\
                      \x20 end\n\
                      end\n\
                      x: MyErr = Failed(\"n\")\n\
                      x.message()\n";

/// The week-5 gap the probe records: a user enum that copies the core
/// `IoError` exactly. A structural hash carries no name now, so the
/// separation must come from the qualified key of the referenced arm.
#[test]
fn the_qualified_key_separates_a_user_enum_from_the_core_family() {
    let module = compile_text("t.lm", MY_ERR).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    let find = |name: &str| {
        module
            .classes
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no class `{name}`"))
    };
    let my_err = find("MyErr");
    let io_error = find("IoError");
    let my_failed = find("MyErr.Failed");
    let core_failed = find("IoError.Failed");
    assert_eq!(module.classes[my_err].key, "MyErr");
    assert_eq!(module.classes[io_error].key, "core.IoError");
    // An arm names its parent by key, and a family names its arms by
    // key, so both levels stay apart.
    assert_ne!(
        identity.class_hashes[my_failed], identity.class_hashes[core_failed],
        "two arms with different parents share a structural hash"
    );
    assert_ne!(
        identity.class_hashes[my_err], identity.class_hashes[io_error],
        "two families with different arms share a structural hash"
    );
}

/// A class rename through the source moves the qualified key, so it
/// moves every hash that names the class. The verification hash holds,
/// because a key lives in the export section.
///
/// An earlier version of this test set `class.name` and left
/// `class.key`, which the source compiler cannot produce, so it proved
/// a case that never occurs.
#[test]
fn a_class_rename_moves_no_hash_when_the_key_holds() {
    let source = "class Point\n  x: Int = 0\nend\np = Point()\np.x\n";
    let module = compile_text("t.lm", source).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    let mut twin = module.clone();
    let idx = twin
        .classes
        .iter()
        .position(|c| c.name == "Point")
        .expect("the class exists");
    twin.classes[idx].name = "Place".to_string();
    let twin_identity = module_identity(&twin).expect("hashes");
    assert_eq!(
        identity.class_hashes, twin_identity.class_hashes,
        "a rename that keeps the key moved a structural hash"
    );

    // The case the source compiler produces: a rename moves the key.
    let renamed = compile_text("t.lm", "class Place\n  x: Int = 0\nend\np = Place()\np.x\n")
        .expect("compiles");
    let renamed_identity = module_identity(&renamed).expect("hashes");
    // The class keeps its own hash, because its own key never enters
    // it. Every definition that NAMES the class moves, because a
    // reference carries the qualified key. The constructor is one.
    assert_eq!(
        identity.class_hashes, renamed_identity.class_hashes,
        "the own key must stay outside the own hash"
    );
    assert_ne!(
        identity.func_hashes, renamed_identity.func_hashes,
        "a source rename must move the hash of a definition that names the class"
    );
    // The verification hash holds: a key lives in the export section,
    // which the verifier never reads.
    assert_eq!(
        lm_bytecode::identity::verification_hash(&module),
        lm_bytecode::identity::verification_hash(&renamed),
        "a source rename moved the verification hash"
    );
}

/// The load path computes no identity. The core layout comes from the
/// core role table the artifact declares, and the verifier proves the
/// shape of every filled slot. A second load skips the verifier.
#[test]
fn a_load_computes_no_identity() {
    let bytes =
        lm_testkit::compile_to_bytes("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(41)\n").unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    let first = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(cache.verifications, 1);
    let second = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(
        cache.verifications, 1,
        "the second load ran the verifier again"
    );
    assert_eq!(
        format!("{:?}", first.core_layout()),
        format!("{:?}", second.core_layout())
    );
    // The declared table is the whole resolution.
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    assert_eq!(
        format!("{:?}", first.core_layout()),
        format!("{:?}", lm_bytecode::corepin::declared_layout(&module))
    );
}

/// The core layout names the embedded core class of each role, never
/// a user class with a core name.
#[test]
fn a_user_enum_cannot_fill_a_core_slot() {
    let module = compile_text("t.lm", MY_ERR).expect("compiles");
    let layout = lm_bytecode::corepin::declared_layout(&module);
    let io_error = layout.io_error.expect("the core IoError resolves");
    let failed = layout.io_error_failed.expect("the core arm resolves");
    assert_eq!(module.classes[io_error as usize].name, "IoError");
    assert_eq!(module.classes[failed as usize].name, "IoError.Failed");
}
