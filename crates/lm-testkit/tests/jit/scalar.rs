use super::*;

#[test]
fn scalar_loop_matches_the_interpreter() {
    let (interpreted, _, _) = run(SCALAR_LOOP, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, _) = run(SCALAR_LOOP, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(49_995_000)));
    assert_eq!(metrics.compiled_regions, 1);
    assert!(metrics.native_retired_instructions > 100_000);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn byte_reads_match_the_interpreter() {
    let source = concat!(
        "def sum_bytes(bytes: Bytes): Int\n",
        "  total = 0\n",
        "  pass = 0\n",
        "  while pass < 1000\n",
        "    index = 0\n",
        "    while index < bytes.len()\n",
        "      total = total + bytes.at(index)\n",
        "      index = index + 1\n",
        "    end\n",
        "    pass = pass + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "sum_bytes(Bytes(\"loom\"))\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(439_000)));
    assert!(metrics.native_retired_instructions > 50_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn syntax_operations_match_the_interpreter() {
    let source = r#"
def inspect_syntax(): Bool
  builder = SyntaxBuilder()
  children = List[SyntaxElement]()
  children.push(builder.integer("40"))
  children.push(builder.whitespace(" "))
  children.push(builder.plus())
  children.push(builder.whitespace(" "))
  children.push(builder.integer("2"))
  node = builder.statement(children)
  root = node.to_tree().root()
  detached = root.detach()
  root.kind() == 7 and
    root.category() == 0 and
    root.range_start() == 0 and
    root.range_end() == 6 and
    root.text() == "40 + 2" and
    root.children().len() == 5 and
    detached.text() == "40 + 2"
end

inspect_syntax()
"#;
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert!(metrics.native_retired_instructions > 20, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn dynamic_result_packaging_matches_each_fuel_boundary() {
    let compiled = compile_module_with_options(
        "jit-dynamic-result",
        &SourceFile::new("jit-dynamic-result.lm", "(41, \"loom\")\n"),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().dynamic_result(),
    )
    .expect("the dynamic result case compiles");
    let artifact =
        lm_testkit::artifact_from_compiled(compiled).expect("the dynamic artifact builds");
    for fuel in 0..=8 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (_, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert!(metrics.native_allocations > 0, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}
