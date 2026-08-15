//! Week-6 identity suites: nominal class identity, the core-pin
//! lookup, and the interface hash.

use lm_bytecode::identity::module_identity;
use lm_testkit::compile_text;
use std::collections::HashMap;

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

/// Two classes with different names must never share a definition
/// hash. Class identity is nominal.
#[test]
fn differently_named_classes_never_share_a_definition_hash() {
    let module = compile_text("t.lm", MY_ERR).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    let mut groups: HashMap<[u8; 32], Vec<&str>> = HashMap::new();
    for (idx, class) in module.classes.iter().enumerate() {
        groups
            .entry(identity.class_hashes[idx])
            .or_default()
            .push(class.name.as_str());
    }
    for names in groups.values() {
        let first = names[0];
        assert!(
            names.iter().all(|n| *n == first),
            "one definition hash covers differently named classes: {names:?}"
        );
    }
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
