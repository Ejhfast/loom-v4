use super::*;

#[test]
fn captured_closure_calls_stay_native() {
    let source = concat!(
        "base = 7\n",
        "stored = do |value: Int|: Int base + value end\n",
        "i = 0\nsum = 0\n",
        "while i < 10000\n",
        "  sum = sum + stored(i)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-captured-closure.lm", source)
        .expect("the captured closure case compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function
            .blocks
            .iter()
            .flatten()
            .any(|instruction| matches!(instruction, lm_bytecode::Instr::CallValue { .. }))
    }));
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function
            .blocks
            .iter()
            .flatten()
            .any(|instruction| matches!(instruction, lm_bytecode::Instr::LoadCapture(_)))
    }));
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(50_065_000)));
    assert!(metrics.compiled_call_sites >= 1, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
}

#[test]
fn captured_closure_calls_preserve_scheduler_results() {
    let source = concat!(
        "base = 7\n",
        "stored = do |value: Int|: Int base + value end\n",
        "i = 0\nsum = 0\n",
        "while i < 10000\n",
        "  sum = sum + stored(i)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-captured-closure-scheduler.lm", source)
        .expect("the scheduler closure case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the scheduler closure case publishes");
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
            .expect("the scheduler closure case runs");
        let retired = world.metrics().retired_instructions;
        let dump = world.dump_live(&outcome);
        (outcome, retired, dump)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let native = run(Arc::clone(&engine));
    assert_eq!(native, interpreted, "{:?}", engine.metrics());
    let metrics = engine.metrics();
    assert_eq!(metrics.native_continuation_suspends, 0, "{metrics:?}");
    assert!(metrics.materializations > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
}

#[test]
fn closure_call_stack_limits_match_the_interpreter() {
    let source = concat!(
        "base = 7\n",
        "stored = do |value: Int|: Int base + value end\n",
        "stored(35)\n",
    );
    let artifact = lm_testkit::compile_text("jit-closure-stack-limit.lm", source)
        .expect("the closure stack-limit case compiles");
    let config = VmConfig {
        max_frames: 1,
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::StackLimit));
}

#[test]
fn auto_mode_compiles_only_after_interpreted_work() {
    let (interpreted, _, interpreted_dump) = run(SCALAR_LOOP, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(SCALAR_LOOP, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.compilation_attempts, 1, "{metrics:?}");
    assert_eq!(metrics.compiled_regions, 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn native_code_capacity_is_not_an_unsupported_verdict() {
    let artifact =
        lm_testkit::compile_text("jit-budget.lm", SCALAR_LOOP).expect("the budget case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the budget case publishes");
    let engine = Arc::new(Engine::with_native_code_budget(EngineMode::Native, 1));
    let mut vm = Vm::new_with_engine(arena, namespace, VmConfig::default(), Arc::clone(&engine));
    let outcome = vm.run();
    assert_eq!(outcome, Outcome::Done(lm_value::Value::Int(49_995_000)));
    let metrics = engine.metrics();
    assert!(metrics.code_cache_capacity_fallbacks > 0, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn auto_mode_demotes_repeated_quick_native_exits() {
    let source = concat!(
        "def append_one(mut items: [Int]): Int\n",
        "  items.push(1)\n",
        "  items.len()\n",
        "end\n",
        "items: [Int] = []\n",
        "i = 0\n",
        "while i < 50000\n",
        "  append_one(items)\n",
        "  i = i + 1\n",
        "end\n",
        "items.len()\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(automatic, Outcome::Done(lm_value::Value::Int(50_000)));
    assert!(metrics.compiled_regions >= 1, "{metrics:?}");
    assert!(metrics.unproductive_native_demotions >= 1, "{metrics:?}");
    assert!(metrics.native_entries < 64, "{metrics:?}");
}

#[test]
fn native_cache_is_scoped_to_one_arena_layout() {
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let first = concat!("class P\nend\n", "def make(): P\n  P()\nend\n", "make()\n",);
    let shifted = concat!(
        "class Q\nend\n",
        "class P\nend\n",
        "def make(): P\n  P()\nend\n",
        "make()\n",
    );
    assert_eq!(
        run_with_shared_engine(first, Arc::clone(&engine)),
        "Done(P{})"
    );
    assert_eq!(
        run_with_shared_engine(shifted, Arc::clone(&engine)),
        "Done(P{})"
    );
    assert!(engine.metrics().compiled_regions >= 4);
}
