use super::*;

#[test]
fn integer_overflow_matches_the_interpreter() {
    let source = "value = 9223372036854775807\nvalue + 1\n";
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::IntegerOverflow));
    assert_eq!(metrics.native_fault_exits, 1);
}

#[test]
fn integer_division_and_remainder_match_the_interpreter() {
    let source = concat!(
        "left = 0 - 20\n",
        "right = 3\n",
        "quotient = left / right\n",
        "remainder = left % right\n",
        "scaled = quotient * 10\n",
        "scaled + remainder\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(-62)));
    assert!(metrics.native_retired_instructions > 0);
    assert_eq!(metrics.native_fault_exits, 0);
}

#[test]
fn integer_division_faults_match_at_each_fuel_boundary() {
    let cases = [
        (
            "value = 7\nzero = 0\nvalue / zero\n",
            lm_vm::FaultCode::DivideByZero,
        ),
        (
            "value = 7\nzero = 0\nvalue % zero\n",
            lm_vm::FaultCode::DivideByZero,
        ),
        (
            concat!(
                "minimum = 0 - 9223372036854775807\n",
                "minimum = minimum - 1\n",
                "negative_one = 0 - 1\n",
                "minimum / negative_one\n",
            ),
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            concat!(
                "minimum = 0 - 9223372036854775807\n",
                "minimum = minimum - 1\n",
                "negative_one = 0 - 1\n",
                "minimum % negative_one\n",
            ),
            lm_vm::FaultCode::IntegerOverflow,
        ),
    ];
    for (source, expected) in cases {
        let artifact =
            lm_testkit::compile_text("jit-division-fault.lm", source).expect("the case compiles");
        for fuel in 0..=16 {
            let (interpreted, _, interpreted_dump) =
                run_artifact(&artifact, EngineMode::Interpreter, fuel);
            let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
            assert_eq!(native, interpreted, "fuel {fuel}");
            assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
        }
        let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
        assert_eq!(native, Outcome::Fault(expected));
        assert_eq!(metrics.native_fault_exits, 1);
    }
}

#[test]
fn direct_scalar_calls_match_at_each_fuel_boundary() {
    let source = concat!(
        "def add1(value: Int): Int\n  next = value + 1\n  next\nend\n",
        "i = 0\n",
        "while i < 10000\n  i = add1(i)\nend\n",
        "i\n",
    );
    let artifact = lm_testkit::compile_text("jit-call.lm", source).expect("the call case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(
            native, interpreted,
            "fuel {fuel}: {metrics:?}\n{native_dump}"
        );
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(10_000)));
    assert_eq!(metrics.compiled_call_sites, 1);
    assert_eq!(metrics.compiled_inlined_call_sites, 1);
    assert!(metrics.native_retired_instructions > 40_000);
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn auto_inline_leaf_reenters_after_each_quantum() {
    let source = concat!(
        "def step(value: Int): Int\n  next = value + 1\n  next\nend\n",
        "i = 0\n",
        "while i < 200000\n  i = step(i)\nend\n",
        "i\n",
    );
    let artifact = lm_testkit::compile_text("jit-inline-auto.lm", source)
        .expect("the inline leaf case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the inline leaf case publishes");
    let run = |engine: Arc<Engine>| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        let outcome = fixed_scheduler()
            .run(&mut world)
            .expect("the inline leaf case runs");
        (outcome, world.dump_live(&outcome), engine.metrics())
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Auto));
    let warmup = run(Arc::clone(&engine));
    assert_eq!(warmup.2.compiled_inlined_call_sites, 1, "{:?}", warmup.2);
    engine.reset_metrics();
    let automatic = run(engine);
    assert_eq!(automatic.0, interpreted.0, "{:?}", automatic.2);
    assert_eq!(automatic.1, interpreted.1);
    assert_eq!(automatic.0, Outcome::Done(lm_value::Value::Int(200_000)));
    assert_eq!(automatic.2.native_replay_exits, 0, "{:?}", automatic.2);
    assert!(
        automatic.2.missing_entry_fallbacks < 512,
        "{:?}",
        automatic.2
    );
    assert_eq!(
        automatic.2.unproductive_native_demotions, 0,
        "{:?}",
        automatic.2
    );
    assert!(
        automatic.2.native_retired_instructions > 2_900_000,
        "{:?}",
        automatic.2
    );
}

#[test]
fn generic_calls_preserve_each_exact_type_environment() {
    let source = concat!(
        "def identity[T](value: T): T\n  value\nend\n",
        "def outer[T](value: T): T\n  identity(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 1000\n",
        "  number = outer(i)\n",
        "  text = outer[String](\"x\")\n",
        "  sum = sum + number + text.byte_len()\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-generic-call.lm", source)
        .expect("the generic call case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(500_500)));
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 15_000, "{metrics:?}");
    assert!(metrics.native_type_environment_exits <= 4, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn image_slot_calls_keep_native_state_across_scheduler_turns() {
    let source = concat!(
        "final class Box\n  value: Int = 3\nend\n",
        "def identity[T](value: T): T\n  value\nend\n",
        "index = 0\ntotal = 0\n",
        "while index < 20000\n",
        "  box = Box()\n",
        "  total = total + identity(box.value)\n",
        "  index = index + 1\n",
        "end\ntotal\n",
    );
    let compiled = compile_module_with_options(
        "jit-slot-calls",
        &SourceFile::new("jit-slot-calls.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new()
            .late_function("identity")
            .late_class("Box"),
    )
    .expect("the image slot case compiles");
    assert!(compiled
        .module
        .funcs
        .iter()
        .flat_map(|function| &function.blocks)
        .flatten()
        .any(|instruction| matches!(
            instruction,
            lm_bytecode::Instr::Extended(
                lm_bytecode::ExtendedInstr::CallSlot { .. }
                    | lm_bytecode::ExtendedInstr::NewSlot { .. }
            )
        )));
    let artifact =
        lm_testkit::artifact_from_compiled(compiled).expect("the image slot artifact builds");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the image slot artifact publishes");
    let run = |engine: Arc<Engine>| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        let outcome = fixed_scheduler()
            .run(&mut world)
            .expect("the image slot case runs");
        (outcome, world.dump_live(&outcome), engine.metrics())
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let native = run(Arc::new(Engine::new(EngineMode::Native)));
    assert_eq!(native.0, interpreted.0, "{:?}", native.2);
    assert_eq!(native.1, interpreted.1);
    assert_eq!(native.0, Outcome::Done(lm_value::Value::Int(60_000)));
    assert_eq!(native.2.compiled_interpreter_sites, 0, "{:?}", native.2);
    assert!(native.2.compiled_call_sites >= 2, "{:?}", native.2);
    assert!(native.2.native_continuation_resumes > 0, "{:?}", native.2);
}

#[test]
fn fault_value_operations_use_typed_allocation_helpers() {
    let source = concat!(
        "index = 0\nvalid = true\n",
        "while index < 2000\n",
        "  fault = Fault.denied(\"blocked\")\n",
        "  valid = valid and fault.code() == \"PolicyDenied\"\n",
        "  index = index + 1\n",
        "end\nvalid\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 3_900, "{metrics:?}");
}

#[test]
fn fault_value_operations_match_each_fuel_boundary() {
    let artifact = lm_testkit::compile_text(
        "jit-fault-value-fuel.lm",
        "Fault.denied(\"blocked\").code()\n",
    )
    .expect("the fault value case compiles");
    for fuel in 0..=16 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn generic_option_calls_match_the_interpreter() {
    let cases = [
        "missing: Option[Int] = None\nmissing.expect(\"missing item\")\n",
        concat!(
            "def choose[T](value: Option[T], fallback: T): T\n",
            "  value.value_or(fallback)\n",
            "end\n",
            "(choose[Int](None, 9), choose[String](None, \"empty\"), ",
            "choose[Int](Some(4), 9))\n",
        ),
    ];
    for source in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{metrics:?}");
        assert_eq!(native_dump, interpreted_dump);
    }
}

#[test]
fn generic_environment_cache_does_not_enter_shared_code() {
    let source = concat!(
        "def identity[T](value: T): T\n  value\nend\n",
        "def outer[T](value: T): T\n  identity(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 100\n",
        "  number = outer(i)\n",
        "  text = outer[String](\"x\")\n",
        "  sum = sum + number + text.byte_len()\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-generic-shared.lm", source)
        .expect("the shared generic case compiles");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    for _ in 0..8 {
        let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
            .expect("the shared generic case publishes");
        let mut vm =
            Vm::new_with_engine(arena, namespace, VmConfig::default(), Arc::clone(&engine));
        assert_eq!(vm.run(), Outcome::Done(lm_value::Value::Int(5_050)));
    }
    let metrics = engine.metrics();
    assert!(metrics.native_type_environment_exits <= 32, "{metrics:?}");
    assert_eq!(metrics.native_type_environment_fallbacks, 0, "{metrics:?}");
}

#[test]
fn generic_environment_cache_survives_graph_helpers() {
    let source = concat!(
        "def identity[T](value: T): T\n  value\nend\n",
        "i = 0\nwhile i < 1000\n",
        "  value = identity(i)\n",
        "  table = {\"value\": value}\n",
        "  table.freeze()\n",
        "  i = i + 1\n",
        "end\ni\n",
    );
    let (outcome, metrics, _) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(outcome, Outcome::Done(lm_value::Value::Int(1_000)));
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
    assert!(metrics.native_type_environment_exits <= 2, "{metrics:?}");
    assert_eq!(metrics.native_type_environment_fallbacks, 0, "{metrics:?}");
}

#[test]
fn generic_allocation_preserves_each_exact_type_environment() {
    let source = concat!(
        "class Token[T]\nend\n",
        "def make[T](): Token[T]\n  Token[T]()\nend\n",
        "i = 0\nwhile i < 1000\n",
        "  number = make[Int]()\n",
        "  text = make[String]()\n",
        "  i = i + 1\n",
        "end\ni\n",
    );
    let artifact = lm_testkit::compile_text("jit-generic-allocation.lm", source)
        .expect("the generic allocation case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000)));
    assert!(metrics.native_allocations >= 2_000, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert!(metrics.native_type_environment_exits <= 8, "{metrics:?}");
    assert_eq!(metrics.native_type_environment_fallbacks, 0, "{metrics:?}");
}

#[test]
fn optional_list_reads_stay_native() {
    let source = concat!(
        "items = [10, 20, 30]\ni = 0\ntotal = 0\n",
        "while i < 1000\n",
        "  case items.get(i % 5)\n",
        "  in Some(value) then total = total + value\n",
        "  in None then total = total + 1\n",
        "  end\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-list-get.lm", source)
        .expect("the optional list read case compiles");
    for fuel in 0..=48 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(12_400)));
    assert!(metrics.native_retired_instructions > 15_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn list_push_uses_inline_writes_and_typed_growth() {
    let source = concat!(
        "items: [Int] = []\ni = 0\n",
        "while i < 1000\n",
        "  items.push(i)\n",
        "  i = i + 1\n",
        "end\nitems.len()\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-list-push.lm", source).expect("the list push case compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000)));
    assert!(metrics.compiled_heap_write_sites >= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 2, "{metrics:?}");
}

#[test]
fn list_mutations_use_direct_heap_paths() {
    let source = concat!(
        "items: [Int] = []\ni = 0\ntotal = 0\n",
        "while i < 200\n",
        "  items.insert(0, i)\n",
        "  items.insert(items.len(), i + 1)\n",
        "  total = total + items.remove(0)\n",
        "  total = total + items.swap_remove(0)\n",
        "  items.push(i + 2)\n",
        "  case items.pop()\n",
        "  in Some(value) then total = total + value\n",
        "  in None then total = total - 1000\n",
        "  end\n",
        "  items.push(i)\n",
        "  items.truncate(0)\n",
        "  case items.pop()\n",
        "  in Some(_) then total = total - 1000\n",
        "  in None then total = total + 1\n",
        "  end\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-list-mutations.lm", source)
        .expect("the list mutation case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(60_500)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 6, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
}

#[test]
fn list_swap_uses_a_direct_heap_path() {
    let source = concat!(
        "items = [1, 2, 3]\ni = 0\ntotal = 0\n",
        "while i < 500\n",
        "  items.swap(0, 2)\n",
        "  total = total + items.at(1)\n",
        "  i = i + 1\n",
        "end\ntotal + items.at(0) * 10 + items.at(2)\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-list-swap.lm", source).expect("the list swap case compiles");
    for fuel in [0, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89] {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1013)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");

    let modified = concat!(
        "items = [1, 2]\niterator = items.iterator()\n",
        "items.swap(0, 0)\niterator.next()\n",
        "items.swap(0, 1)\niterator.next()\n",
    );
    let (interpreted, _, interpreted_dump) = run(modified, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(modified, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::CollectionModified));
}

#[test]
fn list_push_preserves_heap_limit_and_frozen_faults() {
    let limit_source = concat!(
        "items: [Int] = []\ni = 0\n",
        "while i < 1000\n",
        "  items.push(i)\n",
        "  i = i + 1\n",
        "end\nitems.len()\n",
    );
    let artifact = lm_testkit::compile_text("jit-list-push-limit.lm", limit_source)
        .expect("the list push limit case compiles");
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

    let frozen_source = "items = [1]\nitems.freeze()\nitems.push(2)\n";
    let (interpreted, _, interpreted_dump) = run(frozen_source, EngineMode::Interpreter, u64::MAX);
    let (native, _, native_dump) = run(frozen_source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
}

#[test]
fn a_faulting_native_callee_matches_the_interpreter() {
    let cases = [
        (
            concat!(
                "def add1(value: Int): Int\n  next = value + 1\n  next\nend\n",
                "value = 9223372036854775807\nadd1(value)\n",
            ),
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            concat!(
                "def divide(left: Int, right: Int): Int\n",
                "  result = left / right\n  result\nend\n",
                "left = 7\nright = 0\ndivide(left, right)\n",
            ),
            lm_vm::FaultCode::DivideByZero,
        ),
    ];
    for (source, expected) in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted);
        assert_eq!(native_dump, interpreted_dump);
        assert_eq!(native, Outcome::Fault(expected));
        assert_eq!(metrics.compiled_call_sites, 1);
        assert_eq!(metrics.compiled_inlined_call_sites, 1);
        assert_eq!(metrics.native_fault_exits, 1);
    }
}

#[test]
fn call_guards_preserve_the_frame_limit() {
    let source = concat!(
        "def add1(value: Int): Int\n  next = value + 1\n  next\nend\n",
        "add1(41)\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-call-limit.lm", source).expect("the call case compiles");
    let config = VmConfig {
        max_frames: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::StackLimit));
    assert!(metrics.native_entries > 0);
    assert_eq!(metrics.native_fault_exits, 1);
}

#[test]
fn recursive_calls_stay_on_one_native_turn_stack() {
    let source = concat!(
        "def sum_to(value: Int): Int\n",
        "  if value == 0 then 0 else value + sum_to(value - 1) end\n",
        "end\n",
        "sum_to(100)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(5_050)));
    assert_eq!(metrics.compiled_call_sites, 2);
    assert_eq!(metrics.compiled_inlined_call_sites, 0);
    assert_eq!(metrics.compiled_regions, 2, "{metrics:?}");
    assert!(metrics.native_entries <= 3, "{metrics:?}");
    assert!(metrics.materializations <= 3, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 900, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn deep_recursion_grows_one_native_turn_stack() {
    let source = concat!(
        "def descend(value: Int): Int\n",
        "  if value == 0 then 0 else descend(value - 1) + 1 end\n",
        "end\n",
        "descend(1000)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000)));
    assert!(metrics.native_activation_grows >= 2, "{metrics:?}");
    assert!(metrics.native_entries <= 3, "{metrics:?}");
    assert!(metrics.materializations <= 3, "{metrics:?}");
    assert_eq!(metrics.backend_unavailable_fallbacks, 0, "{metrics:?}");
}

#[test]
fn deep_recursion_rolls_over_before_the_host_stack_limit() {
    let source = concat!(
        "def descend(value: Int): Int\n",
        "  if value == 0 then 0 else descend(value - 1) + 1 end\n",
        "end\n",
        "descend(60000)\n",
    );
    let artifact = lm_testkit::compile_text("jit-deep-recursion.lm", source)
        .expect("the deep recursion case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let native = std::thread::Builder::new()
        .name("jit-small-stack".to_owned())
        .stack_size(1024 * 1024)
        .spawn(move || run_artifact(&artifact, EngineMode::Native, u64::MAX))
        .expect("the small-stack JIT thread starts")
        .join()
        .expect("the small-stack JIT thread returns");
    let (native, metrics, native_dump) = native;
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(60_000)));
    assert!(metrics.native_entries <= 4, "{metrics:?}");
    assert!(metrics.materializations <= 4, "{metrics:?}");
    assert_eq!(metrics.native_fault_exits, 0, "{metrics:?}");
    assert_eq!(metrics.backend_unavailable_fallbacks, 0, "{metrics:?}");
}

#[test]
fn mutual_recursion_stays_on_one_native_turn_stack() {
    let source = concat!(
        "def even(value: Int): Bool\n",
        "  if value == 0 then true else odd(value - 1) end\n",
        "end\n",
        "def odd(value: Int): Bool\n",
        "  if value == 0 then false else even(value - 1) end\n",
        "end\n",
        "if even(101) then 1 else 2 end\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(2)));
    assert!(metrics.native_entries <= 6, "{metrics:?}");
    assert!(metrics.materializations <= 6, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 700, "{metrics:?}");
}

#[test]
fn native_recursion_preserves_the_frame_limit() {
    let source = concat!(
        "def descend(value: Int): Int\n",
        "  if value == 0 then 0 else 1 + descend(value - 1) end\n",
        "end\n",
        "descend(100)\n",
    );
    let artifact = lm_testkit::compile_text("jit-recursion-limit.lm", source)
        .expect("the recursion case compiles");
    let config = VmConfig {
        max_frames: 8,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::StackLimit));
    assert_eq!(metrics.native_fault_exits, 1, "{metrics:?}");
}

#[test]
fn a_deep_native_fault_materializes_each_frame() {
    let source = concat!(
        "def descend(value: Int): Int\n",
        "  if value == 0 then 1 / 0 else 1 + descend(value - 1) end\n",
        "end\n",
        "descend(20)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::DivideByZero));
    assert_eq!(metrics.native_fault_exits, 1, "{metrics:?}");
    assert!(metrics.materializations <= 3, "{metrics:?}");
}

#[test]
fn recursive_calls_match_each_fuel_boundary() {
    let source = concat!(
        "def sum_to(value: Int): Int\n",
        "  if value == 0 then 0 else value + sum_to(value - 1) end\n",
        "end\n",
        "sum_to(8)\n",
    );
    let artifact = lm_testkit::compile_text("jit-recursive-fuel.lm", source)
        .expect("the recursive fuel case compiles");
    let fuels = [0, 1, 2, 3, 4, 5, 6, 7, 8, 12, 16, 24, 32, 48, 64, 80, 96];
    for fuel in fuels {
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
fn inlined_branching_calls_match_each_fuel_boundary() {
    let source = concat!(
        "def choose(value: Int): Int\n",
        "  if value > 0 then value + 1 else 0 end\n",
        "end\n",
        "i = 0\ns = 0\n",
        "while i < 3\n",
        "  s = s + choose(i)\n",
        "  i = i + 1\n",
        "end\ns\n",
    );
    let artifact = lm_testkit::compile_text("jit-call-transition.lm", source)
        .expect("the call transition case compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(5)));
    assert_eq!(metrics.compiled_inlined_call_sites, 1);
}

#[test]
fn inlined_super_calls_match_the_interpreter() {
    let source = concat!(
        "class Base\n",
        "  def hello(self): String\n    \"base\"\n  end\n",
        "end\n",
        "class Child < Base\n",
        "  def hello(self): String\n    \"child+#{super.hello()}\"\n  end\n",
        "end\n",
        "Child().hello()\n",
    );
    let artifact = lm_testkit::compile_text("jit-super-inline.lm", source)
        .expect("the super call case compiles");
    let (interpreted, _, interpreted_dump) = run_artifact(&artifact, EngineMode::Interpreter, 128);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, 128);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
}

#[test]
fn mutating_callees_remain_native_calls() {
    let source = concat!(
        "def append(mut items: List[Int], value: Int): ()\n",
        "  items.push(value)\n",
        "end\n",
        "items: List[Int] = []\nappend(items, 7)\nitems.len()\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1)));
    assert_eq!(metrics.compiled_inlined_call_sites, 0);
}

#[test]
fn a_closure_caller_reaches_a_hot_native_callee() {
    let source = concat!(
        "def hot(limit: Int): Int\n",
        "  i = 0\ns = 0\n",
        "  while i < limit\n",
        "    s = s + i\n",
        "    i = i + 1\n",
        "  end\ns\n",
        "end\n",
        "text = \"loom\"\n",
        "run = do ||: Int hot(10000) + text.len() end\n",
        "run()\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert!(metrics.compiled_regions >= 2, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert!(metrics.compiled_regions >= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn nested_arithmetic_compiles_factorial_and_fibonacci() {
    let source = concat!(
        "def factorial(n: Int): Int\n",
        "  if n <= 1 then 1 else n * factorial(n - 1) end\n",
        "end\n",
        "def fib(n: Int): Int\n",
        "  if n <= 1 then n else fib(n - 1) + fib(n - 2) end\n",
        "end\n",
        "factorial(10) + fib(12)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3_628_944)));
    assert!(metrics.compiled_regions >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn nested_arithmetic_faults_keep_residual_operands() {
    let source = concat!(
        "left = 7\n",
        "maximum = 9223372036854775807\n",
        "left + (maximum + 1)\n",
    );
    let artifact = lm_testkit::compile_text("jit-nested-fault.lm", source)
        .expect("the nested fault case compiles");
    for fuel in 0..=16 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn deferred_integer_overflow_replays_from_the_segment_entry() {
    let cases = [
        concat!(
            "def fail(left: Int, maximum: Int): Int\n",
            "  left + (maximum + 1)\n",
            "end\n",
            "fail(7, 9223372036854775807)\n",
        ),
        concat!(
            "def fail(maximum: Int): Int\n",
            "  (1 + 2) + maximum\n",
            "end\n",
            "fail(9223372036854775807)\n",
        ),
    ];
    for source in cases {
        let artifact = lm_testkit::compile_text("jit-deferred-overflow.lm", source)
            .expect("the overflow case compiles");
        for fuel in 0..=24 {
            let (interpreted, _, interpreted_dump) =
                run_artifact(&artifact, EngineMode::Interpreter, fuel);
            let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
            assert_eq!(native, interpreted, "fuel {fuel}");
            assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
        }
        let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
        assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::IntegerOverflow));
        assert!(metrics.native_replay_exits > 0, "{metrics:?}");
    }
}

#[test]
fn realistic_scalar_expression_stays_native() {
    let source = concat!(
        "i = 0\ns = 0\n",
        "while i < 10000\n",
        "  s = s + i * 2 - 1\n",
        "  i = i + 1\n",
        "end\n",
        "s\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn direct_call_cache_entries_pin_the_callee_version() {
    let first = lm_testkit::compile_text(
        "jit-call-version.lm",
        concat!(
            "def adjust(value: Int): Int\n  next = value + 1\n  next\nend\n",
            "adjust(40)\n",
        ),
    )
    .expect("the first call version compiles");
    let second = lm_testkit::compile_text(
        "jit-call-version.lm",
        concat!(
            "def adjust(value: Int): Int\n  next = value + 2\n  next\nend\n",
            "adjust(40)\n",
        ),
    )
    .expect("the second call version compiles");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let run_version = |artifact: lm_bytecode::artifact::Artifact| {
        let (arena, namespace) =
            lm_testkit::publish_compiled_artifact(artifact).expect("the call version publishes");
        let mut vm =
            Vm::new_with_engine(arena, namespace, VmConfig::default(), Arc::clone(&engine));
        vm.run()
    };
    assert_eq!(run_version(first), Outcome::Done(lm_value::Value::Int(41)));
    assert_eq!(run_version(second), Outcome::Done(lm_value::Value::Int(42)));
    let metrics = engine.metrics();
    assert!(metrics.compiled_regions >= 2);
    assert!(metrics.compiled_call_sites >= 2);
}
