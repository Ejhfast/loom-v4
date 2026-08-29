use lm_vm::{Engine, EngineMode, Outcome, RecordingHost, RootEvent, Vm, VmConfig, World};
use std::sync::Arc;

const SCALAR_LOOP: &str = r#"
i = 0
sum = 0
float = 0.0
same = false
while i < 10000
  sum = sum + i
  float = float + 1.25
  same = i == i
  i = i + 1
end
if same then sum else 0 end
"#;

fn run(source: &str, mode: EngineMode, fuel: u64) -> (Outcome, lm_vm::EngineMetrics, String) {
    let artifact = lm_testkit::compile_text("jit.lm", source).expect("the JIT case compiles");
    run_artifact(&artifact, mode, fuel)
}

fn run_artifact(
    artifact: &lm_bytecode::artifact::Artifact,
    mode: EngineMode,
    fuel: u64,
) -> (Outcome, lm_vm::EngineMetrics, String) {
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the JIT case publishes");
    let engine = Arc::new(Engine::new(mode));
    let mut vm = Vm::new_with_engine(
        arena,
        namespace,
        VmConfig {
            fuel,
            ..VmConfig::default()
        },
        Arc::clone(&engine),
    );
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    (outcome, engine.metrics(), dump)
}

#[test]
fn scalar_loop_matches_the_interpreter() {
    let (interpreted, _, _) = run(SCALAR_LOOP, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, _) = run(SCALAR_LOOP, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(49_995_000)));
    assert_eq!(metrics.compiled_regions, 1);
    assert!(metrics.native_retired_instructions > 100_000);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn scalar_loop_fuel_matches_the_interpreter() {
    let artifact =
        lm_testkit::compile_text("jit-fuel.lm", SCALAR_LOOP).expect("the fuel case compiles");
    for fuel in 0..=64 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

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
fn float_operations_match_the_interpreter() {
    let source = concat!(
        "value = -(3.5 - 1.25) * 2.0 / 0.5\n",
        "nan = 0.0 / 0.0\n",
        "ok = nan == nan\n",
        "ok = ok and not (nan != nan)\n",
        "ok = ok and value < -8.0\n",
        "ok = ok and value <= -9.0\n",
        "ok = ok and value > -10.0\n",
        "ok = ok and value >= -9.0\n",
        "ok = ok == true\n",
        "if ok then 42 else 0 end\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42)));
    assert!(metrics.native_retired_instructions > 0);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn native_float_results_use_the_canonical_nan() {
    let source = "0.0 / 0.0\n";
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(
        native,
        Outcome::Done(lm_value::Value::Float(lm_value::CANONICAL_NAN_BITS))
    );
    assert!(metrics.native_retired_instructions > 0);
}

#[test]
fn auto_mode_reports_an_unsupported_fallback() {
    let source = "text = \"loom\"\ntext\n";
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.native_entries, 0);
    assert_eq!(metrics.unsupported_region_fallbacks, 1);
}

fn captured_loop(
    path: &str,
    source: &str,
) -> (lm_bytecode::artifact::Artifact, lm_vm::snapshot::Image) {
    let artifact = lm_testkit::compile_text(path, source).expect("the snapshot case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the case publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for _ in 0..40 {
        let gate = world.next_gate();
        let image = world
            .capture_snapshot(gate, 0, false)
            .expect("the scalar state captures");
        let image = image.world();
        if image.machines[0]
            .frames
            .last()
            .is_some_and(|frame| frame.block == 1 && frame.ip == 0)
        {
            return (artifact, image.clone());
        }
        if !matches!(world.step_root(), RootEvent::Ran) {
            break;
        }
    }
    panic!("the scalar loop did not reach its header")
}

fn captured_scalar_loop() -> (lm_bytecode::artifact::Artifact, lm_vm::snapshot::Image) {
    captured_loop(
        "jit-snapshot.lm",
        "i = 0\ns = 0\nwhile i < 100\n  s = s + i\n  i = i + 1\nend\ns\n",
    )
}

fn restore_with_native(
    artifact: &lm_bytecode::artifact::Artifact,
    image: &lm_vm::snapshot::Image,
) -> (RootEvent, lm_vm::EngineMetrics) {
    let bytes =
        lm_vm::snapshot::codec::encode(image, usize::MAX).expect("the scalar image encodes");
    let admitted = lm_testkit::load_snapshot_for_artifact(
        artifact,
        &bytes,
        lm_vm::snapshot::LoadLimits::default(),
    )
    .expect("the scalar image admits");
    let (arena, namespace) =
        lm_testkit::publish_artifact(artifact).expect("the scalar artifact publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    let target = world.new_child(0).expect("the child budget exists");
    let root = world
        .restore_image(0, target, &admitted)
        .expect("the scalar image restores");
    (world.run_machine(root), engine.metrics())
}

#[test]
fn an_external_scalar_snapshot_uses_guarded_native_code() {
    let (artifact, image) = captured_scalar_loop();
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(matches!(event, RootEvent::Done(lm_value::Value::Int(4950))));
    assert!(metrics.guarded_values > 0);
    assert!(metrics.native_retired_instructions > 0);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn an_external_mid_segment_snapshot_reaches_native_code() {
    let artifact = lm_testkit::compile_text("jit-mid-snapshot.lm", SCALAR_LOOP)
        .expect("the snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the snapshot case publishes");
    let mut world = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 1),
        Some(lm_vm::SliceExit::Yielded)
    ));
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the mid-segment state captures");
    let (event, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        event,
        RootEvent::Done(lm_value::Value::Int(49_995_000))
    ));
    assert!(metrics.missing_entry_fallbacks > 0);
    assert!(metrics.native_retired_instructions > 0);
}

#[test]
fn a_wrong_external_scalar_value_never_enters_native_code() {
    let (artifact, mut image) = captured_scalar_loop();
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| matches!(value, lm_value::Value::Int(_)))
        .expect("the loop holds an integer local");
    *local = lm_value::Value::Bool(false);
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(event, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert_eq!(metrics.guard_failures, 1);
    assert_eq!(metrics.native_entries, 0);
}

#[test]
fn a_wrong_dormant_value_does_not_block_native_code() {
    let source = concat!(
        "dormant = 777\n",
        "i = 0\n",
        "while i < 100\n",
        "  i = i + 1\n",
        "end\n",
        "dormant = 2\n",
        "dormant\n",
    );
    let (artifact, mut image) = captured_loop("jit-dormant.lm", source);
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| matches!(value, lm_value::Value::Int(777)))
        .expect("the loop holds the dormant local");
    *local = lm_value::Value::Bool(false);
    let (event, metrics) = restore_with_native(&artifact, &image);
    assert!(matches!(event, RootEvent::Done(lm_value::Value::Int(2))));
    assert!(metrics.native_entries > 0);
    assert_eq!(metrics.guard_failures, 0);
}

#[test]
fn engine_switches_preserve_scalar_state() {
    let artifact =
        lm_testkit::compile_text("jit-switch.lm", SCALAR_LOOP).expect("the switch case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    for _ in 0..20 {
        let gate = world.next_gate();
        let image = world
            .capture_snapshot(gate, 0, false)
            .expect("the switch state captures");
        if image.world().machines[0]
            .frames
            .last()
            .is_some_and(|frame| frame.block == 1 && frame.ip == 0)
        {
            break;
        }
        assert!(matches!(world.step_root(), RootEvent::Ran));
    }
    engine.set_mode(EngineMode::Native);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert!(engine.metrics().native_retired_instructions > 0);

    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes again");
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
    assert!(matches!(
        world.drive_slice(root, 4096),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert!(engine.metrics().native_retired_instructions > 0);
    engine.set_mode(EngineMode::Interpreter);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
}

#[test]
fn native_execution_resumes_after_an_interpreter_mid_segment_stop() {
    let artifact =
        lm_testkit::compile_text("jit-mid-segment.lm", SCALAR_LOOP).expect("the case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
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
        world.drive_slice(root, 1),
        Some(lm_vm::SliceExit::Yielded)
    ));
    engine.set_mode(EngineMode::Native);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    let metrics = engine.metrics();
    assert!(metrics.missing_entry_fallbacks > 0);
    assert!(metrics.native_retired_instructions > 0);
}

#[test]
fn alternating_engines_preserve_each_bounded_turn() {
    let source = "i = 0\ns = 0\nwhile i < 100\n  s = s + i\n  i = i + 1\nend\ns\n";
    let artifact = lm_testkit::compile_text("jit-alternate.lm", source).expect("the case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the case publishes");
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
    for turn in 0..1000 {
        let mode = if turn % 2 == 0 {
            EngineMode::Native
        } else {
            EngineMode::Interpreter
        };
        engine.set_mode(mode);
        match world.drive_slice(root, 13) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the alternating run stopped early: {other:?}"),
        }
    }
    assert_eq!(
        world.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(4950))
    );
    assert!(engine.metrics().native_retired_instructions > 0);
}

#[test]
fn native_capture_resumes_in_the_interpreter() {
    let artifact =
        lm_testkit::compile_text("jit-capture.lm", SCALAR_LOOP).expect("the capture case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut native = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        engine,
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        native.drive_slice(root, 4096),
        Some(lm_vm::SliceExit::Yielded)
    ));
    let gate = native.next_gate();
    let snapshot = native
        .capture_snapshot(gate, 0, false)
        .expect("native state captures");
    let bytes =
        lm_vm::snapshot::codec::encode(snapshot.world(), usize::MAX).expect("native state encodes");
    let admitted = lm_testkit::load_snapshot_for_artifact(
        &artifact,
        &bytes,
        lm_vm::snapshot::LoadLimits::default(),
    )
    .expect("native state admits");
    let (arena, namespace) =
        lm_testkit::publish_artifact(&artifact).expect("the artifact publishes");
    let mut interpreted = World::new(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    let target = interpreted.new_child(0).expect("the child budget exists");
    let restored = interpreted
        .restore_image(0, target, &admitted)
        .expect("native state restores");
    assert!(matches!(
        interpreted.run_machine(restored),
        RootEvent::Done(lm_value::Value::Int(49_995_000))
    ));
}
