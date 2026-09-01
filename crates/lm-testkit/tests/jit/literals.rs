use super::*;

#[test]
fn tuple_and_list_literals_stay_native() {
    let source = concat!(
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  pair = (i, i + 1)\n",
        "  items = [pair[0], pair[1]]\n",
        "  sum = sum + items[0] + items[1]\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-value-array-allocation.lm", source)
        .expect("the value-array allocation case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000_000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 2000, "{metrics:?}");
}

#[test]
fn tuple_and_list_literals_match_each_fuel_boundary() {
    let source = "pair = (1, 2)\nitems = [pair[0], pair[1]]\nitems[0] + items[1]\n";
    let artifact = lm_testkit::compile_text("jit-value-array-allocation-fuel.lm", source)
        .expect("the value-array fuel case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn allocation_retry_preserves_capture_and_array_inputs() {
    let source = concat!(
        "class Token\n  value: Int = 0\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\nend\n",
        "token = Token(41)\n",
        "saved = do ||: Int token.value end\n",
        "items = [token]\n",
        "i = 0\n",
        "while i < 1000\n",
        "  saved = do ||: Int token.value end\n",
        "  pair = (token, i)\n",
        "  items = [pair[0]]\n",
        "  i = i + 1\n",
        "end\n",
        "saved() + items[0].value\n",
    );
    let artifact = lm_testkit::compile_text("jit-allocation-retry-roots.lm", source)
        .expect("the allocation retry case compiles");
    let config = VmConfig {
        heap_bytes: 4096,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(82)));
    assert!(metrics.native_collection_slow_paths > 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
}

#[test]
fn tuple_allocation_preserves_the_heap_limit_fault() {
    let artifact = lm_testkit::compile_text("jit-tuple-allocation-limit.lm", "(1, 2)\n")
        .expect("the tuple allocation limit case compiles");
    let config = VmConfig {
        heap_bytes: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::HeapLimit));
    assert!(metrics.native_entries > 0, "{metrics:?}");
}

#[test]
fn map_literals_stay_native() {
    let source = concat!(
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  table = {\"a\": i, \"a\": i + 1, \"b\": i + 2}\n",
        "  sum = sum + table.len()\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-allocation.lm", source)
        .expect("the map allocation case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(2000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_allocations > 900, "{metrics:?}");
}

#[test]
fn map_literals_match_each_fuel_boundary() {
    let source = "table = {\"a\": 1, \"b\": 2}\ntable.len()\n";
    let artifact = lm_testkit::compile_text("jit-map-allocation-fuel.lm", source)
        .expect("the map allocation fuel case compiles");
    for fuel in 0..=24 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}
