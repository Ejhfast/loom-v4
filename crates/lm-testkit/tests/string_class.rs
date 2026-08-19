use lm_bytecode::{
    corepin::{ROLE_CHAR, ROLE_STRING, ROLE_SUBSTRING, ROLE_TEXT},
    BcType, Instr,
};
use lm_testkit::{compile_text, run_text};
use lm_vm::{Vm, VmConfig};

fn string_method(module: &lm_bytecode::Module, name: &str) -> (u32, u32) {
    let class = module.core_roles[ROLE_TEXT];
    assert_ne!(class, lm_bytecode::NO_ROLE);
    module.classes[class as usize]
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == name)
        .copied()
        .expect("the String method exists")
}

#[test]
fn text_methods_use_scalar_indices_and_explicit_byte_indices() {
    let source = r#"
text = "aé猫"
(
  (
    text.byte_len(),
    text.len(),
    text.at(1),
    text.find("猫"),
    text.find_bytes("猫"),
    text.slice(1, 2),
    text.slice_bytes(1, 2),
    text.slice_bytes(2, 1),
    text.bytes().hex()
  ),
  (
    "".is_empty(),
    "ab" + "é",
    text.starts_with("aé"),
    text.ends_with("猫"),
    text.contains("é猫"),
    "same" == "same",
    "same" != "other",
    String().is_empty(),
    "a".__add__("b")
  )
)
"#;
    assert_eq!(
        run_text("string_methods.lm", source, VmConfig::default()).unwrap(),
        "Done(((6, 3, Some('é'), Some(2), Some(3), Ok(\"é猫\"), Ok(\"é\"), Err(InvalidBoundary), \"61c3a9e78cab\"), (true, \"abé\", true, true, true, true, true, true, \"ab\")))"
    );
}

#[test]
fn string_intrinsics_inline_to_canonical_instructions() {
    let source = r#"
(
  "a".byte_len(),
  "a".len(),
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
fn interpolation_finishes_its_private_builder() {
    let module =
        compile_text("interpolation_finish.lm", "\"value={1}\"\n").expect("the program compiles");
    let instructions: Vec<Instr> = module.funcs[module.entry as usize]
        .blocks
        .iter()
        .flatten()
        .copied()
        .collect();
    assert!(instructions.contains(&Instr::Native(lm_bytecode::NativeInstr::SbFinish)));
    assert!(!instructions.contains(&Instr::Native(lm_bytecode::NativeInstr::SbBuild)));
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
            .contains("core role `String` does not name a final stateless Text class"),
        "{error}"
    );
}

#[test]
fn the_verifier_requires_the_complete_text_family() {
    let mut module = compile_text("text_family.lm", "\"x\"\n").expect("the program compiles");
    module.core_roles[ROLE_SUBSTRING] = lm_bytecode::NO_ROLE;
    let error = lm_verify::verify_module(&module).expect_err("the family rejects");
    assert!(
        error
            .message
            .contains("only String and Substring can inherit Text"),
        "{error}"
    );
}

#[test]
fn the_verifier_rejects_an_extra_text_subclass() {
    let mut module =
        compile_text("text_subclass.lm", "class Extra\nend\n0\n").expect("the program compiles");
    let text = module.core_roles[ROLE_TEXT];
    let extra = module
        .classes
        .iter()
        .position(|class| class.name == "Extra")
        .expect("the class exists");
    module.classes[extra].parent = text;
    let error = lm_verify::verify_module(&module).expect_err("the subclass rejects");
    assert!(
        error
            .message
            .contains("only String and Substring can inherit Text"),
        "{error}"
    );
}

#[test]
fn the_verifier_rejects_heap_allocation_for_char() {
    let mut module = compile_text("char_new.lm", "0\n").expect("the program compiles");
    let class = module.core_roles[ROLE_CHAR];
    let char_ty = module
        .types
        .iter()
        .position(|ty| *ty == BcType::Class(class))
        .expect("the Char type exists") as u32;
    let entry = module.entry as usize;
    module.funcs[entry].ret = char_ty;
    module.funcs[entry].blocks = vec![vec![Instr::New(class), Instr::Return]];
    let error = lm_verify::verify_module(&module).expect_err("native allocation rejects");
    assert!(
        error
            .message
            .contains("cannot allocate a native core class"),
        "{error}"
    );
}

#[test]
fn text_views_and_chars_reject_direct_construction() {
    for name in ["Text", "Substring", "Char"] {
        let source = format!("{name}()\n");
        let error = compile_text("native_text_new.lm", &source).expect_err("construction rejects");
        assert!(
            error.contains(&format!("`{name}` values cannot be constructed directly")),
            "{error}"
        );
    }
}

#[test]
fn string_and_substring_share_text_equality_and_map_keys() {
    let source = r#"
case "xé猫z".slice(1, 2)
in Ok(view) then (
  view == "é猫",
  "[" + view + "]",
  {"é猫": 1, view: 2}.len(),
  {"é猫": 1, view: 2}.at("é猫"),
  {"é猫": 1, view: 2}.at(view)
)
in Err(_) then (false, "bad", 0, {"x": 0}.at("y"), {"x": 0}.at("y"))
end
"#;
    assert_eq!(
        run_text("text_map_keys.lm", source, VmConfig::default()).unwrap(),
        "Done((true, \"[é猫]\", 1, 2, 2))"
    );
}

#[test]
fn substring_concatenation_and_string_map_queries_use_text_content() {
    let source = r#"
values: {String: Int} = {"é猫": 7}
case "xé猫z".slice(1, 2)
in Ok(view) then (
    view + "!",
    view.concat("?"),
    values.has(view),
    values.get(view).value_or(0),
    values.at(view),
    values[view]
  )
in Err(_) then ("bad", "bad", false, 0, 0, 0)
end
"#;
    assert_eq!(
        run_text("text_query_keys.lm", source, VmConfig::default()).unwrap(),
        "Done((\"é猫!\", \"é猫?\", true, 7, 7, 7))"
    );
}

#[test]
fn each_and_map_traverse_unicode_scalars() {
    let source = r#"
seen = StringBuilder()
"aé猫".each(do |value: Char|
  seen.push_char(value)
  ()
end)
mapped = "aé猫".map(do |value: Char| value end)
(seen.finish(), mapped)
"#;
    assert_eq!(
        run_text("text_traversal.lm", source, VmConfig::default()).unwrap(),
        "Done((\"aé猫\", \"aé猫\"))"
    );

    let module = compile_text("text_traversal.lm", source).expect("the program compiles");
    assert!(module.funcs.iter().any(|func| {
        func.blocks
            .iter()
            .flatten()
            .any(|instr| *instr == Instr::Native(lm_bytecode::NativeInstr::TextAtByte))
    }));
}
