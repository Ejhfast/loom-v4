use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_source::SourceFile;
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
fn syntax_operations_match_the_interpreter() {
    let source = r#"
def inspect_syntax(): Bool
  builder = SyntaxBuilder()
  children = List[SyntaxElement]()
  children.push(builder.integer("40"))
  children.push(builder.whitespace(" "))
  children.push(builder.plus())
  children.push(builder.whitespace(" "))
  children.push(builder.integer("2"))
  node = builder.statement(children)
  root = node.to_tree().root()
  detached = root.detach()
  root.kind() == 7 and
    root.category() == 0 and
    root.range_start() == 0 and
    root.range_end() == 6 and
    root.text() == "40 + 2" and
    root.children().len() == 5 and
    detached.text() == "40 + 2"
end

inspect_syntax()
"#;
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Bool(true)));
    assert!(metrics.native_retired_instructions > 20, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn dynamic_result_packaging_matches_each_fuel_boundary() {
    let compiled = compile_module_with_options(
        "jit-dynamic-result",
        &SourceFile::new("jit-dynamic-result.lm", "(41, \"loom\")\n"),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new().dynamic_result(),
    )
    .expect("the dynamic result case compiles");
    let artifact =
        lm_testkit::artifact_from_compiled(compiled).expect("the dynamic artifact builds");
    for fuel in 0..=8 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
    let (_, metrics, _) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert!(metrics.native_allocations > 0, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

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
        let outcome = lm_proc::Scheduler::default()
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
        let outcome = lm_proc::Scheduler::default()
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
        let outcome = lm_proc::Scheduler::default()
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
fn text_metadata_and_hash_mix_stay_native() {
    let source = concat!(
        "def measure_string(value: String): Int\n",
        "  value.byte_len() * 10 + value.len()\n",
        "end\n",
        "def measure_view(value: Substring): Int\n",
        "  value.byte_len() * 10 + value.len()\n",
        "end\n",
        "text = \"aé猫z\"\n",
        "view = text.slice(1, 2).expect(\"the text slice exists\")\n",
        "i = 0\n",
        "total = 0\n",
        "hash = 0\n",
        "while i < 1000\n",
        "  total = total + measure_string(text) + measure_view(view)\n",
        "  hash = hash_combine(hash, i)\n",
        "  i = i + 1\n",
        "end\n",
        "(total, hash)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 20_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn map_metadata_and_digest_comparison_stay_native() {
    let source = concat!(
        "left = [1, 2, 3]\nleft.freeze()\n",
        "right = [1, 2, 3]\nright.freeze()\n",
        "left_digest = left.digest()\nright_digest = right.digest()\n",
        "table = {1: 10, 2: 20}\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + table.len()\n",
        "  if left_digest == right_digest\n",
        "    sum = sum + 1\n",
        "  end\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(3_000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 0, "{metrics:?}");
}

#[test]
fn graph_operations_match_each_fuel_boundary() {
    let source = concat!(
        "left = [1, 2, 3]\n",
        "right = left.freeze()\n",
        "first = right.digest()\n",
        "second = right.digest()\n",
        "first == second\n",
    );
    let artifact = lm_testkit::compile_text("jit-graph-fuel.lm", source)
        .expect("the graph fuel case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn graph_helpers_preserve_limit_and_digest_faults() {
    let freeze_source = "items = [[1], [2]]\nitems.freeze()\n";
    let artifact = lm_testkit::compile_text("jit-freeze-limit.lm", freeze_source)
        .expect("the freeze limit case compiles");
    let config = VmConfig {
        graph: lm_vm::GraphLimits {
            max_objects: 1,
            ..lm_vm::GraphLimits::default()
        },
        ..VmConfig::default()
    };
    let (interpreted, _, interpreted_dump) =
        run_artifact_with_config(&artifact, EngineMode::Interpreter, config);
    let (native, metrics, native_dump) =
        run_artifact_with_config(&artifact, EngineMode::Native, config);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::BoundaryLimit));

    let digest_source = "items = [1, 2]\nitems.digest()\n";
    let (interpreted, _, interpreted_dump) = run(digest_source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(digest_source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Fault(lm_vm::FaultCode::UnsendableValue));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert_eq!(metrics.native_interpreter_exits, 1, "{metrics:?}");
}

#[test]
fn utf8_byte_guards_match_the_interpreter() {
    let source = concat!(
        "text = \"aé猫z\"\n",
        "valid = text.slice_bytes(1, 5).is_ok()\n",
        "invalid = text.slice_bytes(2, 1).is_err()\n",
        "mapped = text.map() { |value: Char| value }\n",
        "(valid, invalid, mapped)\n",
    );
    let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 2, "{metrics:?}");
}

#[test]
fn utf8_scalar_reads_stay_native() {
    let source = concat!(
        "text = \"aé猫z\"\n",
        "round = 0\ntotal = 0\n",
        "while round < 1000\n",
        "  index = 0\n",
        "  while index < text.len()\n",
        "    total = total + text.at(index).expect(\"the scalar exists\").codepoint()\n",
        "    index = index + 1\n",
        "  end\n",
        "  round = round + 1\n",
        "end\ntotal\n",
    );
    let artifact = lm_testkit::compile_text("jit-utf8-scalar.lm", source)
        .expect("the scalar read case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(29_935_000)));
    assert!(metrics.native_retired_instructions > 50_000, "{metrics:?}");
    assert!(metrics.compiled_heap_read_sites >= 1, "{metrics:?}");
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
}

#[test]
fn guarded_callback_conversion_matches_the_interpreter() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "stored = do |value: Int|: Int value + 1 end\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + invoke(stored)\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-conversion.lm", source)
        .expect("the callback conversion compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function.blocks.iter().flatten().any(|instruction| {
            matches!(
                instruction,
                lm_bytecode::Instr::Extended(lm_bytecode::ExtendedInstr::AsCallback)
            )
        })
    }));
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42_000)));
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
}

#[test]
fn callback_slots_preserve_captures_across_native_calls() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "base = 1\n",
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  sum = sum + invoke() { |value: Int| value + base }\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-slot.lm", source)
        .expect("the callback slot case compiles");
    assert!(artifact.root().module().funcs.iter().any(|function| {
        function.blocks.iter().flatten().any(|instruction| {
            matches!(
                instruction,
                lm_bytecode::Instr::Extended(lm_bytecode::ExtendedInstr::MakeCallback { .. })
            )
        })
    }));
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(42_000)));
    assert!(metrics.compiled_call_sites >= 2, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits <= 1, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 5_000, "{metrics:?}");
}

#[test]
fn closure_and_callback_creation_match_each_fuel_boundary() {
    let source = concat!(
        "base = 1\n",
        "stored = do |value: Int|: Int value + base end\n",
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "invoke() { |value: Int| value + stored(value) }\n",
    );
    let artifact = lm_testkit::compile_text("jit-capture-allocation-fuel.lm", source)
        .expect("the capture-allocation case compiles");
    for fuel in 0..=40 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn callback_creation_preserves_scheduler_quanta() {
    let source = concat!(
        "def invoke(f: (Int) -> Int): Int\n",
        "  f(41)\n",
        "end\n",
        "base = 1\n",
        "i = 0\nsum = 0\n",
        "while i < 2000\n",
        "  sum = sum + invoke() { |value: Int| value + base }\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-callback-allocation-scheduler.lm", source)
        .expect("the scheduler callback case compiles");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the scheduler callback case publishes");
    let run = |engine: Arc<Engine>| {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            engine,
        );
        let outcome = lm_proc::Scheduler::default()
            .run(&mut world)
            .expect("the scheduler callback case runs");
        let retired = world.metrics().retired_instructions;
        let dump = world.dump_live(&outcome);
        (outcome, retired, dump)
    };
    let interpreted = run(Arc::new(Engine::new(EngineMode::Interpreter)));
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let native = run(Arc::clone(&engine));
    assert_eq!(native, interpreted, "{:?}", engine.metrics());
    let metrics = engine.metrics();
    assert!(metrics.native_continuation_resumes > 0, "{metrics:?}");
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
}

#[test]
fn native_closure_creation_preserves_the_heap_limit_fault() {
    let source = "base = 1\ndo |value: Int|: Int value + base end\n";
    let artifact = lm_testkit::compile_text("jit-closure-allocation-limit.lm", source)
        .expect("the closure-allocation case compiles");
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
fn tuple_and_list_literals_stay_native() {
    let source = concat!(
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  pair = (i, i + 1)\n",
        "  items = [pair[0], pair[1]]\n",
        "  sum = sum + items[0] + items[1]\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-value-array-allocation.lm", source)
        .expect("the value-array allocation case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1_000_000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_allocations >= 2000, "{metrics:?}");
}

#[test]
fn tuple_and_list_literals_match_each_fuel_boundary() {
    let source = "pair = (1, 2)\nitems = [pair[0], pair[1]]\nitems[0] + items[1]\n";
    let artifact = lm_testkit::compile_text("jit-value-array-allocation-fuel.lm", source)
        .expect("the value-array fuel case compiles");
    for fuel in 0..=32 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn tuple_allocation_preserves_the_heap_limit_fault() {
    let artifact = lm_testkit::compile_text("jit-tuple-allocation-limit.lm", "(1, 2)\n")
        .expect("the tuple allocation limit case compiles");
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
fn map_literals_stay_native() {
    let source = concat!(
        "i = 0\nsum = 0\n",
        "while i < 1000\n",
        "  table = {\"a\": i, \"a\": i + 1, \"b\": i + 2}\n",
        "  sum = sum + table.len()\n",
        "  i = i + 1\n",
        "end\nsum\n",
    );
    let artifact = lm_testkit::compile_text("jit-map-allocation.lm", source)
        .expect("the map allocation case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}\n{native_dump}");
    assert_eq!(native_dump, interpreted_dump, "{metrics:?}");
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(2000)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_allocations > 900, "{metrics:?}");
}

#[test]
fn map_literals_match_each_fuel_boundary() {
    let source = "table = {\"a\": 1, \"b\": 2}\ntable.len()\n";
    let artifact = lm_testkit::compile_text("jit-map-allocation-fuel.lm", source)
        .expect("the map allocation fuel case compiles");
    for fuel in 0..=24 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, _, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn map_lookup_helpers_stay_native() {
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
fn map_lookup_helpers_match_each_fuel_boundary() {
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
fn map_insertions_use_typed_probe_and_commit_helpers() {
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
fn captured_closure_calls_preserve_scheduler_quanta() {
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
        let outcome = lm_proc::Scheduler::default()
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
    assert!(metrics.native_continuation_resumes > 0, "{metrics:?}");
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

#[test]
fn builders_and_byte_construction_use_dedicated_paths() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "builder.append(\"A\").append_int(-12).append_bool(true)\n",
        "builder.append_float(1.5).push_char('é')\n",
        "built_text = builder.build()\n",
        "text_size = builder.len() + builder.byte_len()\n",
        "builder.clear().append(\"done\")\n",
        "finished_text = builder.finish()\n",
        "buffer = ByteBuffer()\n",
        "buffer.reserve(8).append(1).append(2).extend(Bytes(\"AB\"))\n",
        "byte = buffer.at_or(1, 0)\n",
        "built_bytes = buffer.build()\n",
        "byte_size = buffer.len()\n",
        "buffer.clear().extend(Bytes(\"done\"))\n",
        "finished_bytes = buffer.finish()\n",
        "slice_size = case built_bytes.slice(1, 2)\n",
        "in Ok(value) then value.len()\n",
        "in Err(_) then 0\n",
        "end\n",
        "joined = built_bytes + Bytes(\"!\")\n",
        "compact = joined.compact()\n",
        "view_size = case Bytes(\"loom\").utf8_view()\n",
        "in Ok(value) then value.byte_len()\n",
        "in Err(_) then 0\n",
        "end\n",
        "left = b\"\\x0f\\xf0\"\n",
        "right = b\"\\x33\\x55\"\n",
        "bits = (left & right).len() + (left | right).len()\n",
        "bits = bits + (left ^ right).len() + (~left).len()\n",
        "(built_text.byte_len(), text_size, finished_text.byte_len(), byte, ",
        "byte_size, finished_bytes.len(), slice_size, compact.len(), view_size, bits)\n",
    );
    let artifact = lm_testkit::compile_text("jit-builders.lm", source)
        .expect("the builder construction case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(matches!(native, Outcome::Done(_)));
    assert_eq!(metrics.compiled_interpreter_sites, 0, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn builder_construction_matches_each_fuel_boundary() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "text = builder.append(\"loom\").append_bool(false).build()\n",
        "buffer = ByteBuffer()\n",
        "bytes = buffer.reserve(8).append(1).extend(Bytes(text)).build()\n",
        "(text, bytes)\n",
    );
    let artifact = lm_testkit::compile_text("jit-builder-fuel.lm", source)
        .expect("the builder fuel case compiles");
    for fuel in 0..=48 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn builder_faults_match_the_interpreter() {
    let cases = [
        (
            "buffer = ByteBuffer()\nbuffer.append(256)\n",
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            "buffer = ByteBuffer()\nbuffer.reserve(-1)\n",
            lm_vm::FaultCode::IntegerOverflow,
        ),
        (
            "builder = StringBuilder()\nbuilder.finish()\nbuilder.len()\n",
            lm_vm::FaultCode::InvalidVmState,
        ),
        (
            "buffer = ByteBuffer()\nbuffer.finish()\nbuffer.len()\n",
            lm_vm::FaultCode::InvalidVmState,
        ),
    ];
    for (source, expected) in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{metrics:?}");
        assert_eq!(native_dump, interpreted_dump);
        assert_eq!(native, Outcome::Fault(expected));
    }
}

#[test]
fn builder_growth_preserves_the_heap_limit() {
    let source = concat!(
        "builder = StringBuilder()\n",
        "i = 0\n",
        "while i < 10000\n",
        "  builder.append(\"abcdefgh\")\n",
        "  i = i + 1\n",
        "end\n",
        "builder.len()\n",
    );
    let artifact = lm_testkit::compile_text("jit-builder-limit.lm", source)
        .expect("the builder limit case compiles");
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
}

#[test]
fn text_byte_and_conversion_algorithms_use_dedicated_paths() {
    let source = concat!(
        "base: Text = \"  Ab,é,,C  \"\n",
        "joined = base.concat(\"!\")\n",
        "checks = (base.starts_with(\"  A\"), base.ends_with(\"  \"), ",
        "base.contains(\"é\"), base.find(\"é\"), base.find_bytes(\"é\"))\n",
        "trimmed = (base.trim(), base.trim_start(), base.trim_end())\n",
        "mapped = (base.to_lower_ascii(), base.to_upper_ascii())\n",
        "replaced = base.replace(\",\", \"|\")\n",
        "padded = (\"x\".pad_start(3), \"x\".pad_end(3))\n",
        "pieces = \"a,b,,c\".split(\",\")\n",
        "lines = \"a\\r\\nb\\n\".lines()\n",
        "scalar_slice = case base.slice(2, 2)\n",
        "in Ok(value) then value.to_string()\n",
        "in Err(_) then \"bad\"\n",
        "end\n",
        "byte_slice = case base.slice_bytes(2, 2)\n",
        "in Ok(value) then value.to_string()\n",
        "in Err(_) then \"bad\"\n",
        "end\n",
        "bytes = base.bytes()\n",
        "byte_checks = (bytes.starts_with(Bytes(\"  A\")), bytes.ends_with(Bytes(\"  \")), ",
        "bytes.contains(Bytes(\"é\")), bytes.find(Bytes(\"é\")), bytes.hex(), bytes.utf8())\n",
        "buffer = ByteBuffer().extend(Bytes(\"abcabc\"))\n",
        "buffer_find = buffer.find_from(Bytes(\"bc\"), 2)\n",
        "numbers = (\"7f\".parse_int(16), \"bad\".parse_int(10), ",
        "\"1.25\".parse_float(), \"bad\".parse_float(), 12.5.fixed(2))\n",
        "(joined, checks, trimmed, mapped, replaced, padded, pieces, lines, ",
        "scalar_slice, byte_slice, byte_checks, buffer_find, numbers)\n",
    );
    let artifact = lm_testkit::compile_text("jit-text-algorithms.lm", source)
        .expect("the text algorithm case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, u64::MAX);
    assert_eq!(native, interpreted, "{metrics:?}");
    assert_eq!(native_dump, interpreted_dump);
    assert!(matches!(native, Outcome::Done(_)));
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
}

#[test]
fn text_algorithms_match_each_fuel_boundary() {
    let source = concat!(
        "text = \"  abc,def  \"\n",
        "bytes = text.trim().to_string().bytes()\n",
        "parts = text.split(\",\")\n",
        "(text.replace(\"abc\", \"ABC\"), bytes.hex(), parts, 1.25.fixed(3))\n",
    );
    let artifact =
        lm_testkit::compile_text("jit-text-fuel.lm", source).expect("the text fuel case compiles");
    for fuel in 0..=96 {
        let (interpreted, _, interpreted_dump) =
            run_artifact(&artifact, EngineMode::Interpreter, fuel);
        let (native, metrics, native_dump) = run_artifact(&artifact, EngineMode::Native, fuel);
        assert_eq!(native, interpreted, "fuel {fuel}: {metrics:?}");
        assert_eq!(native_dump, interpreted_dump, "fuel {fuel}");
    }
}

#[test]
fn text_conversion_faults_match_the_interpreter() {
    let cases = [
        ("1.5.fixed(-1)\n", lm_vm::FaultCode::InvalidPrecision),
        ("b\"\\xff\".text()\n", lm_vm::FaultCode::BadCast),
    ];
    for (source, expected) in cases {
        let (interpreted, _, interpreted_dump) = run(source, EngineMode::Interpreter, u64::MAX);
        let (native, metrics, native_dump) = run(source, EngineMode::Native, u64::MAX);
        assert_eq!(native, interpreted, "{metrics:?}");
        assert_eq!(native_dump, interpreted_dump);
        assert_eq!(native, Outcome::Fault(expected));
    }
}

#[test]
fn a_native_callee_grows_shared_root_storage() {
    let source = concat!(
        "def wide(seed: String): Int\n",
        "  a = seed\n  b = seed\n  c = seed\n  d = seed\n",
        "  e = seed\n  f = seed\n  g = seed\n  h = seed\n",
        "  i = seed\n  j = seed\n  k = seed\n  l = seed\n",
        "  builder = StringBuilder()\n",
        "  builder.append(a).append(b).append(c).append(d)\n",
        "  builder.append(e).append(f).append(g).append(h)\n",
        "  builder.append(i).append(j).append(k).append(l)\n",
        "  builder.len()\nend\n",
        "wide(\"x\")\n",
    );
    let artifact = lm_testkit::compile_text("jit-root-growth.lm", source)
        .expect("the root growth case compiles");
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the root growth case publishes");
    let engine = Arc::new(Engine::new(EngineMode::Native));
    let run = || {
        let mut world = World::new_with_engine(
            arena.clone(),
            namespace,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        lm_proc::run_world(&mut world)
    };
    assert_eq!(run(), Outcome::Done(lm_value::Value::Int(12)));
    engine.reset_metrics();
    assert_eq!(run(), Outcome::Done(lm_value::Value::Int(12)));
    let metrics = engine.metrics();
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert_eq!(metrics.backend_unavailable_fallbacks, 0, "{metrics:?}");
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
        let outcome = lm_proc::Scheduler::default()
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
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(499_500)));
    assert!(metrics.native_allocations >= 1000, "{metrics:?}");
    assert!(metrics.compiled_heap_write_sites >= 2, "{metrics:?}");
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
    assert!(metrics.native_allocations >= 900, "{metrics:?}");
    assert!(metrics.native_interpreter_exits > 0, "{metrics:?}");
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
fn a_wrong_external_map_value_replays_before_native_mutation() {
    let source = concat!(
        "table = {\"value\": 1}\ni = 0\ntotal = 0\n",
        "while i < 100\n",
        "  case table.put(\"value\", i)\n",
        "  in Some(previous) then total = total + previous\n",
        "  in None then total = total + 0\n",
        "  end\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (artifact, mut image) = captured_loop("jit-map-snapshot.lm", source);
    let value = image.machines[0]
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.object {
            lm_vm::Object::Map { entries, .. } => entries.first_mut(),
            _ => None,
        })
        .expect("the snapshot holds one map entry");
    value.value = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(metrics.native_entries > 0, "{metrics:?}");
    assert!(metrics.native_interpreter_exits > 0, "{metrics:?}");
}

#[test]
fn a_wrong_external_option_payload_matches_the_interpreter() {
    let source = concat!(
        "def read(value: Option[Int]): Int\n",
        "  case value\n",
        "  in Some(found) then found\n",
        "  in None then 0\n",
        "  end\n",
        "end\n",
        "value: Option[Int] = Some(7)\n",
        "i = 0\ntotal = 0\n",
        "while i < 100\n",
        "  total = total + read(value)\n",
        "  i = i + 1\n",
        "end\ntotal\n",
    );
    let (artifact, mut image) = captured_loop("jit-option-snapshot.lm", source);
    let local = image.machines[0]
        .locals
        .iter_mut()
        .find(|value| **value == lm_value::Value::Int(7))
        .expect("the snapshot holds the Option payload");
    *local = lm_value::Value::Bool(false);
    let (interpreted, _) = restore_with_engine(&artifact, &image, EngineMode::Interpreter);
    let (native, metrics) = restore_with_native(&artifact, &image);
    assert!(
        matches!(interpreted, RootEvent::Fault(record) if record.code == lm_vm::FaultCode::TypeMismatch)
    );
    assert!(
        matches!(native, RootEvent::Fault(ref record) if record.code == lm_vm::FaultCode::TypeMismatch),
        "{native:?}: {metrics:?}"
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
fn native_calls_carry_each_supported_object_reference() {
    let source = concat!(
        "def keep_text(value: String, first: Bool): String\n",
        "  if first then value else \"other\" end\nend\n",
        "def keep_map(value: Map[String, Int], first: Bool): Map[String, Int]\n",
        "  if first then value else {} end\nend\n",
        "def keep_task(escaping value: () -> Int, first: Bool): () -> Int\n",
        "  if first then value else do ||: Int 8 end end\nend\n",
        "text = \"loom\"\n",
        "table: Map[String, Int] = {\"loom\": 1}\n",
        "task = do ||: Int 7 end\n",
        "i = 0\nwhile i < 1000\n",
        "  text = keep_text(text, true)\n",
        "  table = keep_map(table, true)\n",
        "  task = keep_task(task, true)\n",
        "  i = i + 1\n",
        "end\ni\n",
    );
    let artifact = lm_testkit::compile_text("jit-object-calls.lm", source)
        .expect("the object call case compiles");
    let (interpreted, _, interpreted_dump) =
        run_artifact(&artifact, EngineMode::Interpreter, u64::MAX);
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the object call case publishes");
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
    for _ in 0..100_000 {
        match world.drive_slice(root, 17) {
            Some(lm_vm::SliceExit::Yielded) => {}
            Some(lm_vm::SliceExit::Terminal) => break,
            other => panic!("the object call run stopped early: {other:?}"),
        }
    }
    let native = world.task_outcome(root);
    let native_dump = world.dump_live(&native);
    assert_eq!(native, interpreted);
    assert_eq!(native_dump, interpreted_dump);
    assert_eq!(native, Outcome::Done(lm_value::Value::Int(1000)));
    let metrics = engine.metrics();
    assert!(metrics.compiled_call_sites >= 3, "{metrics:?}");
    assert!(metrics.native_retired_instructions > 10_000, "{metrics:?}");
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
