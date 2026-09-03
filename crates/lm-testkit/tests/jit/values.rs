use super::*;

#[test]
fn scalar_loop_fuel_matches_the_interpreter() {
    let artifact =
        lm_testkit::compile_text("jit-fuel.lm", SCALAR_LOOP).expect("the fuel case compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(
            native, interpreted,
            "fuel {fuel}: {metrics:?}\n{native_dump}"
        );
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn completed_freeze_does_not_use_an_interpreter_site() {
    let source = concat!(
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  table = {\"value\": i}\n",
        "  table.freeze()\n",
        "  sum = sum + 1\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-interpreter-site.lm", source)
        .expect("the mixed function compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn simple_numeric_operations_stay_native() {
    let source = concat!(
        "i = 0\ntotal = 0\nsame = true\n",
        "while i < 10000\n",
        "  value = ((i & 7) | 8) ^ 3\n",
        "  value = (value << 2) >> 1\n",
        "  value = value ^ (-1 >>> 60)\n",
        "  value = value.wrapping_add(i).wrapping_sub(i)\n",
        "  value = value.wrapping_mul(3)\n",
        "  value = value.rotate_left(5).rotate_right(5)\n",
        "  value = value.rotate_left_32(5).rotate_right_32(5)\n",
        "  float = value.to_float()\n",
        "  same = same and Float.from_bits(float.bits()) == float\n",
        "  same = same and not float.is_nan()\n",
        "  total = total + value\n",
        "  i = i + 1\n",
        "end\n",
        "if same then total else 0 end\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 200_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn numeric_utility_operations_stay_native() {
    let source = concat!(
        "i = 0\ntotal = 0\nsame = true\n",
        "infinity = 1.0 / 0.0\nnan = (-1.0).sqrt()\n",
        "while i < 10000\n",
        "  value = (i - 5000).signum()\n",
        "  total = total + i.count_ones() + i.leading_zeros() + i.trailing_zeros()\n",
        "  float = i.to_float() + 0.5\n",
        "  same = same and float.abs().sqrt().is_finite()\n",
        "  same = same and float.floor().max(0.0) <= float.ceil().min(10000.0)\n",
        "  same = same and float.round().trunc().is_finite()\n",
        "  same = same and (-1.25).floor() == -2.0 and (-1.25).ceil() == -1.0\n",
        "  same = same and 2.5.round() == 2.0 and 3.5.round() == 4.0\n",
        "  same = same and (-0.0).min(0.0).bits() == (-1 << 63)\n",
        "  same = same and (-0.0).max(0.0).bits() == 0\n",
        "  same = same and nan.min(1.0).is_nan() and nan.max(1.0).is_nan()\n",
        "  same = same and not infinity.is_finite() and infinity.is_infinite()\n",
        "  same = same and nan.is_nan() and not nan.is_infinite()\n",
        "  total = total + value\n",
        "  i = i + 1\n",
        "end\n",
        "if same then total else -1 end\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 500_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn invalid_shift_amounts_replay_one_instruction() {
    for source in [
        "1 << 64\n",
        "1 >> -1\n",
        "1.rotate_left(64)\n",
        "1.rotate_right_32(32)\n",
    ] {
        let artifact = lm_testkit::compile_text("jit-shift-fault.lm", source)
            .expect("the shift case compiles");
        for fuel in 0..=4 {
            let (interpreted, _, interpreted_dump) =
                run_artifact(&artifact, EngineMode::Interpreter, fuel);
            let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
            assert_eq!(native, interpreted, "fuel {fuel}");
            assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
        }
        let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
        assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::ShiftOutOfRange));
        assert_eq!(metrics.native_interpreter_exits, 1, "{metrics:?}");
    }
}

#[test]
fn reference_equality_stays_native() {
    let source = concat!(
        "class Token\nend\n",
        "first = Token()\nalias = first\nother = Token()\n",
        "i = 0\nsame = false\n",
        "while i < 10000\n",
        "  same = first == alias\n",
        "  if first == other then same = false end\n",
        "  i = i + 1\n",
        "end\nsame\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn structural_value_equality_uses_one_typed_helper() {
    let source = concat!(
        "enum Pair\n  Value(left: Int, right: (Int, String))\nend\n",
        "left: Pair = Value(1, (2, \"loom\"))\n",
        "same: Pair = Value(1, (2, \"loom\"))\n",
        "different: Pair = Value(1, (3, \"loom\"))\n",
        "i = 0\nequal = false\n",
        "while i < 10000\n",
        "  equal = left == same and left != different\n",
        "  i = i + 1\n",
        "end\nequal\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
}

#[test]
fn structural_value_equality_matches_each_fuel_boundary() {
    let source = concat!(
        "enum Pair\n  Value(left: Int, right: Int)\nend\n",
        "left: Pair = Value(1, 2)\nright: Pair = Value(1, 2)\n",
        "left == right\n",
    );
    let artifact = lm_testkit::compile_text("jit-value-equality-fuel.lm", source)
        .expect("the value equality fuel case compiles");
    for fuel in 0..=24 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn list_contains_uses_structural_value_equality() {
    let source = concat!(
        "enum Pair\n  Value(left: Int, right: Int)\nend\n",
        "items: [Pair] = [Value(1, 2), Value(3, 4)]\n",
        "needle: Pair = Value(3, 4)\nmissing: Pair = Value(5, 6)\n",
        "i = 0\nfound = false\n",
        "while i < 10000\n",
        "  found = items.contains(needle) and not items.contains(missing)\n",
        "  i = i + 1\n",
        "end\nfound\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
}

#[test]
fn text_and_byte_comparison_helpers_stay_native() {
    let source = concat!(
        "text = \"alpha\"\nsame_text = \"alpha\"\nlater_text = \"omega\"\n",
        "bytes = b\"alpha\"\nsame_bytes = b\"alpha\"\nlater_bytes = b\"omega\"\n",
        "i = 0\nvalid = false\nhash = 0\n",
        "while i < 10000\n",
        "  valid = text == same_text and text != later_text\n",
        "  valid = valid and text < later_text and text <= same_text\n",
        "  valid = valid and later_text > text and later_text >= text\n",
        "  valid = valid and bytes == same_bytes and bytes != later_bytes\n",
        "  valid = valid and bytes < later_bytes and bytes <= same_bytes\n",
        "  valid = valid and later_bytes > bytes and later_bytes >= bytes\n",
        "  hash = hash_of(text) ^ hash_of(bytes)\n",
        "  i = i + 1\n",
        "end\n(valid, hash)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(native_dump.contains("(true,"), "{native_dump}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 300_000, "{metrics:?}");
}

#[test]
fn text_and_byte_comparisons_match_each_fuel_boundary() {
    let source = concat!(
        "text = \"alpha\"\nbytes = b\"alpha\"\n",
        "(text == \"alpha\", text < \"omega\", bytes == b\"alpha\", hash_of(bytes))\n",
    );
    let artifact = lm_testkit::compile_text("jit-value-comparison-fuel.lm", source)
        .expect("the comparison fuel case compiles");
    for fuel in 0..=40 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn class_tests_and_casts_use_the_runtime_parent_table() {
    let source = concat!(
        "class Shape\nend\n",
        "class Circle < Shape\n  radius: Int = 3\nend\n",
        "class LargeCircle < Circle\nend\n",
        "def radius(shape: Shape): Int\n",
        "  if shape is Circle then (shape as Circle).radius else 0 end\n",
        "end\n",
        "shape: Shape = LargeCircle()\ni = 0\ntotal = 0\n",
        "while i < 10000\n",
        "  total = total + radius(shape)\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(30_000)));
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn character_operations_materialize_exactly() {
    let source = concat!(
        "i = 0\ntotal = 0\nvalue = '猫'\nsame = true\n",
        "while i < 1000\n",
        "  total = total + value.codepoint() + value.utf8_len()\n",
        "  same = same and value == '猫' and value > 'a'\n",
        "  i = i + 1\n",
        "end\n",
        "if same then total else 0 end\n",
    );
    let artifact = lm_testkit::compile_text("jit-char.lm", source).expect("the Char case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(29_486_000)));
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn canonical_option_values_stay_native() {
    let source = concat!(
        "def read(value: Option[Int]): Int\n",
        "  case value\n",
        "  in Some(found) then found\n",
        "  in None then 0\n",
        "  end\n",
        "end\n",
        "i = 0\ntotal = 0\n",
        "while i < 10000\n",
        "  value: Option[Int] = if i % 2 == 0 then Some(i) else None end\n",
        "  total = total + read(value)\n",
        "  i = i + 1\n",
        "end\n",
        "nested: Option[Option[Int]] = Some(None)\n",
        "case nested\n",
        "in Some(None) then total\n",
        "in _ then 0\n",
        "end\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-option.lm", source).expect("the Option case compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(24_995_000)));
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn cached_string_and_byte_literals_stay_native() {
    let source = concat!(
        "i = 0\ntext = \"\"\nbytes = b\"\"\n",
        "while i < 10000\n",
        "  text = \"hello\"\n",
        "  bytes = b\"\\x01\\x02\"\n",
        "  i = i + 1\n",
        "end\n",
        "if text.byte_len() == 5 and bytes.len() == 2 then i else 0 end\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-literals.lm", source).expect("the literal case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(10_000)));
    assert!(metrics.native_retired_instructions > 50_000, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 4, "{metrics:?}");
}
