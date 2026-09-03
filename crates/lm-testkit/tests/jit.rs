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

fn fixed_scheduler() -> lm_proc::Scheduler {
    lm_proc::Scheduler::new_with_quantum(
        lm_proc::SchedulerMode::Deterministic,
        lm_proc::DEFAULT_QUANTUM,
    )
}

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
    if std::env::var_os("LOOM_JIT_PROFILE").is_some() {
        engine.set_jit_profiling(true);
    }
    let mut vm = Vm::new_with_engine(arena, namespace, config, Arc::clone(&engine));
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    if std::env::var_os("LOOM_JIT_PROFILE").is_some() {
        eprintln!("{:#?}", engine.jit_profile());
    }
    (outcome, engine.metrics(), dump)
}

fn run_artifact_and_capture(
    artifact: &lm_bytecode::artifact::Artifact,
    mode: EngineMode,
    fuel: u64,
) -> (
    Outcome,
    lm_vm::EngineMetrics,
    lm_vm::snapshot::SnapshotImage,
) {
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact.clone()).expect("the JIT case publishes");
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
    let outcome = world.run_root();
    let gate = world.next_gate();
    let image = world
        .capture_snapshot(gate, 0, false)
        .expect("the stopped JIT state captures");
    (outcome, engine.metrics(), image)
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

#[path = "jit/builders.rs"]
mod builders;
#[path = "jit/calls.rs"]
mod calls;
#[path = "jit/closures.rs"]
mod closures;
#[path = "jit/dispatch.rs"]
mod dispatch;
#[path = "jit/effects.rs"]
mod effects;
#[path = "jit/graph_text.rs"]
mod graph_text;
#[path = "jit/heap.rs"]
mod heap;
#[path = "jit/literals.rs"]
mod literals;
#[path = "jit/maps.rs"]
mod maps;
#[path = "jit/regex.rs"]
mod regex;
#[path = "jit/scalar.rs"]
mod scalar;
#[path = "jit/scalar_replacement.rs"]
mod scalar_replacement;
#[path = "jit/snapshots.rs"]
mod snapshots;
#[path = "jit/values.rs"]
mod values;

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
