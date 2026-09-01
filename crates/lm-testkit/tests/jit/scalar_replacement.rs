use super::*;

#[test]
fn scalar_replaced_construction_matches_each_fuel_boundary() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "i = 0\ns = 0\nwhile i < 3\n",
        "  p = Point(i, i + 1)\n  x = p.x\n  y = p.y\n",
        "  s = s + x + y\n  i = i + 1\n",
        "end\ns\n",
    );
    let artifact = lm_testkit::compile_text("jit-scalar-instance-fuel.lm", source)
        .expect("the scalar instance case compiles");
    for fuel in 0..=160 {
        let (interpreted, _, interpreted_image) =
            run_artifact_and_capture(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_image) =
            run_artifact_and_capture(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(
            lm_vm::snapshot::dump::diff(&native_image, &interpreted_image),
            None,
            "fuel {fuel}: {metrics:?}"
        );
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(9)));
    assert_eq!(metrics.native_heap_allocations, 0, "{metrics:?}");
    assert_eq!(metrics.native_allocations, 0, "{metrics:?}");
    assert_eq!(metrics.scalar_replaced_allocations, 3, "{metrics:?}");
}

#[test]
fn scalar_replacement_preserves_the_heap_limit() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "point = Point(20, 22)\npoint.x\n",
    );
    let artifact = lm_testkit::compile_text("jit-scalar-instance-limit.lm", source)
        .expect("the scalar instance limit case compiles");
    let config = VmConfig {
        heap_bytes: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::HeapLimit));
    assert_eq!(metrics.scalar_replaced_allocations, 0, "{metrics:?}");
}

#[test]
fn scalar_replacement_preserves_the_frame_limit() {
    let source = concat!(
        "class Point\n  x: Int = 0\n",
        "  def init(mut self, x: Int)\n    self.x = x\n  end\nend\n",
        "point = Point(42)\npoint.x\n",
    );
    let artifact = lm_testkit::compile_text("jit-scalar-instance-frame-limit.lm", source)
        .expect("the scalar instance frame-limit case compiles");
    let config = VmConfig {
        max_frames: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::StackLimit));
    assert_eq!(metrics.scalar_replaced_allocations, 0, "{metrics:?}");
}

#[test]
fn escaping_constructor_results_keep_canonical_payloads() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "def make(x: Int, y: Int): Point\n  Point(x, y)\nend\n",
        "point = make(20, 22)\npoint.x + point.y\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(
        native, interpreted,
        "{metrics:?}\n{native_dump}\n{interpreted_dump}"
    );
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42)));
    assert_eq!(metrics.pending_instance_allocations, 0, "{metrics:?}");
    assert!(metrics.native_allocations > 0, "{metrics:?}");
}

#[test]
fn pending_instance_aliases_release_one_object() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "i = 0\nsum = 0\nwhile i < 1000\n",
        "  point = Point(i, i + 1)\n",
        "  left = point\n  right = left\n",
        "  sum = sum + point.x + left.y + right.x\n",
        "  i = i + 1\nend\nsum\n",
    );
    let (interpreted, _, _) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_499_500)));
    assert!(metrics.pending_instance_allocations >= 900, "{metrics:?}");
    assert_eq!(
        metrics.pending_instance_releases, metrics.pending_instance_allocations,
        "{metrics:?}"
    );
    assert_eq!(metrics.pending_instance_materializations, 0, "{metrics:?}");
}

#[test]
fn pending_instances_materialize_at_fuel_boundaries() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "first = Point(1, 2)\n",
        "point = Point(20, 22)\n",
        "i = 0\nwhile i < 20\n  i = i + 1\nend\n",
        "point.x + point.y + i\n",
    );
    let artifact = lm_testkit::compile_text("jit-pending-fuel.lm", source)
        .expect("the pending instance fuel case compiles");
    let mut materialized = false;
    for fuel in 0..=240 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        if metrics.pending_instance_materializations > 0 {
            materialized = true;
            assert_eq!(native_dump, interpreted_dump, "fuel {fuel}: {metrics:?}");
        }
    }
    assert!(
        materialized,
        "no fuel boundary materialized the pending object"
    );
}

#[test]
fn pending_instances_materialize_at_scheduler_polls() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "first = Point(1, 2)\n",
        "point = Point(20, 22)\n",
        "i = 0\nwhile i < 1000\n  i = i + 1\nend\n",
        "point.x + point.y + i\n",
    );
    let artifact = lm_testkit::compile_text("jit-pending-poll.lm", source)
        .expect("the pending instance poll case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the pending instance poll case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
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
    let control = lm_vm::ExecutionControl::new();
    control.request_yield();
    let mut metrics = engine.metrics();
    for _ in 0..16 {
        assert!(matches!(
            world.drive_slice_polled(root, u32::MAX, 64, &control),
            Some(lm_vm::SliceExit::Yielded)
        ));
        metrics = engine.metrics();
        if metrics.pending_instance_materializations > 0 {
            break;
        }
    }
    assert!(metrics.pending_instance_allocations > 0, "{metrics:?}");
    assert!(metrics.pending_instance_materializations > 0, "{metrics:?}");
    control.clear_yield();
    assert_eq!(world.run_root(), Outcome::Done(lm_value::Value::Int(1042)));
}

#[test]
fn pending_instance_allocation_preserves_the_heap_limit() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "Point(20, 22).x\n",
    );
    let artifact = lm_testkit::compile_text("jit-pending-limit.lm", source)
        .expect("the pending instance heap-limit case compiles");
    let config = VmConfig {
        heap_bytes: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(
        native, interpreted,
        "{metrics:?}\n{native_dump}\n{interpreted_dump}"
    );
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::HeapLimit));
    assert_eq!(metrics.pending_instance_allocations, 0, "{metrics:?}");
}

#[test]
fn pending_instance_pool_exhaustion_materializes_live_objects() {
    let mut source = concat!(
        "class Point\n  x: Int = 0\n",
        "  def init(mut self, x: Int)\n    self.x = x\n  end\nend\n",
    )
    .to_string();
    for value in 0..80 {
        source.push_str(&format!("point_{value} = Point({value})\n"));
    }
    source.push_str("sum = 0\n");
    for value in 0..80 {
        source.push_str(&format!("sum = sum + point_{value}.x\n"));
    }
    source.push_str("sum\n");

    let (interpreted, _, _) = run(&source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(&source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3160)));
    assert!(metrics.pending_instance_allocations >= 64, "{metrics:?}");
    assert!(metrics.pending_instance_materializations > 0, "{metrics:?}");
    assert_eq!(
        metrics.pending_instance_allocations,
        metrics
            .pending_instance_materializations
            .saturating_add(metrics.pending_instance_releases),
        "{metrics:?}"
    );
    assert_eq!(metrics.backend_unavailable_fallbacks, 0, "{metrics:?}");
}

#[test]
fn native_allocation_resumes_native_execution() {
    let (interpreted, _, interpreted_dump) =
        run(ALLOCATION_LOOP, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(ALLOCATION_LOOP, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1000)));
    assert!(metrics.compiled_allocation_sites > 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 1000, "{metrics:?}");
    assert!(metrics.native_inline_allocations >= 999, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn native_allocation_matches_each_fuel_boundary() {
    let source = ALLOCATION_LOOP.replace("1000", "3");
    let artifact = lm_testkit::compile_text("jit-allocation-fuel.lm", &source)
        .expect("the allocation case compiles");
    for fuel in 0..=48 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn native_allocation_preserves_collection_roots() {
    let artifact = lm_testkit::compile_text("jit-allocation-gc.lm", ALLOCATION_LOOP)
        .expect("the allocation case compiles");
    let config = VmConfig {
        heap_bytes: 4096,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1000)));
    assert!(metrics.native_allocations >= 900, "{metrics:?}");
    assert!(metrics.native_collection_slow_paths > 0, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn native_allocation_preserves_the_heap_limit_fault() {
    let source = "class Token\nend\nToken()\n";
    let artifact = lm_testkit::compile_text("jit-allocation-limit.lm", source)
        .expect("the allocation limit case compiles");
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
fn a_fault_after_allocation_does_not_replay_the_allocation() {
    let source = concat!(
        "class Token\n",
        "end\n",
        "def make(divisor: Int): Token\n",
        "  token = Token()\n",
        "  ignored = 1 / divisor\n",
        "  token\n",
        "end\n",
        "make(0)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::DivideByZero));
    assert_eq!(metrics.native_allocations, 1, "{metrics:?}");
}
