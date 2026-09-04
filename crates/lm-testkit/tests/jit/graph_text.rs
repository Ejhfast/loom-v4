use super::*;

#[test]
fn content_digests_match_across_engines() {
    let source = concat!(
        "use std.digest.crc32\n",
        "use std.digest.md5\n",
        "use std.digest.sha256\n",
        "input = b\"123456789\"\n",
        "i = 0\nchecksum = 0\nresult = b\"\"\nlegacy = b\"\"\n",
        "while i < 1000\n",
        "  checksum = checksum ^ crc32(input)\n",
        "  result = sha256(input)\n",
        "  legacy = md5(input)\n",
        "  i = i + 1\n",
        "end\n",
        "(checksum, result.hex(), legacy.hex())\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn byte_index_faults_match_the_interpreter() {
    for index in [-1, 4] {
        let source = format!(
            concat!(
                "def read(bytes: Bytes): Int\n",
                "  spin = 0\n",
                "  while spin < 1000\n",
                "    spin = spin + 1\n",
                "  end\n",
                "  bytes.at({})\n",
                "end\n",
                "read(Bytes(\"loom\"))\n",
            ),
            index
        );
        let (interpreted, _, interpreted_dump) = run(&source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(&source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted);
        assert_eq!(native_dump, interpreted_dump);
        assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    }
}

#[test]
fn byte_word_faults_match_the_interpreter() {
    for expression in [
        "bytes.read_u32_be(-1)",
        "bytes.read_u32_le(2)",
        "b\"abc\".read_u32_be(0)",
    ] {
        let source = format!(
            concat!(
                "def read(bytes: Bytes): Int\n",
                "  spin = 0\n",
                "  while spin < 1000\n",
                "    spin = spin + 1\n",
                "  end\n",
                "  {}\n",
                "end\n",
                "read(b\"loom\")\n",
            ),
            expression
        );
        let (interpreted, _, interpreted_dump) = run(&source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(&source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{expression}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "{expression}");
        assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    }
}

#[test]
fn checked_byte_word_reads_stay_native() {
    let source = concat!(
        "bytes = b\"loom!\"\n",
        "i = 0\ntotal = 0\nmissing = false\n",
        "while i < 1000\n",
        "  total = total ^ bytes.get_u32_be(i & 1).value_or(0)\n",
        "  missing = missing or bytes.get_u32_le(-1).is_none()\n",
        "  i = i + 1\n",
        "end\n(total, missing)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn text_metadata_and_hash_mix_stay_native() {
    let source = concat!(
        "def measure_string(value: String): Int\n",
        "  value.byte_len() * 10 + value.len()\n",
        "end\n",
        "def measure_view(value: Substring): Int\n",
        "  value.byte_len() * 10 + value.len()\n",
        "end\n",
        "text = \"aé猫z\"\n",
        "view = text.slice(1, 2).expect(\"the text slice exists\")\n",
        "i = 0\n",
        "total = 0\n",
        "hash = 0\n",
        "while i < 1000\n",
        "  total = total + measure_string(text) + measure_view(view)\n",
        "  hash = hash_combine(hash, i)\n",
        "  i = i + 1\n",
        "end\n",
        "(total, hash)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 20_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn compact_split_views_support_native_text_consumers() {
    let source = concat!(
        "pieces = \"alpha,beta,gamma\".split(\",\")\n",
        "piece = pieces.at(1)\n",
        "builder = StringBuilder()\n",
        "builder.append(piece)\n",
        "table = {\"beta\": 7}\n",
        "i = 0\ntotal = 0\n",
        "while i < 1000\n",
        "  if piece == \"beta\"\n",
        "    total = total + piece.byte_len() + table.get(piece).value_or(0)\n",
        "  end\n",
        "  i = i + 1\n",
        "end\n",
        "(builder.finish(), total, hash_of(piece))\n",
    );
    let artifact = lm_testkit::compile_text("jit-compact-text.lm", source)
        .expect("the compact text case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn map_metadata_and_digest_comparison_stay_native() {
    let source = concat!(
        "left = [1, 2, 3]\nleft.freeze()\n",
        "right = [1, 2, 3]\nright.freeze()\n",
        "left_digest = left.digest()\nright_digest = right.digest()\n",
        "table = {1: 10, 2: 20}\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + table.len()\n",
        "  if left_digest == right_digest\n",
        "    sum = sum + 1\n",
        "  end\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3_000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn graph_operations_match_each_fuel_boundary() {
    let source = concat!(
        "left = [1, 2, 3]\n",
        "right = left.freeze()\n",
        "first = right.digest()\n",
        "second = right.digest()\n",
        "first == second\n",
    );
    let artifact = lm_testkit::compile_text("jit-graph-fuel.lm", source)
        .expect("the graph fuel case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn graph_helpers_preserve_limit_and_digest_faults() {
    let freeze_source = "items = [[1], [2]]\nitems.freeze()\n";
    let artifact = lm_testkit::compile_text("jit-freeze-limit.lm", freeze_source)
        .expect("the freeze limit case compiles");
    let config = VmConfig {
        graph: lm_vm::GraphLimits {
            max_objects: 1,
            ..lm_vm::GraphLimits::default()
        },
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::BoundaryLimit));

    let digest_source = "items = [1, 2]\nitems.digest()\n";
    let (interpreted, _, interpreted_dump) = run(digest_source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(digest_source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::UnsendableValue));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 1, "{metrics:?}");
}

#[test]
fn utf8_byte_guards_match_the_interpreter() {
    let source = concat!(
        "text = \"aé猫z\"\n",
        "valid = text.slice_bytes(1, 5).is_ok()\n",
        "invalid = text.slice_bytes(2, 1).is_err()\n",
        "mapped = text.map() { |value: Char| value }\n",
        "(valid, invalid, mapped)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn utf8_scalar_reads_stay_native() {
    let source = concat!(
        "text = \"aé猫z\"\n",
        "round = 0\ntotal = 0\n",
        "while round < 1000\n",
        "  index = 0\n",
        "  while index < text.len()\n",
        "    total = total + text.at(index).expect(\"the scalar exists\").codepoint()\n",
        "    index = index + 1\n",
        "  end\n",
        "  round = round + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-utf8-scalar.lm", source)
        .expect("the scalar read case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(29_935_000)));
    assert!(metrics.native_retired_instructions > 50_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 1, "{metrics:?}");
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
}

#[test]
fn guarded_callback_conversion_matches_the_interpreter() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "stored = do |value: Int|: Int value + 1 end\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + invoke(stored)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-conversion.lm", source)
        .expect("the callback conversion compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function.blocks.iter().flatten().any(|instruction| {
            matches!(
                instruction,
                lm_bytecode::Instr::Extended(lm_bytecode::ExtendedInstr::AsCallback)
            )
        })
    }));
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42_000)));
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
}

#[test]
fn callback_slots_preserve_captures_across_native_calls() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "base = 1\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + invoke() { |value: Int| value + base }\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-slot.lm", source)
        .expect("the callback slot case compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function.blocks.iter().flatten().any(|instruction| {
            matches!(
                instruction,
                lm_bytecode::Instr::Extended(lm_bytecode::ExtendedInstr::MakeCallback { .. })
            )
        })
    }));
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42_000)));
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
}

#[test]
fn closure_and_callback_creation_match_each_fuel_boundary() {
    let source = concat!(
        "base = 1\n",
        "stored = do |value: Int|: Int value + base end\n",
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "invoke() { |value: Int| value + stored(value) }\n",
    );
    let artifact = lm_testkit::compile_text("jit-capture-allocation-fuel.lm", source)
        .expect("the capture-allocation case compiles");
    for fuel in 0..=40 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn callback_creation_preserves_scheduler_quanta() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "base = 1\n",
        "i = 0\nsum = 0\n",
        "while i < 2000\n",
        "  sum = sum + invoke() { |value: Int| value + base }\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-allocation-scheduler.lm", source)
        .expect("the scheduler callback case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the scheduler callback case publishes");
    let run = |engine: Arc<Engine>| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            engine,
        );
        let outcome = fixed_scheduler()
            .run(&mut world)
            .expect("the scheduler callback case runs");
        let retired = world.metrics().retired_instructions;
        let dump = world.dump_live(&outcome);
        (outcome, retired, dump)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let native = run(Arc::clone(&engine));
    assert_eq!(native, interpreted, "{:?}", engine.metrics());
    let metrics = engine.metrics();
    assert!(metrics.native_continuation_resumes > 0, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn native_closure_creation_preserves_the_heap_limit_fault() {
    let source = "base = 1\ndo |value: Int|: Int value + base end\n";
    let artifact = lm_testkit::compile_text("jit-closure-allocation-limit.lm", source)
        .expect("the closure-allocation case compiles");
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
