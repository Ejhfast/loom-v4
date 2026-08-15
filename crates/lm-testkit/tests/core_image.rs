//! The pinned core image: determinism, the in-repo hash pin, and
//! prelude independence.

use lm_hir::{check_module_with, lower_module, CheckOptions};
use lm_testkit::repo_root;
use lm_vm::VmConfig;

fn compile_with_prelude(text: &str, prelude: bool) -> Vec<u8> {
    let ast = lm_source::parse::parse(text).expect("parses");
    let hir = check_module_with(&ast, CheckOptions { prelude }).expect("checks");
    lm_bytecode::encode(&lower_module(&hir))
}

#[test]
fn core_image_recompiles_byte_identically() {
    let a = lm_bytecode::encode(&lm_hir::core_image());
    let b = lm_bytecode::encode(&lm_hir::core_image());
    assert_eq!(a, b, "the core image bytes differ between compilations");
}

#[test]
fn core_image_passes_the_verifier() {
    let image = lm_hir::core_image();
    lm_verify::verify_module(&image).expect("the core image verifies");
}

/// The determinism gate: a rebuild must reproduce the pinned bytes.
/// After a deliberate core change, update `core/pinned-hash.txt` with
/// the hash printed in the failure message.
#[test]
fn core_image_matches_the_pinned_hash() {
    let bytes = lm_bytecode::encode(&lm_hir::core_image());
    let found = lm_bytecode::hash::sha256_hex(&bytes);
    let pin_path = repo_root().join("core/pinned-hash.txt");
    let pinned = std::fs::read_to_string(&pin_path)
        .expect("core/pinned-hash.txt exists")
        .trim()
        .to_string();
    assert_eq!(
        found, pinned,
        "the core image hash changed; if the change is deliberate, \
         write the new hash {found} into core/pinned-hash.txt"
    );
}

/// The prelude is a name-import layer only. The core image compiles
/// without it, and a program that uses no prelude name compiles to
/// identical bytes with the prelude on or off.
#[test]
fn prelude_membership_does_not_change_core_identity() {
    let program = "def double(n: Int): Int\n  n * 2\nend\ndouble(21)\n";
    let with_prelude = compile_with_prelude(program, true);
    let without_prelude = compile_with_prelude(program, false);
    assert_eq!(with_prelude, without_prelude);
}

/// `List.get` returns the pinned core `Option` even when the prelude
/// name `Option` is not in scope: the identity comes from the core
/// image, not from the prelude.
#[test]
fn get_returns_core_option_without_the_prelude() {
    let program = "xs = [1, 2]\nxs.get(0).is_some()\n";
    let bytes = compile_with_prelude(program, false);
    let loaded = lm_vm::load_bytes(&bytes).expect("loads");
    let mut vm = lm_vm::Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(true)");
}

/// Without the prelude the unqualified names are unknown; the
/// definitions still exist inside the core image.
#[test]
fn prelude_controls_only_unqualified_names() {
    let ast = lm_source::parse::parse("x: Option[Int] = None\nx\n").expect("parses");
    let err = check_module_with(&ast, CheckOptions { prelude: false })
        .err()
        .expect("the name `Option` must be unknown without the prelude");
    assert_eq!(err.code, "E1013");
}

/// The user module cannot see core-internal names that the prelude
/// does not export; there are none today, so shadowing is the check:
/// a user `Option` shadows the prelude name without an error.
#[test]
fn user_definitions_shadow_the_prelude() {
    let program = "enum Option\n  Empty\n  Full(v: Int)\nend\n\
                   x: Option = Full(3)\ncase x\nin Full(v) then v\nin Empty then 0\nend\n";
    let bytes = compile_with_prelude(program, true);
    let loaded = lm_vm::load_bytes(&bytes).expect("loads");
    let mut vm = lm_vm::Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3)");
}
