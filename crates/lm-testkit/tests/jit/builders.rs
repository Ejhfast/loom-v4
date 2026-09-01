use super::*;

#[test]
fn builders_and_byte_construction_use_dedicated_paths() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "builder.append(\"A\").append_int(-12).append_bool(true)\n",
        "builder.append_float(1.5).push_char('é')\n",
        "built_text = builder.build()\n",
        "text_size = builder.len() + builder.byte_len()\n",
        "builder.clear().append(\"done\")\n",
        "finished_text = builder.finish()\n",
        "buffer = ByteBuffer()\n",
        "buffer.reserve(8).append(1).append(2).extend(Bytes(\"AB\"))\n",
        "byte = buffer.at_or(1, 0)\n",
        "built_bytes = buffer.build()\n",
        "byte_size = buffer.len()\n",
        "buffer.clear().extend(Bytes(\"done\"))\n",
        "finished_bytes = buffer.finish()\n",
        "slice_size = case built_bytes.slice(1, 2)\n",
        "in Ok(value) then value.len()\n",
        "in Err(_) then 0\n",
        "end\n",
        "joined = built_bytes + Bytes(\"!\")\n",
        "compact = joined.compact()\n",
        "view_size = case Bytes(\"loom\").utf8_view()\n",
        "in Ok(value) then value.byte_len()\n",
        "in Err(_) then 0\n",
        "end\n",
        "left = b\"\\x0f\\xf0\"\n",
        "right = b\"\\x33\\x55\"\n",
        "bits = (left & right).len() + (left | right).len()\n",
        "bits = bits + (left ^ right).len() + (~left).len()\n",
        "(built_text.byte_len(), text_size, finished_text.byte_len(), byte, ",
        "byte_size, finished_bytes.len(), slice_size, compact.len(), view_size, bits)\n",
    );
    let artifact = lm_testkit::compile_text("jit-builders.lm", source)
        .expect("the builder construction case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(matches!(native, Outcome::Done(_)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn nested_builder_finishes_stay_native() {
    let source = concat!(
        "def finish_text(value: String): String\n",
        "  builder = StringBuilder()\n",
        "  builder.append(value)\n",
        "  builder.finish()\n",
        "end\n",
        "def finish_bytes(value: Bytes): Bytes\n",
        "  buffer = ByteBuffer()\n",
        "  buffer.extend(value)\n",
        "  buffer.finish()\n",
        "end\n",
        "i = 0\ntotal = 0\n",
        "while i < 1000\n",
        "  total = total + finish_text(\"loom\").byte_len()\n",
        "  total = total + finish_bytes(Bytes(\"loom\")).len()\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(8000)));
    assert_eq!(metrics.native_replay_exits, 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 4000, "{metrics:?}");
}

#[test]
fn nested_collection_preserves_suspended_caller_roots() {
    let source = concat!(
        "def churn(): Int\n",
        "  i = 0\n  total = 0\n",
        "  while i < 1000\n",
        "    buffer = ByteBuffer()\n",
        "    buffer.append(65)\n",
        "    total = total + buffer.finish().len()\n",
        "    i = i + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "def outer(): Int\n",
        "  kept = [41]\n",
        "  made = churn()\n",
        "  kept.at(0) + made\n",
        "end\n",
        "outer()\n",
    );
    let artifact = lm_testkit::compile_text("jit-nested-roots.lm", source)
        .expect("the nested root case compiles");
    let config = VmConfig {
        heap_bytes: 4096,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1041)));
    assert_eq!(metrics.native_replay_exits, 0, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 2000, "{metrics:?}");
}

#[test]
fn direct_json_pipeline_preserves_native_roots_during_collection() {
    let source = r##"
use std.json.Json
use std.json.parse
use std.json.stringify

builder = StringBuilder()
builder.append("[")
seed = 31337
index = 0
while index < 10
  if index > 0
    builder.append(",")
  end
  seed = (seed * 1103515245 + 12345) % 2147483648
  score = seed % 1000
  builder.append("{\"id\":")
  builder.append_int(index)
  builder.append(",\"name\":\"user")
  builder.append_int(index)
  builder.append("\",\"score\":")
  builder.append_int(score)
  builder.append(",\"active\":")
  if score % 2 == 0
    builder.append("true")
  else
    builder.append("false")
  end
  builder.append("}")
  index = index + 1
end
builder.append("]")
source = builder.build()

total = 0
length = 0
round = 0
while round < 2
  case parse(source)
  in Ok(document)
    case document
    in Json.ListValue(items)
      for item in items
        case item
        in Json.Object(fields)
          case fields.get("score")
          in Some(Json.Number(value))
            if value >= 500.0
              total = total + 1
            end
          in _ then total = total - 1000000
          end
        in _ then total = total - 1000000
        end
      end
      case stringify(document)
      in Ok(text) then length = length + text.len()
      in Err(_) then total = total - 1000000
      end
    in _ then total = total - 1000000
    end
  in Err(_) then total = total - 1000000
  end
  round = round + 1
end
(total, length)
"##;
    let artifact =
        lm_testkit::compile_text("jit-json-roots.lm", source).expect("the JSON root case compiles");
    let config = VmConfig {
        heap_bytes: 32 * 1024,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert!(matches!(interpreted, Outcome::Done(_)));
    assert!(matches!(native, Outcome::Done(_)));
    assert!(
        interpreted_dump.starts_with("outcome: Done((10, 1010))\n"),
        "{interpreted_dump}"
    );
    assert!(
        native_dump.starts_with("outcome: Done((10, 1010))\n"),
        "{native_dump}"
    );
    assert!(metrics.native_collection_slow_paths > 0, "{metrics:?}");
}

#[test]
fn builder_construction_matches_each_fuel_boundary() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "text = builder.append(\"loom\").append_int(-12).append_bool(false).push_char('é').build()\n",
        "buffer = ByteBuffer()\n",
        "bytes = buffer.reserve(8).append(1).extend(Bytes(text)).build()\n",
        "(text, bytes)\n",
    );
    let artifact = lm_testkit::compile_text("jit-builder-fuel.lm", source)
        .expect("the builder fuel case compiles");
    for fuel in 0..=48 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn native_builder_formats_integer_bounds_and_unicode_widths() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "builder.append_int(0).append(\",\").append_int(-1).append(\",\")\n",
        "builder.append_int(-9223372036854775807 - 1).append(\",\")\n",
        "builder.append_int(9223372036854775807)\n",
        "builder.push_char('A').push_char('é').push_char('猫').push_char('😀')\n",
        "builder.finish() == \"0,-1,-9223372036854775808,9223372036854775807Aé猫😀\"\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn builder_faults_match_the_interpreter() {
    let cases = [
        (
            "buffer = ByteBuffer()\nbuffer.append(256)\n",
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            "buffer = ByteBuffer()\nbuffer.reserve(-1)\n",
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            "builder = StringBuilder()\nbuilder.finish()\nbuilder.len()\n",
            lm_vm::FaultCode::InvalidVmState,
        ),
        (
            "buffer = ByteBuffer()\nbuffer.finish()\nbuffer.len()\n",
            lm_vm::FaultCode::InvalidVmState,
        ),
    ];
    for (source, expected) in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{metrics:?}");
        assert_eq!(native_dump, interpreted_dump);
        assert_eq!(native, Outcome::Fault(expected));
    }
}

#[test]
fn builder_growth_preserves_the_heap_limit() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "i = 0\n",
        "while i < 10000\n",
        "  builder.append(\"abcdefgh\")\n",
        "  i = i + 1\n",
        "end\n",
        "builder.len()\n",
    );
    let artifact = lm_testkit::compile_text("jit-builder-limit.lm", source)
        .expect("the builder limit case compiles");
    let config = VmConfig {
        heap_bytes: 1024,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::HeapLimit));
}

#[test]
fn text_byte_and_conversion_algorithms_use_dedicated_paths() {
    let source = concat!(
        "base: Text = \"  Ab,é,,C  \"\n",
        "joined = base.concat(\"!\")\n",
        "checks = (base.starts_with(\"  A\"), base.ends_with(\"  \"), ",
        "base.contains(\"é\"), base.find(\"é\"), base.find_bytes(\"é\"))\n",
        "trimmed = (base.trim(), base.trim_start(), base.trim_end())\n",
        "mapped = (base.to_lower_ascii(), base.to_upper_ascii())\n",
        "replaced = base.replace(\",\", \"|\")\n",
        "padded = (\"x\".pad_start(3), \"x\".pad_end(3))\n",
        "pieces = \"a,b,,c\".split(\",\")\n",
        "lines = \"a\\r\\nb\\n\".lines()\n",
        "scalar_slice = case base.slice(2, 2)\n",
        "in Ok(value) then value.to_string()\n",
        "in Err(_) then \"bad\"\n",
        "end\n",
        "byte_slice = case base.slice_bytes(2, 2)\n",
        "in Ok(value) then value.to_string()\n",
        "in Err(_) then \"bad\"\n",
        "end\n",
        "bytes = base.bytes()\n",
        "byte_checks = (bytes.starts_with(Bytes(\"  A\")), bytes.ends_with(Bytes(\"  \")), ",
        "bytes.contains(Bytes(\"é\")), bytes.find(Bytes(\"é\")), bytes.hex(), bytes.utf8())\n",
        "buffer = ByteBuffer().extend(Bytes(\"abcabc\"))\n",
        "buffer_find = buffer.find_from(Bytes(\"bc\"), 2)\n",
        "numbers = (\"7f\".parse_int(16), \"bad\".parse_int(10), ",
        "\"1.25\".parse_float(), \"bad\".parse_float(), 12.5.fixed(2))\n",
        "(joined, checks, trimmed, mapped, replaced, padded, pieces, lines, ",
        "scalar_slice, byte_slice, byte_checks, buffer_find, numbers)\n",
    );
    let artifact = lm_testkit::compile_text("jit-text-algorithms.lm", source)
        .expect("the text algorithm case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(matches!(native, Outcome::Done(_)));
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn text_algorithms_match_each_fuel_boundary() {
    let source = concat!(
        "text = \"  abc,def  \"\n",
        "bytes = text.trim().to_string().bytes()\n",
        "parts = text.split(\",\")\n",
        "(text.replace(\"abc\", \"ABC\"), bytes.hex(), parts, 1.25.fixed(3))\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-text-fuel.lm", source).expect("the text fuel case compiles");
    for fuel in 0..=96 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn text_conversion_faults_match_the_interpreter() {
    let cases = [
        ("1.5.fixed(-1)\n", lm_vm::FaultCode::InvalidPrecision),
        ("b\"\\xff\".text()\n", lm_vm::FaultCode::BadCast),
    ];
    for (source, expected) in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{metrics:?}");
        assert_eq!(native_dump, interpreted_dump);
        assert_eq!(native, Outcome::Fault(expected));
    }
}

#[test]
fn a_native_callee_grows_shared_root_storage() {
    let source = concat!(
        "def wide(seed: String): Int\n",
        "  a = seed\n  b = seed\n  c = seed\n  d = seed\n",
        "  e = seed\n  f = seed\n  g = seed\n  h = seed\n",
        "  i = seed\n  j = seed\n  k = seed\n  l = seed\n",
        "  builder = StringBuilder()\n",
        "  builder.append(a).append(b).append(c).append(d)\n",
        "  builder.append(e).append(f).append(g).append(h)\n",
        "  builder.append(i).append(j).append(k).append(l)\n",
        "  builder.len()\nend\n",
        "wide(\"x\")\n",
    );
    let artifact = lm_testkit::compile_text("jit-root-growth.lm", source)
        .expect("the root growth case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the root growth case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let run = || {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        lm_proc::run_world(&mut world)
    };
    assert_eq!(run(), Outcome::Done(lm_value::Value::Int(12)));
    engine.reset_metrics();
    assert_eq!(run(), Outcome::Done(lm_value::Value::Int(12)));
    let metrics = engine.metrics();
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert_eq!(metrics.backend_unavailable_fallbacks, 0, "{metrics:?}");
}
