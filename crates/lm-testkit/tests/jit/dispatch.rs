use super::*;

#[test]
fn virtual_calls_use_native_dispatch_rows() {
    let source = concat!(
        "class Base\n",
        "  def step(self, value: Int): Int\n",
        "    value + 1\n",
        "  end\n",
        "end\n",
        "class Child < Base\n",
        "  def step(self, value: Int): Int\n",
        "    value + 2\n",
        "  end\n",
        "end\n",
        "def sum_steps(value: Base): Int\n",
        "  index = 0\n",
        "  total = 0\n",
        "  while index < 10000\n",
        "    total = total + value.step(index)\n",
        "    index = index + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "sum_steps(Child())\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(50_015_000)));
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn virtual_calls_preserve_scheduler_retirement_counts() {
    let source = concat!(
        "class Base\n",
        "  def step(self, value: Int): Int\n    value + 1\n  end\n",
        "end\n",
        "class Child < Base\n",
        "  def step(self, value: Int): Int\n    value + 2\n  end\n",
        "end\n",
        "def sum_steps(value: Base): Int\n",
        "  index = 0\n  total = 0\n",
        "  while index < 10000\n",
        "    total = total + value.step(index)\n",
        "    index = index + 1\n",
        "  end\n  total\n",
        "end\n",
        "sum_steps(Child())\n",
    );
    let artifact = lm_testkit::compile_text("jit-virtual-retired.lm", source)
        .expect("the virtual retirement case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the virtual retirement case publishes");
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
            .expect("the virtual retirement case runs");
        (outcome, world.metrics().retired_instructions)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Auto));
    let cold = run(Arc::clone(&engine));
    let warm = run(Arc::clone(&engine));
    assert_eq!(cold, interpreted);
    assert_eq!(warm, interpreted);
    assert!(
        engine.metrics().native_retired_instructions > 0,
        "{:?}",
        engine.metrics()
    );
}

#[test]
fn the_default_scheduler_skips_unneeded_physical_yields() {
    let pure = concat!(
        "index = 0\ntotal = 0\n",
        "while index < 100000\n",
        "  total = total + index\n  index = index + 1\n",
        "end\ntotal\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-uncontended-pure.lm", pure).expect("the pure case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the pure case publishes");
    let run_pure = |mut scheduler: lm_proc::Scheduler| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig {
                fuel: u64::MAX,
                ..VmConfig::default()
            },
            Box::new(RecordingHost::new(1)),
            Arc::new(Engine::new(EngineMode::Interpreter)),
        );
        let outcome = scheduler.run(&mut world).expect("the pure case runs");
        assert!(matches!(outcome, Outcome::Done(_)));
        scheduler.stats().root_slices
    };
    let fixed = run_pure(lm_proc::Scheduler::new_with_quantum(
        lm_proc::SchedulerMode::Deterministic,
        lm_proc::DEFAULT_QUANTUM,
    ));
    let default = run_pure(lm_proc::Scheduler::default());
    assert!(fixed > default.saturating_mul(50), "{fixed} {default}");
    let parallel = run_pure(lm_proc::Scheduler::from_config(
        lm_proc::SchedulerConfig::parallel(1),
    ));
    assert_eq!(parallel, default);

    let effectful = concat!(
        "def run(): Int with Clock.Now\n",
        "  index = 0\n  while index < 100000\n    index = index + 1\n  end\n",
        "  sys.clock.now()\n  index\nend\nrun()\n",
    );
    let artifact = lm_testkit::compile_text("jit-uncontended-effect.lm", effectful)
        .expect("the effectful case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the effectful case publishes");
    let run_effectful = |mut scheduler: lm_proc::Scheduler| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig {
                fuel: u64::MAX,
                ..VmConfig::default()
            },
            Box::new(RecordingHost::new(1)),
            Arc::new(Engine::new(EngineMode::Interpreter)),
        );
        world.allow("Clock.Now").expect("the clock grant exists");
        let outcome = scheduler.run(&mut world).expect("the effectful case runs");
        assert!(matches!(outcome, Outcome::Done(_)));
        scheduler.stats().root_slices
    };
    let fixed = run_effectful(lm_proc::Scheduler::new_with_quantum(
        lm_proc::SchedulerMode::Deterministic,
        lm_proc::DEFAULT_QUANTUM,
    ));
    let default = run_effectful(lm_proc::Scheduler::default());
    assert!(fixed > default.saturating_mul(50), "{fixed} {default}");
}

#[test]
fn native_scheduler_polls_materialize_only_requested_yields() {
    let artifact = lm_testkit::compile_text("jit-native-poll.lm", SCALAR_LOOP)
        .expect("the poll case compiles");
    let make_world = || {
        let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
            .expect("the poll case publishes");
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let world = World::new_with_engine(
            arena,
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        (world, engine)
    };
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };

    let (mut idle, idle_engine) = make_world();
    let idle_control = lm_vm::ExecutionControl::new();
    assert!(matches!(
        idle.drive_slice_polled(root, u32::MAX, 4_096, &idle_control),
        Some(lm_vm::SliceExit::Terminal)
    ));
    let idle_metrics = idle_engine.metrics();
    assert_eq!(idle_metrics.native_continuation_suspends, 0);
    assert!(idle_metrics.native_retired_instructions > 100_000);

    let (mut requested, requested_engine) = make_world();
    let control = lm_vm::ExecutionControl::new();
    control.request_yield();
    let before = requested.world_fuel();
    assert!(matches!(
        requested.drive_slice_polled(root, u32::MAX, 4_096, &control),
        Some(lm_vm::SliceExit::Yielded)
    ));
    let retired = before - requested.world_fuel();
    assert!((4_096..=4_160).contains(&retired), "{retired}");
    let requested_metrics = requested_engine.metrics();
    assert_eq!(requested_metrics.native_continuation_suspends, 0);
    assert!(requested_metrics.materializations > 0);
    control.clear_yield();
    assert!(matches!(
        requested.drive_slice_polled(root, u32::MAX, 4_096, &control),
        Some(lm_vm::SliceExit::Terminal)
    ));
    assert_eq!(
        requested.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
}

#[test]
fn idle_native_polls_preserve_direct_calls() {
    let source = concat!(
        "def next(value: Int): Int\n  value + 1\nend\n",
        "value = 0\nwhile value < 100000\n  value = next(value)\nend\nvalue\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-native-call-poll.lm", source).expect("the case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let control = lm_vm::ExecutionControl::new();
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice_polled(root, u32::MAX, 4_096, &control),
        Some(lm_vm::SliceExit::Terminal)
    ));
    assert_eq!(
        world.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(100_000))
    );
    assert!(engine.metrics().native_retired_instructions > 500_000);
}

#[test]
fn automatic_native_polls_preserve_multishot_search_state() {
    let source = include_str!("../../../../examples/14-vm-as-multishot-search/05-n-queens.lm")
        .replace(
            "(solutions(4), solutions(5), solutions(6), solutions(7), solutions(8))",
            "solutions(4)",
        );
    let artifact =
        lm_testkit::compile_text("jit-multishot-poll.lm", &source).expect("the search compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the search publishes");
    let run = |mode, demand_polling| {
        let engine = Arc::new(Engine::new(mode));
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        world.allow("Vm").expect("the VM grant exists");
        let outcome = if demand_polling {
            // A short test interval exercises many idle poll rearms.
            lm_proc::Scheduler::from_config(
                lm_proc::SchedulerConfig::deterministic()
                    .with_quanta(256, 256)
                    .expect("the poll interval is valid"),
            )
            .run(&mut world)
        } else {
            fixed_scheduler().run(&mut world)
        }
        .expect("the search runs");
        let dump = world.dump_live(&outcome);
        (
            outcome,
            dump,
            world.metrics().retired_instructions,
            engine.metrics(),
        )
    };
    let baseline = run(EngineMode::Interpreter, false);
    let fixed_native = run(EngineMode::Native, false);
    let interpreted = run(EngineMode::Interpreter, true);
    let automatic = run(EngineMode::Auto, true);
    let native = run(EngineMode::Native, true);
    assert_eq!(interpreted.0, baseline.0, "{:?}", interpreted.3);
    assert_eq!(interpreted.1, baseline.1, "{:?}", interpreted.3);
    assert_eq!(interpreted.2, baseline.2, "{:?}", interpreted.3);
    assert_eq!(fixed_native.0, baseline.0, "{:?}", fixed_native.3);
    assert_eq!(fixed_native.1, baseline.1, "{:?}", fixed_native.3);
    assert_eq!(fixed_native.2, baseline.2, "{:?}", fixed_native.3);
    assert_eq!(native.0, baseline.0, "{:?} {:?}", native.1, native.3);
    assert_eq!(native.1, baseline.1, "{:?}", native.3);
    assert_eq!(native.2, baseline.2, "{:?}", native.3);
    assert_eq!(automatic.0, baseline.0, "{:?}", automatic.3);
    assert_eq!(automatic.1, baseline.1, "{:?}", automatic.3);
    assert_eq!(automatic.2, baseline.2, "{:?}", automatic.3);
    assert!(native.3.native_retired_instructions > 0, "{:?}", native.3);
}

#[test]
fn native_poll_sweep_preserves_nested_vm_boundaries() {
    let source =
        include_str!("../../../../examples/14-vm-as-multishot-search/01-answer-a-choice.lm");
    let artifact =
        lm_testkit::compile_text("jit-nested-poll.lm", source).expect("the driver compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the driver publishes");
    let run = |interval, engine: Arc<Engine>| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            engine,
        );
        world.allow("Vm").expect("the VM grant exists");
        let outcome = if let Some(interval) = interval {
            lm_proc::Scheduler::from_config(
                lm_proc::SchedulerConfig::deterministic()
                    .with_quanta(interval, interval)
                    .expect("the poll interval is valid"),
            )
            .run(&mut world)
        } else {
            fixed_scheduler().run(&mut world)
        }
        .expect("the driver runs");
        let dump = world.dump_live(&outcome);
        (outcome, dump, world.metrics().retired_instructions)
    };
    let baseline = run(None, Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Native));
    for interval in 1..=64 {
        let actual = run(Some(interval), Arc::clone(&engine));
        assert_eq!(actual.0, baseline.0, "poll interval {interval}");
        assert_eq!(actual.1, baseline.1, "poll interval {interval}");
        assert_eq!(actual.2, baseline.2, "poll interval {interval}");
    }
    assert!(engine.metrics().native_retired_instructions > 0);
}

#[test]
fn interface_calls_use_one_polymorphic_native_cache() {
    let source = concat!(
        "interface Valued\n",
        "  def value(self): Int\n    7\n  end\n",
        "end\n",
        "final class DefaultValue implements Valued\nend\n",
        "final class OverrideValue implements Valued\n",
        "  def value(self): Int\n    11\n  end\n",
        "end\n",
        "def read[T: Valued](value: T): Int\n  value.value()\nend\n",
        "left = DefaultValue()\nright = OverrideValue()\n",
        "index = 0\ntotal = 0\n",
        "while index < 1000\n",
        "  total = total + read(left) + read(right)\n",
        "  index = index + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-interface-call.lm", source)
        .expect("the interface call case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(18_000)));
    assert!(metrics.compiled_call_sites >= 3, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn interface_calls_preserve_scheduler_retirement_counts() {
    let source = concat!(
        "interface Valued\n",
        "  def value(self): Int\n    7\n  end\n",
        "end\n",
        "final class Token implements Valued\nend\n",
        "def read[T: Valued](value: T): Int\n  value.value()\nend\n",
        "token = Token()\nindex = 0\ntotal = 0\n",
        "while index < 10000\n",
        "  total = total + read(token)\n",
        "  index = index + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-interface-retired.lm", source)
        .expect("the interface retirement case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the interface retirement case publishes");
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
            .expect("the interface retirement case runs");
        (outcome, world.metrics().retired_instructions)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Auto));
    let cold = run(Arc::clone(&engine));
    let warm = run(Arc::clone(&engine));
    assert_eq!(cold, interpreted);
    assert_eq!(warm, interpreted);
    assert!(
        engine.metrics().native_retired_instructions > 0,
        "{:?}",
        engine.metrics()
    );
}

#[test]
fn generic_virtual_calls_preserve_exact_type_environments() {
    let source = concat!(
        "class Box[T]\n",
        "  value: T\n",
        "  def init(mut self, value: T)\n    self.value = value\n  end\n",
        "  def keep[U](self, other: U): T\n    self.value\n  end\n",
        "end\n",
        "def read[T, U](box: Box[T], other: U): T\n  box.keep(other)\nend\n",
        "left = Box(7)\nright = Box(true)\n",
        "index = 0\ntotal = 0\n",
        "while index < 1000\n",
        "  if read(right, index) then total = total + 1 end\n",
        "  total = total + read(left, true)\n",
        "  index = index + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-generic-virtual-call.lm", source)
        .expect("the generic virtual call case compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function
            .blocks
            .iter()
            .flatten()
            .any(|instruction| matches!(instruction, lm_bytecode::Instr::CallVirtualG { .. }))
    }));
    for fuel in 0..=48 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (native, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(8_000)));
    assert!(metrics.compiled_call_sites >= 4, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 20_000, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn generic_virtual_calls_preserve_scheduler_retirement_counts() {
    let source = concat!(
        "class Box[T]\n",
        "  value: T\n",
        "  def init(mut self, value: T)\n    self.value = value\n  end\n",
        "  def keep[U](self, other: U): T\n    self.value\n  end\n",
        "end\n",
        "box = Box(7)\nindex = 0\ntotal = 0\n",
        "while index < 10000\n",
        "  total = total + box.keep(index)\n",
        "  index = index + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-generic-virtual-retired.lm", source)
        .expect("the generic virtual retirement case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the generic virtual retirement case publishes");
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
            .expect("the generic virtual retirement case runs");
        (outcome, world.metrics().retired_instructions)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Auto));
    let cold = run(Arc::clone(&engine));
    let warm = run(Arc::clone(&engine));
    assert_eq!(cold, interpreted);
    assert_eq!(warm, interpreted);
    assert!(
        engine.metrics().native_retired_instructions > 0,
        "{:?}",
        engine.metrics()
    );
}
