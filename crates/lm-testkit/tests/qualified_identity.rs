//! Qualified-key identity and core-role lookup.

use lm_bytecode::identity::module_identity;
use lm_testkit::compile_module_text;

/// A user enum with the same structure as core `IoError`.
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

/// A qualified key separates a user enum from an equal core shape.
#[test]
fn the_qualified_key_separates_a_user_enum_from_the_core_family() {
    let module = compile_module_text("t.lm", MY_ERR).expect("compiles");
    let identity = module_identity(&module).expect("hashes");
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let core_identity = module_identity(core.module()).expect("the core hashes");
    let find = |module: &lm_bytecode::Module, name: &str| {
        module
            .classes
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no class `{name}`"))
    };
    let my_err = find(&module, "MyErr");
    let io_error = find(core.module(), "IoError");
    let my_failed = find(&module, "MyErr.Failed");
    let core_failed = find(core.module(), "IoError.Failed");
    assert_eq!(module.classes[my_err].key, "MyErr");
    assert_eq!(core.module().classes[io_error].key, "core.IoError");
    // An arm names its parent by key, and a family names its arms by
    // key, so both levels stay apart.
    assert_ne!(
        identity.class_hashes[my_failed], core_identity.class_hashes[core_failed],
        "two arms with different parents share a structural hash"
    );
    assert_ne!(
        identity.class_hashes[my_err], core_identity.class_hashes[io_error],
        "two families with different arms share a structural hash"
    );
}

/// A class rename through source moves the qualified key. It also moves
/// every hash that names the class. A published slot key moves too.
#[test]
fn a_class_rename_moves_no_hash_when_the_key_holds() {
    let source = "class Point\n  x: Int = 0\nend\np = Point()\np.x\n";
    let module = compile_module_text("t.lm", source).expect("compiles");
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
    let renamed = compile_module_text("t.lm", "class Place\n  x: Int = 0\nend\np = Place()\np.x\n")
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
    assert_ne!(
        lm_bytecode::identity::verification_hash(&module),
        lm_bytecode::identity::verification_hash(&renamed),
        "a source binding rename must move the verification hash"
    );
}

/// A published namespace uses the role layout of its exact core unit.
#[test]
fn a_namespace_uses_its_exact_core_layout() {
    let bytes =
        lm_testkit::compile_to_bytes("t.lm", "def f(n: Int): Int\n  n + 1\nend\nf(41)\n").unwrap();
    let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("publishes");
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let vm = lm_vm::Vm::new(arena, namespace, lm_vm::VmConfig::default());
    assert_eq!(
        format!("{:?}", vm.core_layout()),
        format!("{:?}", lm_bytecode::corepin::declared_layout(core.module()))
    );
}

/// The core layout names the embedded core class of each role, never
/// a user class with a core name.
#[test]
fn a_user_enum_cannot_fill_a_core_slot() {
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let module = core.module();
    let layout = lm_bytecode::corepin::declared_layout(module);
    let io_error = layout.io_error.expect("the core IoError resolves");
    let failed = layout.io_error_failed.expect("the core arm resolves");
    assert_eq!(module.classes[io_error as usize].name, "IoError");
    assert_eq!(module.classes[failed as usize].name, "IoError.Failed");
}
