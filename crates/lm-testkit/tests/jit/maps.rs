use super::*;

#[test]
fn text_map_hits_stay_native() {
    let source = concat!(
        "table = {\"a\": 3, \"b\": 5}\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  if table.has(\"a\")\n",
        "    sum = sum + table.at(\"a\")\n",
        "  end\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-lookup.lm", source)
        .expect("the map lookup case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
}

#[test]
fn text_map_hits_match_each_fuel_boundary() {
    let source = concat!(
        "table = {\"a\": 3}\n",
        "if table.has(\"a\") then table.at(\"a\") else 0 end\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-lookup-fuel.lm", source)
        .expect("the map lookup fuel case compiles");
    for fuel in 0..=24 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn text_map_hits_match_string_and_substring_keys() {
    let source = concat!(
        "source = \"_key_\"\n",
        "view = source.slice(1, 3).expect(\"the view exists\")\n",
        "first = Map[Text, Int]()\nfirst.put(\"key\", 7)\n",
        "second = Map[Text, Int]()\nsecond.put(view, 11)\n",
        "(first.at(view), second.at(\"key\"), first.has(\"absent\"))\n",
    );
    let artifact = lm_testkit::compile_text("jit-text-map-hits.lm", source)
        .expect("the text map-hit case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn borrowed_text_map_put_matches_both_engines() {
    let source = concat!(
        "stored_source = \"_key_\"\nstored = stored_source.slice(1, 3).expect(\"view\")\n",
        "other_source = \"_other_\"\nother = other_source.slice(1, 5).expect(\"view\")\n",
        "table = Map[String, Int]()\nfirst = table.put(stored, 1)\n",
        "i = 0\nsum = 0\nwhile i < 1000\n",
        "  case table.put(stored, i)\n",
        "  in Some(previous) then sum = sum + previous\n",
        "  in None then sum = sum - 10000\n",
        "  end\n  i = i + 1\nend\n",
        "table.put(other, 7)\n(first, sum, table.at(\"key\"), table.at(\"other\"))\n",
    );
    let artifact = lm_testkit::compile_text("jit-borrowed-map-put.lm", source)
        .expect("the borrowed map-put case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
}

#[test]
fn byte_map_hits_compare_distinct_storage() {
    let source = concat!(
        "stored = Bytes(\"key\")\nlookup = Bytes(\"key\")\n",
        "table: {Bytes: Int} = {}\ntable.put(stored, 13)\n",
        "(table.at(lookup), table.has(Bytes(\"absent\")))\n",
    );
    let artifact = lm_testkit::compile_text("jit-byte-map-hits.lm", source)
        .expect("the byte map-hit case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn scalar_map_hits_match_each_key_representation() {
    let source = concat!(
        "ints: {Int: Int} = {-7: 11, 9: 13}\n",
        "bools: {Bool: Int} = {false: 17, true: 19}\n",
        "chars: {Char: Int} = {'a': 23, '猫': 29}\n",
        "floats: {Float: Int} = {-0.0: 31, 2.5: 37}\n",
        "(ints.at(-7), ints.has(9), bools.at(true), ",
        "chars.at('猫'), floats.at(0.0), floats.has(2.5))\n",
    );
    let artifact = lm_testkit::compile_text("jit-scalar-map-hits.lm", source)
        .expect("the scalar map-hit case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn scalar_map_probe_handles_collisions_misses_and_tombstones() {
    let source = concat!(
        "table: {Int: Int} = {}\ni = 0\n",
        "while i < 64\n  table.put(i, i * 3)\n  i = i + 1\nend\n",
        "removed = table.remove(17)\ni = 0\nsum = 0\n",
        "while i < 64\n",
        "  if table.has(i)\n    sum = sum + table.at(i)\n  end\n",
        "  i = i + 1\n",
        "end\n(sum, removed, table.has(1000), table.has(17))\n",
    );
    let artifact = lm_testkit::compile_text("jit-scalar-map-probe.lm", source)
        .expect("the scalar map-probe case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn scalar_map_hits_match_each_fuel_boundary() {
    let source = "table: {Int: Int} = {3: 5}\nif table.has(3) then table.at(3) else 0 end\n";
    let artifact = lm_testkit::compile_text("jit-scalar-map-fuel.lm", source)
        .expect("the scalar map fuel case compiles");
    for fuel in 0..=24 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn map_put_uses_direct_replacement_and_checked_insertion() {
    let source = concat!(
        "table: {String: Int} = {}\ni = 0\nsum = 0\n",
        "while i < 1000\n",
        "  case table.put(\"value\", i)\n",
        "  in Some(previous) then sum = sum + previous\n",
        "  in None then sum = sum + 0\n",
        "  end\n",
        "  table.put(\"discard\", i)\n",
        "  i = i + 1\n",
        "end\n",
        "(sum, table.at(\"value\"), table.at(\"discard\"))\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-put.lm", source)
        .expect("the map insertion case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(native_dump.contains("(498501, 999, 999)"), "{native_dump}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 25_000, "{metrics:?}");
}

#[test]
fn map_put_inserts_scalar_text_and_byte_keys_into_spare_storage() {
    let source = concat!(
        "ints: {Int: Int} = {}\nints.put(1, 10)\nint_old = ints.put(2, 20)\n",
        "text_key = \"next\"\ntext_seed: {Text: Int} = {text_key: 0}\n",
        "text: {Text: Int} = {\"base\": 1}\ntext_old = text.put(text_key, 30)\n",
        "byte_key = Bytes(\"next\")\nbyte_seed: {Bytes: Int} = {byte_key: 0}\n",
        "bytes: {Bytes: Int} = {Bytes(\"base\"): 1}\n",
        "byte_old = bytes.put(byte_key, 40)\n",
        "(int_old, text_old, byte_old, ints.at(2), text.at(text_key), bytes.at(byte_key))\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-put-spare.lm", source)
        .expect("the spare map insertion case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(native_dump.contains("(None, None, None, 20, 30, 40)"));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn optional_map_reads_use_direct_scalar_text_and_byte_hits() {
    let source = concat!(
        "ints: {Int: Int} = {3: 5}\n",
        "text: {Text: Int} = {\"key\": 7}\n",
        "view = \"_key_\".slice(1, 3).expect(\"the view exists\")\n",
        "stored = Bytes(\"bytes\")\nlookup = Bytes(\"bytes\")\n",
        "bytes: {Bytes: Int} = {stored: 11}\n",
        "(ints.get(3), ints.get(4), text.get(view), bytes.get(lookup))\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-get-direct.lm", source)
        .expect("the direct map-get case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn map_insertions_match_each_fuel_boundary() {
    let source = concat!(
        "table: {String: Int} = {}\n",
        "first = table.put(\"a\", 1)\n",
        "second = table.put(\"a\", 2)\n",
        "table.put(\"b\", 3)\n",
        "(first, second, table.at(\"a\"), table.at(\"b\"))\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-put-fuel.lm", source)
        .expect("the map insertion fuel case compiles");
    for fuel in 0..=40 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn map_insertions_preserve_heap_limit_and_frozen_faults() {
    let limit_source = concat!(
        "table: {Int: Int} = {}\ni = 0\n",
        "while i < 1000\n",
        "  table.put(i, i)\n",
        "  i = i + 1\n",
        "end\ntable.len()\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-put-limit.lm", limit_source)
        .expect("the map insertion limit case compiles");
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

    let frozen_source = concat!(
        "table = {\"a\": 1}\n",
        "table.freeze()\n",
        "table.put(\"b\", 2)\n",
    );
    let (interpreted, _, interpreted_dump) = run(frozen_source, EngineMode::Interpreter, u64::MAX);
    let (native, _, native_dump) = run(frozen_source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::FrozenWrite));
}

#[test]
fn map_removal_uses_direct_keys_and_preserves_compaction() {
    let source = concat!(
        "ints: {Int: Int} = {}\ni = 0\n",
        "while i < 24\n  ints.put(i, i * 2)\n  i = i + 1\nend\n",
        "i = 0\nsum = 0\nwhile i < 10\n",
        "  sum = sum + ints.remove(i).expect(\"the key exists\")\n",
        "  i = i + 1\nend\n",
        "view = \"_key_\".slice(1, 3).expect(\"the view exists\")\n",
        "text: {Text: Int} = {\"key\": 31}\ntext_value = text.remove(view)\n",
        "stored = Bytes(\"bytes\")\nlookup = Bytes(\"bytes\")\n",
        "bytes: {Bytes: Int} = {stored: 37}\nbyte_value = bytes.remove(lookup)\n",
        "(sum, ints.len(), ints.get(9), ints.get(10), text_value, byte_value)\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-remove-direct.lm", source)
        .expect("the direct map-removal case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(
        native_dump.contains("(90, 14, None, Some(20), Some(31), Some(37))"),
        "{native_dump}"
    );
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

const MAP_MUTATION_SURFACE: &str = r#"
final class Key implements Hashable
  value: Int

  def init(mut self, value: Int)
    self.value = value
  end

  def __eq__(self, other: Key): Bool
    self.value == other.value
  end

  def __hash__(self): Int
    self.value % 2
  end
end

first = Key(1).freeze()
same = Key(1).freeze()
collision = Key(3).freeze()
raw = Map[Key, Int]()
raw.reserve(4)
raw.put(first, 1)
raw.put(collision, 3)
replaced = raw.put(same, 2)
found = raw.get(same)
removed = raw.remove(collision)
raw_sum = 0
for _, value in raw
  raw_sum = raw_sum + value
end

table = {"a": 4, "b": 5}
table.reserve(8)
direct = table.get("a")
removed_text = table.remove("b")
missing = table.get("b")
direct_sum = 0
for _, value in table
  direct_sum = direct_sum + value
end
table.clear()
(found, replaced, removed, raw_sum, direct, removed_text, missing, direct_sum, table.len())
"#;

#[test]
fn map_mutation_and_probe_operations_have_dedicated_treatments() {
    let artifact = lm_testkit::compile_text("jit-map-mutations.lm", MAP_MUTATION_SURFACE)
        .expect("the map mutation case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert!(
        native_dump.contains("(Some(2), Some(1), Some(3), 2, Some(4), Some(5), None, 4, 0)"),
        "{native_dump}"
    );
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 8, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100, "{metrics:?}");
}

#[test]
fn map_mutation_and_probe_operations_match_fuel_boundaries() {
    let artifact = lm_testkit::compile_text("jit-map-mutation-fuel.lm", MAP_MUTATION_SURFACE)
        .expect("the map mutation fuel case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn missing_native_map_key_preserves_the_fault_state() {
    let source = concat!(
        "table = {\"a\": 3}\n",
        "i = 0\nsum = 0\n",
        "while i <= 100\n",
        "  key = if i < 100 then \"a\" else \"missing\" end\n",
        "  sum = sum + table.at(key)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-missing-key.lm", source)
        .expect("the missing map key case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::MissingKey));
    assert!(metrics.native_fault_exits > 0, "{metrics:?}");
}
