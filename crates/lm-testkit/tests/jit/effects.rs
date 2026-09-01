use super::*;

#[test]
fn exact_effects_resume_native_execution() {
    let source = concat!(
        "def go(): Int with Clock.Now\n",
        "  i = 0\n  total = 0\n  last = 0\n",
        "  while i < 100\n",
        "    total = total + i\n",
        "    last = sys.clock.now()\n",
        "    i = i + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "go()\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-effect.lm", source).expect("the effect case compiles");
    let (interpreted, _, interpreted_dump, interpreted_trace) =
        run_effect(&artifact, EngineMode::Interpreter, u64::MAX, &["Clock.Now"]);
    let (native, metrics, native_dump, native_trace) =
        run_effect(&artifact, EngineMode::Native, u64::MAX, &["Clock.Now"]);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native_trace, interpreted_trace);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(4950)));
    assert!(metrics.compiled_effect_sites > 0, "{metrics:?}");
    assert_eq!(metrics.native_effect_exits, 100, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn first_class_effects_resume_native_execution() {
    let source = concat!(
        "use sys.clock.now\n\n",
        "def go(): Int with Clock.Now\n",
        "  operation = now\n",
        "  i = 0\n  total = 0\n  last = 0\n",
        "  while i < 10\n",
        "    last = operation()\n",
        "    total = total + i\n",
        "    i = i + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "go()\n",
    );
    let artifact = lm_testkit::compile_text("jit-effect-value.lm", source)
        .expect("the first-class effect case compiles");
    let (interpreted, _, interpreted_dump, interpreted_trace) =
        run_effect(&artifact, EngineMode::Interpreter, u64::MAX, &["Clock.Now"]);
    let (native, metrics, native_dump, native_trace) =
        run_effect(&artifact, EngineMode::Native, u64::MAX, &["Clock.Now"]);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native_trace, interpreted_trace);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(45)));
    assert_eq!(metrics.native_effect_exits, 10, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn exact_effects_match_each_fuel_boundary() {
    let source = concat!(
        "def go(): Int with Clock.Now\n",
        "  first = sys.clock.now()\n",
        "  second = sys.clock.now()\n",
        "  first + second\n",
        "end\n",
        "go()\n",
    );
    let artifact = lm_testkit::compile_text("jit-effect-fuel.lm", source)
        .expect("the effect fuel case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump, interpreted_trace) =
            run_effect(&artifact, EngineMode::Interpreter, fuel, &["Clock.Now"]);
        let (native, _, native_dump, native_trace) =
            run_effect(&artifact, EngineMode::Native, fuel, &["Clock.Now"]);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
        assert_eq!(native_trace, interpreted_trace, "fuel {fuel}");
    }
}

#[test]
fn a_deep_effect_materializes_the_native_turn_stack() {
    let source = concat!(
        "def descend(value: Int): Int with Clock.Now\n",
        "  if value == 0 then\n",
        "    observed = sys.clock.now()\n",
        "    1\n",
        "  else\n",
        "    1 + descend(value - 1)\n",
        "  end\n",
        "end\n",
        "descend(20)\n",
    );
    let artifact = lm_testkit::compile_text("jit-deep-effect.lm", source)
        .expect("the deep effect case compiles");
    let (interpreted, _, interpreted_dump, interpreted_trace) =
        run_effect(&artifact, EngineMode::Interpreter, u64::MAX, &["Clock.Now"]);
    let (native, metrics, native_dump, native_trace) =
        run_effect(&artifact, EngineMode::Native, u64::MAX, &["Clock.Now"]);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native_trace, interpreted_trace);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(21)));
    assert_eq!(metrics.native_effect_exits, 1, "{metrics:?}");
}

#[test]
fn a_denied_effect_keeps_the_exact_fault_state() {
    let source = "def go(): Int with Clock.Now\n  sys.clock.now()\nend\ngo()\n";
    let artifact = lm_testkit::compile_text("jit-effect-denied.lm", source)
        .expect("the denied effect case compiles");
    let (interpreted, _, interpreted_dump, interpreted_trace) =
        run_effect(&artifact, EngineMode::Interpreter, u64::MAX, &[]);
    let (native, metrics, native_dump, native_trace) =
        run_effect(&artifact, EngineMode::Native, u64::MAX, &[]);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native_trace, interpreted_trace);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::PolicyDenied));
    assert_eq!(metrics.native_effect_exits, 1, "{metrics:?}");
}

#[test]
fn deferred_effect_replies_resume_native_execution() {
    let source = concat!(
        "def go(): Int with Clock.Sleep\n",
        "  i = 0\n",
        "  while i < 3\n",
        "    sys.clock.sleep(1)\n",
        "    i = i + 1\n",
        "  end\n",
        "  i\n",
        "end\n",
        "go()\n",
    );
    let artifact = lm_testkit::compile_text("jit-effect-deferred.lm", source)
        .expect("the deferred effect case compiles");
    let (interpreted, _, interpreted_dump, interpreted_trace) = run_effect(
        &artifact,
        EngineMode::Interpreter,
        u64::MAX,
        &["Clock.Sleep"],
    );
    let (native, metrics, native_dump, native_trace) =
        run_effect(&artifact, EngineMode::Native, u64::MAX, &["Clock.Sleep"]);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native_trace, interpreted_trace);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3)));
    assert_eq!(metrics.native_effect_exits, 3, "{metrics:?}");
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
fn auto_mode_does_not_compile_cold_unsupported_code() {
    let source = "seed = 1\nrun = do ||: Int seed end\nrun()\n";
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.native_entries, 0);
    assert_eq!(metrics.compilation_attempts, 0);
    assert_eq!(metrics.unsupported_region_fallbacks, 0);
}

#[test]
fn auto_mode_compiles_a_hot_captured_closure() {
    let source = concat!(
        "seed = 1\n",
        "run = do ||: Int\n",
        "  i = 0\n",
        "  while i < 100000\n    i = i + 1\n  end\n",
        "  i + seed\n",
        "end\n",
        "run()\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert!(metrics.compilation_attempts > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert_eq!(metrics.unsupported_region_fallbacks, 0);
}
