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

const ALLOCATION_LOOP: &str = r#"
class Token
end

i = 0
while i < 1000
  token = Token()
  i = i + 1
end
i
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
    run_artifact_with_config(
        artifact,
        mode,
        VmConfig {
            fuel,
            ..VmConfig::default()
        },
    )
}

fn run_artifact_with_config(
    artifact: &lm_bytecode::artifact::Artifact,
    mode: EngineMode,
    config: VmConfig,
) -> (Outcome, lm_vm::EngineMetrics, String) {
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the JIT case publishes");
    let engine = Arc::new(Engine::new(mode));
    let mut vm = Vm::new_with_engine(arena, namespace, config, Arc::clone(&engine));
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    (outcome, engine.metrics(), dump)
}

fn run_with_shared_engine(source: &str, engine: Arc<Engine>) -> String {
    let artifact =
        lm_testkit::compile_text("jit-cache.lm", source).expect("the cache case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the cache case publishes");
    let mut vm = Vm::new_with_engine(arena, namespace, VmConfig::default(), engine);
    let outcome = vm.run();
    vm.show_outcome(&outcome)
}

fn run_effect(
    artifact: &lm_bytecode::artifact::Artifact,
    mode: EngineMode,
    fuel: u64,
    grants: &[&str],
) -> (Outcome, lm_vm::EngineMetrics, String, String) {
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the effect case publishes");
    let engine = Arc::new(Engine::new(mode));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig {
            fuel,
            ..VmConfig::default()
        },
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    world.trace_procs();
    for grant in grants {
        world.allow(grant).expect("the effect grant exists");
    }
    let outcome = world.run_root();
    let dump = world.dump_live(&outcome);
    let trace = world.dump_trace();
    (outcome, engine.metrics(), dump, trace)
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
fn byte_reads_match_the_interpreter() {
    let source = concat!(
        "def sum_bytes(bytes: Bytes): Int\n",
        "  total = 0\n",
        "  pass = 0\n",
        "  while pass < 1000\n",
        "    index = 0\n",
        "    while index < bytes.len()\n",
        "      total = total + bytes.at(index)\n",
        "      index = index + 1\n",
        "    end\n",
        "    pass = pass + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "sum_bytes(Bytes(\"loom\"))\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(439_000)));
    assert!(metrics.native_retired_instructions > 50_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn byte_index_faults_match_the_interpreter() {
    for index in [-1, 4] {
        let source = format!(
            concat!(
                "def read(bytes: Bytes): Int\n",
                "  spin = 0\n",
                "  while spin < 1000\n",
                "    spin = spin + 1\n",
                "  end\n",
                "  bytes.at({})\n",
                "end\n",
                "read(Bytes(\"loom\"))\n",
            ),
            index
        );
        let (interpreted, _, interpreted_dump) = run(&source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(&source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted);
        assert_eq!(native_dump, interpreted_dump);
        assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    }
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
    assert_eq!(engine.metrics().compiled_regions, 4);
}

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
fn one_interpreter_instruction_does_not_reject_the_function() {
    let source = concat!(
        "items: [Int] = []\ni = 0\n",
        "while i < 1000\n",
        "  items.push(i)\n",
        "  i = i + 1\n",
        "end\nitems.len()\n",
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
    assert!(metrics.compiled_interpreter_sites >= 1, "{metrics:?}");
    assert!(metrics.native_interpreter_exits >= 1_000, "{metrics:?}");
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
fn invalid_shift_amounts_replay_one_instruction() {
    for source in ["1 << 64\n", "1 >> -1\n", "1.rotate_left(64)\n"] {
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
    assert!(metrics.native_retired_instructions > 40_000);
    assert_eq!(metrics.unsupported_region_fallbacks, 0, "{metrics:?}");
}

#[test]
fn a_faulting_inline_deopt_can_enter_the_native_callee() {
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
    assert!(metrics.guard_failures > 0);
    assert_eq!(metrics.native_entries, 0);
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
fn noninline_calls_match_each_fuel_boundary() {
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
}

#[test]
fn an_unsupported_caller_enters_a_supported_hot_callee() {
    let source = concat!(
        "def hot(limit: Int): Int\n",
        "  i = 0\ns = 0\n",
        "  while i < limit\n",
        "    s = s + i\n",
        "    i = i + 1\n",
        "  end\ns\n",
        "end\n",
        "text = \"loom\"\n",
        "hot(10000) + text.len()\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 100_000, "{metrics:?}");
    assert!(metrics.unsupported_region_fallbacks > 0, "{metrics:?}");
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.compiled_regions, 1, "{metrics:?}");
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
    assert_eq!(metrics.compiled_regions, 2);
    assert_eq!(metrics.compiled_call_sites, 2);
}

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
fn native_class_initialization_releases_each_call_frame() {
    let source = concat!(
        "class Point\n  x: Int = 0\n  y: Int = 0\n",
        "  def init(mut self, x: Int, y: Int)\n",
        "    self.x = x\n    self.y = y\n  end\nend\n",
        "i = 0\ns = 0\nwhile i < 50000\n",
        "  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\n",
        "end\ns\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted, "{metrics:?}\n{automatic_dump}");
    assert_eq!(automatic_dump, interpreted_dump);
    assert!(metrics.native_allocations > 40_000, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 500_000, "{metrics:?}");
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
    assert!(metrics.native_allocations >= 1000);
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
    let source = "text = \"loom\"\ntext\n";
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.native_entries, 0);
    assert_eq!(metrics.compilation_attempts, 0);
    assert_eq!(metrics.unsupported_region_fallbacks, 0);
}

#[test]
fn auto_mode_does_not_probe_hot_unsupported_code() {
    let source = concat!(
        "text = \"loom\"\n",
        "i = 0\n",
        "while i < 100000\n  i = i + 1\nend\n",
        "i\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (automatic, metrics, automatic_dump) = run(source, EngineMode::Auto, u64::MAX);
    assert_eq!(automatic, interpreted);
    assert_eq!(automatic_dump, interpreted_dump);
    assert_eq!(metrics.native_entries, 0);
    assert_eq!(metrics.compilation_attempts, 0);
    assert_eq!(metrics.unsupported_region_fallbacks, 0);
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

fn restore_with_engine(
    artifact: &lm_bytecode::artifact::Artifact,
    image: &lm_vm::snapshot::Image,
    mode: EngineMode,
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
    let engine = Arc::new(Engine::new(mode));
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

fn restore_with_native(
    artifact: &lm_bytecode::artifact::Artifact,
    image: &lm_vm::snapshot::Image,
) -> (RootEvent, lm_vm::EngineMetrics) {
    restore_with_engine(artifact, image, EngineMode::Native)
}

#[test]
fn an_effect_completion_snapshot_resumes_in_both_engines() {
    let source = concat!(
        "def go(): Int with Clock.Now\n",
        "  observed = sys.clock.now()\n",
        "  i = 0\n  total = 0\n",
        "  while i < 1000\n",
        "    total = total + i\n",
        "    i = i + 1\n",
        "  end\n",
        "  total\n",
        "end\n",
        "go()\n",
    );
    let artifact = lm_testkit::compile_text("jit-effect-snapshot.lm", source)
        .expect("the effect snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the effect snapshot case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
        Arc::clone(&engine),
    );
    world.allow("Clock.Now").expect("the clock grant exists");
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        world.drive_slice(root, 128),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(engine.metrics().native_effect_exits, 1);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the completed effect state captures");
    let (interpreted, _) = restore_with_engine(&artifact, image.world(), EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        interpreted,
        RootEvent::Done(lm_value::Value::Int(499_500))
    ));
    assert!(matches!(
        native,
        RootEvent::Done(lm_value::Value::Int(499_500))
    ));
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn a_native_allocation_snapshot_resumes_in_both_engines() {
    let artifact = lm_testkit::compile_text("jit-allocation-snapshot.lm", ALLOCATION_LOOP)
        .expect("the allocation snapshot case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact.clone())
        .expect("the allocation snapshot case publishes");
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
        world.drive_slice(root, 128),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert!(engine.metrics().native_allocations > 0);
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the allocation state captures");
    let (interpreted, _) = restore_with_engine(&artifact, image.world(), EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, image.world());
    assert!(matches!(
        interpreted,
        RootEvent::Done(lm_value::Value::Int(1000))
    ));
    assert!(matches!(
        native,
        RootEvent::Done(lm_value::Value::Int(1000))
    ));
    assert!(metrics.native_allocations > 0, "{metrics:?}");
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
fn a_wrong_external_field_value_replays_before_native_use() {
    let source = concat!(
        "class Pair\n",
        "  left: Int\n",
        "  def init(mut self, left: Int)\n    self.left = left\n  end\n",
        "end\n",
        "pair = Pair(7)\ni = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + pair.left\n  i = i + 1\n",
        "end\nsum\n",
    );
    let (artifact, mut image) = captured_loop("jit-field-snapshot.lm", source);
    let field = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::Instance { fields, .. } => fields.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds the pair field");
    *field = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_list_value_replays_before_native_use() {
    let source = concat!(
        "items = [1, 2, 3]\ni = 0\nsum = 0\n",
        "while i < 100\n",
        "  sum = sum + items.at(i % 3)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let (artifact, mut image) = captured_loop("jit-list-snapshot.lm", source);
    let item = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::List { items, .. } => items.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds one list item");
    *item = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
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
    let suspended = engine.metrics();
    assert!(suspended.native_retired_instructions > 0);
    assert_eq!(suspended.native_continuation_suspends, 1);
    assert_eq!(suspended.native_continuation_materializations, 0);
    engine.set_mode(EngineMode::Interpreter);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert_eq!(engine.metrics().native_continuation_materializations, 1);
}

#[test]
fn native_quanta_keep_one_authoritative_continuation() {
    let artifact = lm_testkit::compile_text("jit-quanta.lm", SCALAR_LOOP)
        .expect("the continuation case compiles");
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
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    for _ in 0..2 {
        assert!(matches!(
            world.drive_slice(root, 4096),
            Some(lm_vm::SliceExit::Yielded)
        ));
    }
    let metrics = engine.metrics();
    assert_eq!(metrics.native_continuation_suspends, 2);
    assert_eq!(metrics.native_continuation_resumes, 1);
    assert_eq!(metrics.native_continuation_materializations, 0);
    engine.set_mode(EngineMode::Interpreter);
    assert_eq!(
        world.run_root(),
        Outcome::Done(lm_value::Value::Int(49_995_000))
    );
    assert_eq!(engine.metrics().native_continuation_materializations, 1);
}

#[test]
fn a_native_call_stack_survives_a_quantum() {
    let source = concat!(
        "def sum_to(value: Int): Int\n",
        "  if value == 0 then 0 else value + sum_to(value - 1) end\n",
        "end\n",
        "sum_to(100)\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-call-quantum.lm", source).expect("the call case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the call case publishes");
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
        world.drive_slice(root, 64),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert!(matches!(
        world.drive_slice(root, 64),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(world.run_root(), Outcome::Done(lm_value::Value::Int(5050)));
    let metrics = engine.metrics();
    assert!(metrics.native_continuation_resumes > 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 900, "{metrics:?}");
}

#[test]
fn object_results_survive_native_call_quanta() {
    let source = concat!(
        "class Box\n  value: Int\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\nend\n",
        "def leaf(value: Int): Box\n  Box(value)\nend\n",
        "def middle(value: Int): Box\n  leaf(value)\nend\n",
        "def outer(value: Int): Box\n  middle(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 1000\n",
        "  box = outer(i)\n  sum = sum + box.value\n  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-object-quantum.lm", source)
        .expect("the object result case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the object result case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let mut world = World::new_with_engine(
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
    for _ in 0..100_000 {
        match world.drive_slice(root, 7) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the object result run stopped early: {other:?}"),
        }
    }
    assert_eq!(
        world.task_outcome(root),
        Outcome::Done(lm_value::Value::Int(499_500))
    );
}

#[test]
fn result_objects_survive_native_call_quanta() {
    let source = concat!(
        "class Box\n  value: Int\n",
        "  def init(mut self, value: Int)\n    self.value = value\n  end\nend\n",
        "def leaf(value: Int): Result[Box, String]\n  Ok(Box(value))\nend\n",
        "def outer(value: Int): Result[Box, String]\n  leaf(value)\nend\n",
        "i = 0\nsum = 0\nwhile i < 32\n",
        "  case outer(i)\n",
        "  in Ok(box) then sum = sum + box.value\n",
        "  in Err(_) then sum = sum - 10000\n",
        "  end\n  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-result-quantum.lm", source)
        .expect("the result object case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the result object case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    for quantum in 1..=32 {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        let root = lm_vm::TaskKey {
            vm: 0,
            generation: 0,
        };
        for _ in 0..100_000 {
            match world.drive_slice(root, quantum) {
                Some(lm_vm::SliceExit::Yielded) => {}
                Some(lm_vm::SliceExit::Terminal) => break,
                other => panic!("the result object run stopped early: {other:?}"),
            }
        }
        let outcome = world.task_outcome(root);
        assert_eq!(
            outcome,
            Outcome::Done(lm_value::Value::Int(496)),
            "quantum {quantum}: {:?}\n{}",
            engine.metrics(),
            world.dump_live(&outcome)
        );
    }
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
        Arc::clone(&engine),
    );
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    assert!(matches!(
        native.drive_slice(root, 4096),
        Some(lm_vm::SliceExit::Yielded)
    ));
    assert_eq!(engine.metrics().native_continuation_suspends, 1);
    assert_eq!(engine.metrics().native_continuation_materializations, 0);
    let gate = native.next_gate();
    let snapshot = native
        .capture_snapshot(gate, 0, false)
        .expect("native state captures");
    assert_eq!(engine.metrics().native_continuation_materializations, 1);
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
