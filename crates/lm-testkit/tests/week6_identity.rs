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

/// A class rename moves no structural hash of another definition: a
/// reference carries the qualified key, and the key follows the name.
/// The own name never enters the own hash, so a class that nothing
/// references keeps its hash through a rename.
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
}

/// A cache hit replays the definition hashes and the core layout.
/// Both are pure functions of the verification-hash inputs, so a hit
/// recomputes neither.
#[test]
fn a_cache_hit_replays_the_identity() {
    let bytes =
        lm_testkit::compile_to_bytes("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(41)\n").unwrap();
    let mut cache = lm_vm::VerifiedCache::new();
    let first = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!((cache.verifications, cache.identities), (1, 1));
    let second = lm_vm::load_bytes_cached(&bytes, &mut cache).expect("loads");
    assert_eq!(
        (cache.verifications, cache.identities),
        (1, 1),
        "the second load recomputed identity or verification"
    );
    let classes = first.module().classes.len() as u32;
    for c in 0..classes {
        assert_eq!(first.class_hash(c), second.class_hash(c));
    }
    assert_eq!(
        format!("{:?}", first.core_layout()),
        format!("{:?}", second.core_layout())
    );
}

/// The core layout resolves every slot to the class that carries the
/// slot label, never to a user class of another name.
#[test]
fn a_user_enum_cannot_fill_a_core_slot() {
    let module = compile_text("t.lm", MY_ERR).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    let layout = lm_bytecode::corepin::core_layout(&module, &identity);
    let io_error = layout.io_error.expect("the core IoError resolves");
    let failed = layout.io_error_failed.expect("the core arm resolves");
    assert_eq!(module.classes[io_error as usize].name, "IoError");
    assert_eq!(module.classes[failed as usize].name, "IoError.Failed");
}
