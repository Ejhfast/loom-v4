use super::*;

const REGEX_PROGRAM: &str = r#"
regex = re"(?P<word>[a-z]+)-([0-9]+)"
i = 0
total = 0
while i < 200
  text = if i % 2 == 0 then "ab-42" else "none" end
  if regex.is_match(text)
    matched = regex.captures(text).expect("the match exists")
    total = total + matched.start_byte() + matched.end_byte()
    total = total + matched.group(2).expect("the group exists").len()
  end
  total = total + regex.count(text)
  i = i + 1
end
(total, regex.split("a-1 b-2"), regex.replace_all("a-1 b-2", "${word}:$2"))
"#;

#[test]
fn regex_operations_stay_native() {
    let artifact =
        lm_testkit::compile_text("jit-regex.lm", REGEX_PROGRAM).expect("the regex case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 1000, "{metrics:?}");
}

#[test]
fn regex_operations_match_each_fuel_boundary() {
    let source = concat!(
        "regex = Regex.compile(\"([a-z]+)-([0-9]+)\").expect(\"valid\")\n",
        "matched = regex.captures(\"ab-42\").expect(\"match\")\n",
        "(matched.group(1), regex.split(\"a-1 b-2\"), ",
        "regex.replace_all(\"a-1\", \"$1:$2\"))\n",
    );
    let artifact = lm_testkit::compile_text("jit-regex-fuel.lm", source)
        .expect("the regex fuel case compiles");
    for fuel in 0..=96 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn regex_literals_and_matches_survive_external_snapshots() {
    let source = concat!(
        "regex = re\"(?P<word>[a-z]+)\"\n",
        "regex.captures(\"42 loom 7\").expect(\"match\")\n",
    );
    let artifact = lm_testkit::compile_text("jit-regex-snapshot.lm", source)
        .expect("the regex snapshot case compiles");
    let (_, metrics, image) = run_artifact_and_capture(&artifact, EngineMode::Native, u64::MAX);
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    let (arena, namespace_id) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the snapshot program publishes");
    let namespace = arena
        .namespace(namespace_id)
        .cloned()
        .expect("the snapshot namespace exists");
    let admitted = lm_vm::snapshot::codec::load_external(
        image.bytes().expect("the snapshot encodes"),
        Some(namespace),
        lm_vm::snapshot::LoadLimits::default(),
    )
    .expect("the regex snapshot loads");
    let (event, _) = restore_with_engine(&artifact, admitted.world(), EngineMode::Interpreter);
    assert!(matches!(event, RootEvent::Done(lm_value::Value::Obj(_))));
}

#[test]
fn dynamic_regex_compilation_reports_its_pattern_limit() {
    let pattern = "a".repeat(64 * 1024 + 1);
    let source = format!(
        "case Regex.compile(\"{pattern}\")\nin Err(RegexError.LimitExceeded) then true\nin _ then false\nend\n"
    );
    let artifact = lm_testkit::compile_text("jit-regex-limit.lm", &source)
        .expect("the dynamic regex limit case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
}

#[test]
fn regex_allocations_preserve_roots_during_collection() {
    let source = concat!(
        "regex = re\"(?P<word>[a-z]+)-([0-9]+)\"\n",
        "i = 0\ntotal = 0\n",
        "while i < 400\n",
        "  matched = regex.captures(\"ab-42\").expect(\"match\")\n",
        "  total = total + matched.group(1).expect(\"group\").len()\n",
        "  total = total + regex.split(\"a-1 b-2\").len()\n",
        "  i = i + 1\n",
        "end\n",
        "total\n",
    );
    let artifact = lm_testkit::compile_text("jit-regex-collection.lm", source)
        .expect("the regex collection case compiles");
    let config = VmConfig {
        heap_bytes: 16 * 1024,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(2_000)));
    assert!(metrics.native_heap_allocations > 2_000, "{metrics:?}");
}
