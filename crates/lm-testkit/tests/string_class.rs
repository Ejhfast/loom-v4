use lm_bytecode::{corepin::ROLE_STRING, BcType, Instr};
use lm_testkit::{compile_text, run_text};
use lm_vm::{Vm, VmConfig};

fn string_method(module: &lm_bytecode::Module, name: &str) -> (u32, u32) {
    let class = module.core_roles[ROLE_STRING];
    assert_ne!(class, lm_bytecode::NO_ROLE);
    module.classes[class as usize]
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == name)
        .copied()
        .expect("the String method exists")
}

#[test]
fn string_methods_keep_utf8_and_use_byte_indices() {
    let source = r#"
text = "héllo"
(
  text.byte_len(),
  text.char_count(),
  "".is_empty(),
  "ab" + "é",
  text.starts_with("hé"),
  text.ends_with("llo"),
  text.contains("él"),
  text.find("ll"),
  text.find("z"),
  "same" == "same",
  "same" != "other",
  String().is_empty(),
  "a".__add__("b")
)
"#;
    assert_eq!(
        run_text("string_methods.lm", source, VmConfig::default()).unwrap(),
        "Done((6, 5, true, \"abé\", true, true, true, Some(3), None, true, true, true, \"ab\"))"
    );
}

#[test]
fn string_intrinsics_inline_to_canonical_instructions() {
    let source = r#"
(
  "a".byte_len(),
  "a".char_count(),
  "a".concat("b"),
  "ab".starts_with("a"),
  "ab".ends_with("b"),
  "ab".contains("a"),
  "a" == "a",
  "a" != "b"
)
"#;
    let module = compile_text("string_instructions.lm", source).expect("the program compiles");
    let class = module.core_roles[ROLE_STRING];
    assert!(module.classes[class as usize].is_final);
    let instructions: Vec<Instr> = module.funcs[module.entry as usize]
        .blocks
        .iter()
        .flatten()
        .copied()
        .collect();
    for expected in [
        Instr::Native(lm_bytecode::NativeInstr::StrByteLen),
        Instr::Native(lm_bytecode::NativeInstr::StrCharCount),
        Instr::Native(lm_bytecode::NativeInstr::StrConcat),
        Instr::Native(lm_bytecode::NativeInstr::StrStartsWith),
        Instr::Native(lm_bytecode::NativeInstr::StrEndsWith),
        Instr::Native(lm_bytecode::NativeInstr::StrContains),
        Instr::Native(lm_bytecode::NativeInstr::EqStr),
        Instr::Native(lm_bytecode::NativeInstr::NeStr),
    ] {
        assert!(instructions.contains(&expected), "missing {expected:?}");
    }
    assert!(instructions
        .iter()
        .all(|instr| !matches!(instr, Instr::CallVirtual { .. })));
}

#[test]
fn a_string_tag_supports_verified_virtual_dispatch() {
    let mut module =
        compile_text("string_virtual.lm", "\"é\".byte_len()\n").expect("the program compiles");
    let (selector, _) = string_method(&module, "byte_len");
    let literal = module
        .strings
        .iter()
        .position(|text| text == "é")
        .expect("the literal exists") as u32;
    module.funcs[module.entry as usize].blocks = vec![vec![
        Instr::ConstStr(literal),
        Instr::CallVirtual { selector, argc: 0 },
        Instr::Return,
    ]];
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(2)");
}

#[test]
fn the_verifier_rejects_a_stateful_string_role() {
    let mut module = compile_text("string_role.lm", "\"x\"\n").expect("the program compiles");
    let class = module.core_roles[ROLE_STRING];
    let int_ty = module
        .types
        .iter()
        .position(|ty| *ty == BcType::Int)
        .expect("the Int type exists") as u32;
    module.classes[class as usize]
        .fields
        .push(("bad".to_string(), int_ty));
    let error = lm_verify::verify_module(&module).expect_err("the role rejects");
    assert!(
        error
            .message
            .contains("core role `String` does not name a final stateless class"),
        "{error}"
    );
}
