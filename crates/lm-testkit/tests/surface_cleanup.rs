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
(ANSWER + PAIR[0], PAIR[1])
"#;
    assert_eq!(
        run_text("local_constants.lm", source, VmConfig::default()).unwrap(),
        "Done((42, -1))"
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
fn exported_constants_inline_without_a_runtime_dependency() {
    let library = compile_module(
        "lib.values",
        &SourceFile::new(
            "lib/values.lm",
            "const ANSWER: Int = 42\nconst LABEL: Text = \"ready\"\n",
        ),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles");

    let mut library_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut library_env, library).expect("the library binds");
    let artifact = library_env
        .freeze()
        .complete_artifact("lib.values")
        .expect("the library artifact builds");
    let bytes = lm_bytecode::artifact::encode(&artifact).expect("the library artifact encodes");
    let decoded = lm_bytecode::artifact::decode(&bytes).expect("the library artifact decodes");

    let mut compile_env = CompileEnv::new();
    compile_env
        .bind_projection(decoded.root().interface().clone())
        .expect("the decoded interface binds");
    compile_env.bind_root("lib", "lib").expect("the root binds");
    let program = compile_module(
        "app.main",
        &SourceFile::new(
            "app/main.lm",
            "use lib.values\nuse lib.values.ANSWER\n(ANSWER + values.ANSWER, values.LABEL)\n",
        ),
        &compile_env.freeze(),
        true,
    )
    .expect("the program compiles");
    assert!(program
        .module
        .imports
        .iter()
        .all(|import| import.module != "lib.values"));

    let mut link_env = lm_compiler::core_link_env().expect("the core environment builds");
    bind_compiled_unit(&mut link_env, program).expect("the program binds without the library");
    let artifact = link_env
        .freeze()
        .artifact("app.main")
        .expect("the program artifact builds");
    let (arena, namespace) = publish_artifact(&artifact).expect("the program publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done((84, \"ready\"))");
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
        &SourceFile::new("app/main.lm", "use lib.keys\nkeys.Key.Enter is keys.Key\n"),
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
