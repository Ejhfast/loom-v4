use super::*;

#[test]
fn native_field_reads_resume_from_interpreter_created_state() {
    let source = concat!(
        "class Pair\n",
        "  left: Int\n",
        "  def init(mut self, left: Int)\n    self.left = left\n  end\n",
        "end\n",
        "pair = Pair(7)\ni = 0\nsum = 0\n",
        "while i < 10000\n",
        "  value = pair.left\n  sum = sum + value\n  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-field.lm", source).expect("the field case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the field case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 32),
        Some(lm_vm::SliceExit::Yielded)
    ));
    engine.set_mode(EngineMode::Native);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(70_000))
    );
    let metrics = engine.metrics();
    assert!(metrics.native_retired_instructions > 50_000);
    assert_eq!(metrics.compiled_heap_read_sites, 1);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn direct_heap_access_matches_each_fuel_boundary() {
    let source = concat!(
        "class Cell\n",
        "  value: Int = 0\n",
        "end\n",
        "def bump(mut cell: Cell): Int\n",
        "  cell.value = cell.value + 1\n",
        "  cell.value\n",
        "end\n",
        "cell = Cell()\npair = (7, 8)\nitems = [1, 2, 3]\n",
        "i = 0\nsum = 0\n",
        "while i < 20\n",
        "  bump(cell)\n",
        "  index = i % 3\n",
        "  items.set(index, pair[0] + i)\n",
        "  sum = sum + items.at(index)\n",
        "  i = i + 1\n",
        "end\n",
        "sum + cell.value + pair[1] + items.len()\n",
    );
    let artifact = lm_testkit::compile_text("jit-direct-heap.lm", source)
        .expect("the direct heap case compiles");
    for fuel in 0..=96 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(361)));
    assert!(metrics.compiled_heap_read_sites >= 5, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100, "{metrics:?}");
}

#[test]
fn heap_proofs_end_when_a_local_changes() {
    let source = concat!(
        "class Cell\n",
        "  value: Int\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\n",
        "end\n",
        "left = Cell(1)\nright = Cell(10)\ncurrent = left\n",
        "i = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + current.value\n",
        "  if i == 49 then current = right end\n",
        "  i = i + 1\n",
        "end\n",
        "current = left\n",
        "current.value = if true then current = right; 7 else 7 end\n",
        "sum + current.value * 10 + left.value\n",
    );
    let artifact = lm_testkit::compile_text("jit-heap-proof-local.lm", source)
        .expect("the heap proof case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(657)));
    assert!(metrics.compiled_heap_read_sites > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 500, "{metrics:?}");
}

#[test]
fn cached_list_data_ends_when_a_local_changes() {
    let source = concat!(
        "left = [1]\nright = [2]\ncurrent = left\n",
        "i = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + current.at(0)\n",
        "  if i == 49 then current = right end\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-list-data-local.lm", source)
        .expect("the list data case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(150)));
    assert!(metrics.native_retired_instructions > 500, "{metrics:?}");
}

#[test]
fn direct_collection_metadata_matches_selected_fuel_boundaries() {
    let source = concat!(
        "items = [1, 2, 3]\n",
        "capacity = items.capacity()\n",
        "sum = 0\n",
        "for item in items\n",
        "  sum = sum + item\n",
        "end\n",
        "if capacity < 3 then -1000 else sum end\n",
    );
    let artifact = lm_testkit::compile_text("jit-collection-metadata.lm", source)
        .expect("the collection metadata case compiles");
    for fuel in [0, 1, 2, 3, 4, 5, 8, 13, 21, 34, 55, 89] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(6)));
    assert!(metrics.compiled_heap_read_sites >= 3, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 1, "{metrics:?}");
}

#[test]
fn list_reserve_and_reorder_stay_native() {
    let source = concat!(
        "items = [4, 1, 3, 2]\n",
        "items.reserve(32)\n",
        "items.sort()\n",
        "items[0] * 100 + items[3]\n",
    );
    let artifact = lm_testkit::compile_text("jit-list-reserve.lm", source)
        .expect("the list reserve case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(104)));
    assert!(metrics.compiled_heap_write_sites >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn native_list_iteration_detects_structural_changes() {
    let source = concat!(
        "items = [1, 2, 3]\n",
        "alias = items\n",
        "for item in items\n",
        "  alias.push(item)\n",
        "end\n",
        "0\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::CollectionModified));
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn frozen_instance_sealing_stays_native() {
    let source = concat!(
        "frozen class Token\n",
        "  value: Int\n",
        "  def init(mut self, value: Int)\n",
        "    self.value = value\n",
        "  end\n",
        "end\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  token = Token(i)\n",
        "  sum = sum + token.value\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(
        native, interpreted,
        "{metrics:?}\n{native_dump}\n{interpreted_dump}"
    );
    // Unreachable garbage is not part of a terminal snapshot.
    assert!(native_dump.contains("frames: 0 active"));
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(499_500)));
    assert_eq!(metrics.native_allocations, 0, "{metrics:?}");
    assert!(metrics.scalar_replaced_allocations >= 1000, "{metrics:?}");
    assert_eq!(metrics.pending_instance_allocations, 0, "{metrics:?}");
    assert_eq!(metrics.pending_instance_materializations, 0, "{metrics:?}");
    assert_eq!(metrics.compiled_heap_write_sites, 0, "{metrics:?}");
}

#[test]
fn native_class_initialization_releases_each_call_frame() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "i = 0\ns = 0\nwhile i < 50000\n",
        "  p = Point(i, i + 1)\n  s = s + p.x\n  i = i + 1\n",
        "end\ns\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, native_metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(
        native, interpreted,
        "{native_metrics:?}\n{native_dump}\n{interpreted_dump}"
    );
    // Scalar replacement keeps fields in generated SSA values.
    assert!(native_dump.contains("frames: 0 active"));
    assert_eq!(native_metrics.native_allocations, 0, "{native_metrics:?}");
    assert_eq!(
        native_metrics.pending_instance_allocations, 0,
        "{native_metrics:?}"
    );
    assert!(
        native_metrics.scalar_replaced_allocations > 40_000,
        "{native_metrics:?}"
    );
    let (automatic, automatic_metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(
        automatic, interpreted,
        "{automatic_metrics:?}\n{automatic_dump}"
    );
    assert!(automatic_dump.contains("frames: 0 active"));
    assert_eq!(
        automatic_metrics.native_allocations, 0,
        "{automatic_metrics:?}"
    );
    assert!(
        automatic_metrics.scalar_replaced_allocations > 40_000,
        "{automatic_metrics:?}"
    );
    assert!(
        automatic_metrics.native_retired_instructions > 500_000,
        "{automatic_metrics:?}"
    );
}
