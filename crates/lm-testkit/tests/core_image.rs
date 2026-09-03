//! The pinned core image: determinism, the in-repo hash pin, and
//! prelude independence.

use lm_hir::{check_module_with, lower_module, CheckOptions};
use lm_testkit::repo_root;
use lm_vm::VmConfig;

fn compile_with_prelude(text: &str, prelude: bool) -> Vec<u8> {
    let ast = lm_source::parse::parse(text).expect("parses");
    let hir = check_module_with(
        &ast,
        CheckOptions {
            prelude,
            ..CheckOptions::default()
        },
    )
    .expect("checks");
    lm_bytecode::encode(&lower_module(&hir))
}

fn compile_artifact_with_prelude(text: &str, prelude: bool) -> Vec<u8> {
    let module = lm_bytecode::decode(&compile_with_prelude(text, prelude)).expect("decodes");
    let artifact = lm_testkit::artifact_from_module(module).expect("the artifact builds");
    lm_bytecode::artifact::encode(&artifact).expect("the artifact encodes")
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
    let found = lm_bytecode::hash::hash256_hex(&bytes);
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

/// Compute the pinned core definition lines from one core image.
fn core_def_lines(image: &lm_bytecode::Module) -> String {
    let identity = lm_bytecode::identity::module_identity(image).expect("the core image hashes");
    let mut out = String::new();
    for label in lm_bytecode::corepin::PINNED_LABELS {
        let key = lm_bytecode::corepin::pinned_key(label);
        let idx = image
            .classes
            .iter()
            .position(|c| c.key == key)
            .unwrap_or_else(|| panic!("the core image defines `{label}`"));
        let hex: String = identity.class_hashes[idx]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        out.push_str(&format!("{label} {hex}\n"));
    }
    out
}

/// The hash-linking gate: the pinned core definition hashes match a
/// fresh recompilation. After a deliberate core or identity change,
/// run the ignored test `regenerate_core_pins` and commit the files.
#[test]
fn core_definition_hashes_match_the_pin() {
    let pin_path = repo_root().join("core/pinned-core-defs.txt");
    let pinned = std::fs::read_to_string(&pin_path).expect("core/pinned-core-defs.txt exists");
    let pinned_lines: Vec<&str> = pinned
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .collect();
    let image = lm_hir::core_image();
    let fresh = core_def_lines(&image);
    let fresh_lines: Vec<&str> = fresh.lines().collect();
    assert_eq!(
        pinned_lines, fresh_lines,
        "the core definition hashes changed; if the change is deliberate, \
         run `cargo test -p lm-testkit --test core_image regenerate_core_pins -- --ignored` \
         and commit the new pins"
    );
}

fn encode_intrinsics(intrinsics: &[Option<lm_abi::IntrinsicSlot>]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(intrinsics.len() * 4);
    for intrinsic in intrinsics {
        bytes.extend_from_slice(&intrinsic.unwrap_or(u32::MAX).to_le_bytes());
    }
    bytes
}

#[test]
fn pinned_core_files_match_one_fresh_build() {
    let bundle = lm_abi::standard_bundle();
    let (image, intrinsics) = lm_hir::core_image_with_intrinsics(bundle);
    let compiled = std::fs::read(repo_root().join("core/pinned-core.lmbc"))
        .expect("the pinned core bytes exist");
    let summaries = std::fs::read(repo_root().join("core/pinned-core-intrinsics.bin"))
        .expect("the pinned core intrinsic table exists");
    assert_eq!(compiled, lm_bytecode::encode(&image));
    assert_eq!(summaries, encode_intrinsics(&intrinsics));
    let decoded = lm_bytecode::decode(&compiled).expect("the pinned core decodes");
    lm_verify::verify_module(&decoded).expect("the pinned core verifies");
}

/// The resolved layout in a user module comes from the hashes: every
/// pinned core definition resolves to the embedded core copy.
#[test]
fn core_layout_resolves_by_hash_in_a_user_module() {
    let bytes = compile_with_prelude("x = 1\nx\n", true);
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    let layout = lm_bytecode::corepin::declared_layout(&module);
    for (slot, name) in [
        (layout.option, "Option"),
        (layout.option_some, "Option.Some"),
        (layout.option_none, "Option.None"),
        (layout.result_ok, "Result.Ok"),
        (layout.result_err, "Result.Err"),
        (layout.io_error_failed, "IoError.Failed"),
        (layout.step_done, "StepEvent.Done"),
        (layout.drive_asked, "DriveEvent.Asked"),
    ] {
        let idx = slot.unwrap_or_else(|| panic!("the layout resolves `{name}`"));
        assert_eq!(module.classes[idx as usize].name, name);
    }
}

/// A user enum with a core name but a different shape never spoofs
/// the layout: the hash decides, not the name or the position.
#[test]
fn a_renamed_shape_cannot_spoof_the_core_layout() {
    let program = "enum Option\n  Empty\n  Full(v: Int)\nend\nx: Option = Full(3)\n1\n";
    let bytes = compile_with_prelude(program, true);
    let module = lm_bytecode::decode(&bytes).expect("decodes");
    let layout = lm_bytecode::corepin::declared_layout(&module);
    let option = layout.option.expect("the embedded core Option resolves");
    // The resolved class is the embedded core family, not the user
    // enum: it has the pinned two arms and one type parameter.
    assert_eq!(module.classes[option as usize].type_params, 1);
    let user_option = module
        .classes
        .iter()
        .position(|c| c.name == "Option" && c.type_params == 0)
        .expect("the user enum exists") as u32;
    assert_ne!(option, user_option);
}

/// Rebuild the pinned core files. Run explicitly with
/// `cargo test -p lm-testkit --test core_image -- --ignored`.
#[test]
#[ignore]
fn regenerate_core_pins() {
    let bundle = lm_abi::standard_bundle();
    let (image, intrinsics) = lm_hir::core_image_with_intrinsics(bundle);
    lm_verify::verify_module(&image).expect("the generated core verifies");
    let bytes = lm_bytecode::encode(&image);
    let hash = lm_bytecode::hash::hash256_hex(&bytes);
    std::fs::write(
        repo_root().join("core/pinned-hash.txt"),
        format!("{hash}\n"),
    )
    .expect("pinned-hash.txt writes");
    let header = "# Pinned core definition hashes: `label hash` per line.\n\
                  # Regenerate with the ignored test `regenerate_core_pins`\n\
                  # in crates/lm-testkit/tests/core_image.rs.\n";
    std::fs::write(
        repo_root().join("core/pinned-core-defs.txt"),
        format!("{header}{}", core_def_lines(&image)),
    )
    .expect("pinned-core-defs.txt writes");
    std::fs::write(repo_root().join("core/pinned-core.lmbc"), bytes)
        .expect("pinned-core.lmbc writes");
    std::fs::write(
        repo_root().join("core/pinned-core-intrinsics.bin"),
        encode_intrinsics(&intrinsics),
    )
    .expect("pinned-core-intrinsics.bin writes");
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
    let bytes = compile_artifact_with_prelude(program, false);
    let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("loads");
    let mut vm = lm_vm::Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(true)");
}

/// Without the prelude the unqualified names are unknown; the
/// definitions still exist inside the core image.
#[test]
fn prelude_controls_only_unqualified_names() {
    let ast = lm_source::parse::parse("x: Option[Int] = None\nx\n").expect("parses");
    let err = check_module_with(
        &ast,
        CheckOptions {
            prelude: false,
            ..CheckOptions::default()
        },
    )
    .err()
    .expect("the name `Option` must be unknown without the prelude");
    assert_eq!(err.code, "E1013");
}

/// The user module cannot see core-internal names that the prelude
/// does not export. A user `Option` shadows the prelude name without
/// an error.
#[test]
fn user_definitions_shadow_the_prelude() {
    let program = "enum Option\n  Empty\n  Full(v: Int)\nend\n\
                   x: Option = Full(3)\ncase x\nin Full(v) then v\nin Empty then 0\nend\n";
    let bytes = compile_artifact_with_prelude(program, true);
    let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("loads");
    let mut vm = lm_vm::Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3)");
}
