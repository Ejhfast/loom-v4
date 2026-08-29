use lm_compiler::{compile_module, CompileEnv};
use lm_source::SourceFile;
use lm_testkit::{bind_compiled_unit, publish_artifact, run_text};
use lm_vm::{Vm, VmConfig};

#[test]
fn contextual_class_modifiers_remain_ordinary_names() {
    let source = "final = 40\nfrozen = 2\nfinal + frozen\n";
    assert_eq!(
        run_text("contextual_names.lm", source, VmConfig::default()).unwrap(),
        "Done(42)"
    );
}

#[test]
fn duplicate_class_modifiers_report_the_class_requirement() {
    let error = run_text(
        "duplicate_modifier.lm",
        "frozen final class Box\nend\n0\n",
        VmConfig::default(),
    )
    .expect_err("duplicate class modifiers must reject");
    assert!(error.contains("expected `class`, found `final`"), "{error}");
}

#[test]
fn hard_keywords_report_reserved_name_use() {
    for (source, name) in [
        ("class = 1\n", "class"),
        ("def read(class: Int): Int\n  class\nend\n0\n", "class"),
    ] {
        let error = run_text("reserved_name.lm", source, VmConfig::default())
            .expect_err("the reserved name must reject");
        assert!(
            error.contains(&format!("`{name}` is a reserved word; choose another name")),
            "{error}"
        );
    }
}

#[test]
fn interpolation_accepts_strings_and_balanced_braces() {
    let source = r##""#{"inner"} #{{"key": 7}["key"]}""##;
    assert_eq!(
        run_text("nested_interpolation.lm", source, VmConfig::default()).unwrap(),
        "Done(\"inner 7\")"
    );
}

#[test]
fn character_literals_run_and_match_patterns() {
    let source = r#"
matched = case '猫'
in '猫' then true
in _ then false
end
('a'.codepoint(), '\n'.codepoint(), '猫'.utf8_len(), matched)
"#;
    assert_eq!(
        run_text("characters.lm", source, VmConfig::default()).unwrap(),
        "Done((97, 10, 3, true))"
    );
}

#[test]
fn every_text_value_can_convert_to_string() {
    let source = r#"
case "abcd".slice(1, 2)
in Ok(part) then ("text".to_string(), part.to_string())
in Err(_) then ("", "")
end
"#;
    assert_eq!(
        run_text("text_to_string.lm", source, VmConfig::default()).unwrap(),
        "Done((\"text\", \"bc\"))"
    );
}

#[test]
fn local_constants_inline_literal_values() {
    let source = r#"
const ANSWER: Int = 40
const PAIR: (Int, Int) = (2, -1)
const MARK: Char = '\n'
(ANSWER + PAIR[0], PAIR[1], MARK.codepoint())
"#;
    assert_eq!(
        run_text("local_constants.lm", source, VmConfig::default()).unwrap(),
        "Done((42, -1, 10))"
    );
}

#[test]
fn constants_reject_runtime_expressions() {
    let source = "const ANSWER: Int = 40 + 2\nANSWER\n";
    let error = run_text("bad_constant.lm", source, VmConfig::default())
        .expect_err("the runtime expression must reject");
    assert!(
        error.contains("a `const` value must be a literal or a tuple of literals"),
        "{error}"
    );
}

#[test]
fn constant_names_cannot_replace_enum_arms() {
    for (source, name) in [
        ("enum Color\n  Red\nend\nconst Red: Int = 1\n0\n", "Red"),
        ("const None: Int = 1\n0\n", "None"),
    ] {
        let error = run_text("constant_arm.lm", source, VmConfig::default())
            .expect_err("the duplicate arm name must reject");
        assert!(error.contains("E1010"), "{error}");
        assert!(error.contains(name), "{error}");
    }
}

#[test]
fn constant_names_cannot_be_assignment_targets() {
    for source in [
        "const LIMIT: Int = 1\nLIMIT = 2\nLIMIT\n",
        "const LIMIT: Int = 1\ndef change(): Int\n  LIMIT = 2\n  LIMIT\nend\nchange()\n",
    ] {
        let error = run_text("constant_assignment.lm", source, VmConfig::default())
            .expect_err("constant assignment must reject");
        assert!(error.contains("cannot assign to `LIMIT`"), "{error}");
    }
}

#[test]
fn constant_names_cannot_bind_patterns() {
    let source = r#"
const LIMIT: Int = 3
case 3
in LIMIT then 1
in _ then 0
end
"#;
    let error = run_text("constant_pattern.lm", source, VmConfig::default())
        .expect_err("constant pattern binding must reject");
    assert!(error.contains("`LIMIT` is a constant"), "{error}");
}

#[test]
fn exported_constants_inline_with_exact_provider_pins() {
    let library = compile_module(
        "lib.values",
        &SourceFile::new(
            "lib/values.lm",
            "const ANSWER: Int = 42\nconst LABEL: Text = \"ready\"\n\
             const PAIR: (Text, Int) = (\"a\", 1)\n\
             const LETTER: Char = '猫'\n",
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");

    let mut library_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut library_env, library.clone()).expect("the library binds");
    let artifact = library_env
        .freeze()
        .complete_artifact("lib.values")
        .expect("the library artifact builds");
    let bytes = lm_bytecode::artifact::encode(&artifact).expect("the library artifact encodes");
    let decoded = lm_bytecode::artifact::decode(&bytes).expect("the library artifact decodes");
    let letter_type = decoded
        .root()
        .module()
        .exports
        .iter()
        .find(|export| export.name == "LETTER")
        .and_then(|export| export.constant.as_ref())
        .expect("the letter export has a value")
        .ty;
    let (library_arena, library_namespace) =
        publish_artifact(&decoded).expect("the library publishes");
    let library_namespace = library_arena
        .namespace(library_namespace)
        .expect("the library namespace exists");
    let linked_letter = library_namespace
        .exports()
        .iter()
        .find(|export| export.name == "LETTER" && export.kind.is_constant())
        .expect("the linked constant stays exported");
    let relocation = library_namespace
        .relocation(decoded.id())
        .expect("the root relocation exists");
    assert_eq!(
        linked_letter.constant.as_ref().unwrap().ty,
        relocation.types()[letter_type as usize]
    );

    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(decoded.root().interface().clone())
        .expect("the decoded interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new(
            "app/main.lm",
            "use lib.values\nuse lib.values.ANSWER\n\
             (ANSWER + values.ANSWER, values.LABEL, values.PAIR, \
              values.LETTER.codepoint())\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");
    let constant_imports: Vec<_> = program
        .module
        .imports
        .iter()
        .filter(|import| import.kind == lm_bytecode::ImportKind::Constant)
        .collect();
    assert_eq!(constant_imports.len(), 4);
    assert!(constant_imports
        .iter()
        .all(|import| import.module == "lib.values"));
    assert!(constant_imports
        .iter()
        .all(|import| import.def == lm_bytecode::NO_IMPORT_DEF));

    let mut link_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut link_env, library).expect("the library binds");
    bind_compiled_unit(&mut link_env, program).expect("the program binds");
    let artifact = link_env
        .freeze()
        .artifact("app.main")
        .expect("the program artifact builds");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(
        vm.show_outcome(&outcome),
        "Done((84, \"ready\", (\"a\", 1), 29483))"
    );
}

#[test]
fn a_constant_pin_rejects_a_stale_provider() {
    let compile_library = |value| {
        compile_module(
            "lib.values",
            &SourceFile::new("lib/values.lm", format!("const LIMIT: Int = {value}\n")),
            &CompileEnv::new().freeze(),
            false,
        )
        .expect("the library compiles")
    };
    let old_library = compile_library(3);
    let new_library = compile_library(4);
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(old_library.interface.clone())
        .expect("the old interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new("app/main.lm", "use lib.values.LIMIT\nLIMIT\n"),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");

    let mut link_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut link_env, new_library).expect("the new library binds");
    bind_compiled_unit(&mut link_env, program).expect("the program binds structurally");
    let artifact = link_env
        .freeze()
        .artifact("app.main")
        .expect("the stale artifact builds");
    let error = publish_artifact(&artifact).expect_err("the stale constant pin must reject");
    assert!(
        error.contains("pin") || error.contains("interface"),
        "{error}"
    );
}

#[test]
fn imported_constant_type_errors_use_the_constant_code() {
    let library = compile_module(
        "lib.values",
        &SourceFile::new("lib/values.lm", "const LIMIT: Int = 3\n"),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let mut interface = library.interface;
    let entry = interface
        .exports
        .iter_mut()
        .find(|entry| entry.name == "LIMIT")
        .expect("the constant export exists");
    let lm_bytecode::interface::IfaceItem::Const(constant) = &mut entry.item else {
        panic!("LIMIT must be a constant");
    };
    constant.value = lm_bytecode::ConstValue::String("wrong".to_string());

    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(interface)
        .expect("the crafted interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let error = compile_module(
        "app.main",
        &SourceFile::new("app/main.lm", "use lib.values.LIMIT\nLIMIT\n"),
        &compile_env.freeze(),
        true,
    )
    .expect_err("the invalid constant type must reject");
    assert!(error.contains("E1053"), "{error}");
    assert!(error.contains("LIMIT"), "{error}");
}

#[test]
fn a_module_alias_qualifies_an_enum_case() {
    let library = compile_module(
        "lib.keys",
        &SourceFile::new("lib/keys.lm", "enum Key\n  Enter\n  Escape\nend\n"),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(library.interface.clone())
        .expect("the interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new(
            "app/main.lm",
            "use lib.keys\n\
             case keys.Key.Enter\n\
             in keys.Key.Enter then true\n\
             in keys.Key.Escape then false\n\
             end\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");

    let mut link_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut link_env, library).expect("the library binds");
    bind_compiled_unit(&mut link_env, program).expect("the program binds");
    let artifact = link_env
        .freeze()
        .artifact("app.main")
        .expect("the program artifact builds");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(true)");
}

#[test]
fn a_direct_type_import_qualifies_an_enum_case() {
    let library = compile_module(
        "lib.keys",
        &SourceFile::new("lib/keys.lm", "enum Key\n  Enter\n  Escape\nend\n"),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");
    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(library.interface.clone())
        .expect("the interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new(
            "app/main.lm",
            "use lib.keys.Key\n\
             case Key.Enter\n\
             in Key.Enter then true\n\
             in Key.Escape then false\n\
             end\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");

    let mut link_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut link_env, library).expect("the library binds");
    bind_compiled_unit(&mut link_env, program).expect("the program binds");
    let artifact = link_env
        .freeze()
        .artifact("app.main")
        .expect("the program artifact builds");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(true)");
}
