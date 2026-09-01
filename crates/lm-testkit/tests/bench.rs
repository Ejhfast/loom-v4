//! The benchmark suite.
//!
//! The benchmark groups run here. Every case is `#[ignore]`, so the
//! ordinary suite never pays for them. Run them with:
//!
//! ```text
//! nix-shell --run "cargo test --release -p lm-testkit --test bench \
//!   -- --ignored --nocapture"
//! ```
//!
//! Set `LOOM_BENCH_FILTER` to one case name for a focused run.
//! Set `LOOM_JIT_PROFILE=1` to print sampled native rejections.
//!
//! Method. Each case compiles and loads once outside the timed
//! region, then times the run alone. The reported cost subtracts an
//! empty-program baseline, so it excludes machine construction. Every
//! JIT cases discard timings when later compilation occurs. They report
//! nine stable rounds. A workload returns a consumed value.
//! JIT rows report the complete timed interval for both engines.
//!
//! The output is one tab-separated row per case, so a reader can join
//! it with the CPython table from `benchmarks/ops.py`.

use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
use lm_source::SourceFile;
use lm_vm::{Engine, EngineMetrics, EngineMode, Vm, VmConfig};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Rounds per case. One warm-up plus this many measured rounds.
const ROUNDS: usize = 9;
/// Measured rounds for message cases with high coordinator cost.
const MESSAGE_ROUNDS: usize = 5;
/// Maximum runs used to reach a stable native-code set.
const MAX_WARM_RUNS: usize = 128;

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

/// A large fuel budget: a benchmark must never stop on fuel.
fn config() -> VmConfig {
    VmConfig {
        fuel: 20_000_000_000,
        ..VmConfig::default()
    }
}

/// Time one program: compile and load once, then run it `ROUNDS + 1`
/// times and take the median run.
fn time_program(source: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
        let start = Instant::now();
        let mut vm = Vm::new(arena, namespace, config());
        let outcome = vm.run();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the benchmark faulted: {}",
            vm.show_outcome(&outcome)
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one program with one shared execution engine.
fn time_program_engine(source: &str, mode: EngineMode) -> (Duration, EngineMetrics) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let engine = Arc::new(Engine::new(mode));
    let mut compiler_metrics = EngineMetrics::default();
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    let mut round = 0;
    while runs.len() < ROUNDS {
        assert!(round < MAX_WARM_RUNS, "native compilation did not settle");
        let start = Instant::now();
        let mut vm = Vm::new_with_engine(arena.clone(), namespace, config(), Arc::clone(&engine));
        let outcome = vm.run();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the benchmark faulted: {}",
            vm.show_outcome(&outcome)
        );
        record_warm_round(
            &engine,
            elapsed,
            round == 0,
            &mut runs,
            &mut compiler_metrics,
        );
        round += 1;
    }
    (
        median(runs),
        with_compiler_metrics(engine.metrics(), compiler_metrics),
    )
}

/// Time first native execution with compilation inside each round.
fn time_program_native_cold(source: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let start = Instant::now();
        let mut vm = Vm::new_with_engine(arena.clone(), namespace, config(), engine);
        let outcome = vm.run();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the cold benchmark faulted: {}",
            vm.show_outcome(&outcome)
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one program after an interpreted setup prefix.
fn time_program_engine_after_setup(
    source: &str,
    mode: EngineMode,
    setup: u32,
) -> (Duration, EngineMetrics) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let engine = Arc::new(Engine::new(EngineMode::Interpreter));
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    let mut compiler_metrics = EngineMetrics::default();
    let mut runs = Vec::with_capacity(ROUNDS);
    let mut round = 0;
    while runs.len() < ROUNDS {
        assert!(round < MAX_WARM_RUNS, "native compilation did not settle");
        engine.set_mode(EngineMode::Interpreter);
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::NullHost),
            Arc::clone(&engine),
        );
        assert!(matches!(
            world.drive_slice(root, setup),
            Some(lm_vm::SliceExit::Yielded)
        ));
        engine.set_mode(mode);
        let start = Instant::now();
        let outcome = world.run_root();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the setup benchmark faulted: {}",
            world.show_outcome(&outcome)
        );
        record_warm_round(
            &engine,
            elapsed,
            round == 0,
            &mut runs,
            &mut compiler_metrics,
        );
        round += 1;
    }
    (
        median(runs),
        with_compiler_metrics(engine.metrics(), compiler_metrics),
    )
}

/// Time cold native execution after an interpreted setup prefix.
fn time_program_native_cold_after_setup(source: &str, setup: u32) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    let mut runs = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let engine = Arc::new(Engine::new(EngineMode::Interpreter));
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::NullHost),
            Arc::clone(&engine),
        );
        assert!(matches!(
            world.drive_slice(root, setup),
            Some(lm_vm::SliceExit::Yielded)
        ));
        engine.set_mode(EngineMode::Native);
        let start = Instant::now();
        let outcome = world.run_root();
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the cold setup benchmark faulted: {}",
            world.show_outcome(&outcome)
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one program through fixed scheduler slices.
fn time_program_engine_sliced(
    source: &str,
    mode: EngineMode,
    quantum: u32,
) -> (Duration, EngineMetrics) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let engine = Arc::new(Engine::new(mode));
    let mut compiler_metrics = EngineMetrics::default();
    let root = lm_vm::TaskKey {
        vm: 0,
        generation: 0,
    };
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    let mut round = 0;
    while runs.len() < ROUNDS {
        assert!(round < MAX_WARM_RUNS, "native compilation did not settle");
        let start = Instant::now();
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::NullHost),
            Arc::clone(&engine),
        );
        loop {
            match world.drive_slice(root, quantum) {
                Some(lm_vm::SliceExit::Yielded) => {}
                Some(lm_vm::SliceExit::Terminal) => break,
                other => panic!("the scalar benchmark stopped early: {other:?}"),
            }
        }
        let elapsed = start.elapsed();
        assert!(matches!(world.task_outcome(root), lm_vm::Outcome::Done(_)));
        record_warm_round(
            &engine,
            elapsed,
            round == 0,
            &mut runs,
            &mut compiler_metrics,
        );
        round += 1;
    }
    (
        median(runs),
        with_compiler_metrics(engine.metrics(), compiler_metrics),
    )
}

/// Time one program through the deterministic scheduler.
fn time_program_engine_scheduled(source: &str, mode: EngineMode) -> (Duration, EngineMetrics, u64) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    time_published_engine_scheduled(arena, namespace, mode)
}

/// Time the first scheduled run with one new execution engine.
fn time_program_engine_scheduled_first(
    source: &str,
    mode: EngineMode,
) -> (Duration, EngineMetrics) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|error| panic!("the benchmark source must compile:\n{error}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let engine = Arc::new(Engine::new(mode));
    let mut world = lm_vm::World::new_with_engine(
        arena,
        namespace,
        config(),
        Box::new(lm_vm::NullHost),
        Arc::clone(&engine),
    );
    let start = Instant::now();
    let outcome = lm_proc::Scheduler::default()
        .run(&mut world)
        .expect("the cold benchmark must run");
    let elapsed = start.elapsed();
    assert!(
        matches!(outcome, lm_vm::Outcome::Done(_)),
        "the cold {mode:?} benchmark faulted: {}",
        world.show_outcome(&outcome),
    );
    (elapsed, engine.metrics())
}

/// Time one published program through the deterministic scheduler.
fn time_published_engine_scheduled(
    arena: lm_link::CodeArena,
    namespace: lm_link::NamespaceId,
    mode: EngineMode,
) -> (Duration, EngineMetrics, u64) {
    let engine = Arc::new(Engine::new(mode));
    let mut compiler_metrics = EngineMetrics::default();
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    let mut retired_instructions = None;
    let mut round = 0;
    while runs.len() < ROUNDS {
        assert!(round < MAX_WARM_RUNS, "native compilation did not settle");
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::NullHost),
            Arc::clone(&engine),
        );
        let mut scheduler = lm_proc::Scheduler::default();
        let start = Instant::now();
        let outcome = scheduler
            .run(&mut world)
            .expect("the scheduled scalar benchmark must run");
        let elapsed = start.elapsed();
        assert!(
            matches!(outcome, lm_vm::Outcome::Done(_)),
            "the scheduled {mode:?} benchmark faulted: {}\n{:?}",
            world.show_outcome(&outcome),
            engine.metrics(),
        );
        let retired = world.metrics().retired_instructions;
        if let Some(expected) = retired_instructions {
            assert_eq!(retired, expected, "{mode:?} round {round}");
        } else {
            retired_instructions = Some(retired);
        }
        record_warm_round(
            &engine,
            elapsed,
            round == 0,
            &mut runs,
            &mut compiler_metrics,
        );
        round += 1;
    }
    (
        median(runs),
        with_compiler_metrics(engine.metrics(), compiler_metrics),
        retired_instructions.unwrap_or(0),
    )
}

fn report_jit_slot_calls(name: &str, source: &str) {
    if !selected(name) {
        return;
    }
    let compiled = compile_module_with_options(
        "jit-slot-bench",
        &SourceFile::new("jit-slot-bench.lm", source),
        &CompileEnv::new().freeze(),
        true,
        &CompileOptions::new()
            .late_function("identity")
            .late_class("Box"),
    )
    .expect("the slot benchmark must compile");
    let artifact =
        lm_testkit::artifact_from_compiled(compiled).expect("the slot benchmark artifact builds");
    let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
        .expect("the slot benchmark artifact publishes");
    let (interpreted, _, retired) =
        time_published_engine_scheduled(arena.clone(), namespace, EngineMode::Interpreter);
    let (automatic, auto_metrics, auto_retired) =
        time_published_engine_scheduled(arena.clone(), namespace, EngineMode::Auto);
    let (native, native_metrics, native_retired) =
        time_published_engine_scheduled(arena, namespace, EngineMode::Native);
    assert_eq!(auto_retired, retired);
    assert_eq!(native_retired, retired);
    assert_eq!(native_metrics.compiled_interpreter_sites, 0);
    let auto_coverage =
        auto_metrics.native_retired_instructions as f64 / (retired * ROUNDS as u64) as f64;
    let native_coverage =
        native_metrics.native_retired_instructions as f64 / (retired * ROUNDS as u64) as f64;
    println!(
        "LOOM_JIT_PROGRAM\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{auto_coverage:.4}\t{native_coverage:.4}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        automatic.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / automatic.as_secs_f64(),
        interpreted.as_secs_f64() / native.as_secs_f64(),
        auto_metrics.compilation_attempts,
        auto_metrics.unproductive_native_demotions,
        auto_metrics.unsupported_region_fallbacks,
        native_metrics.unsupported_region_fallbacks,
        auto_metrics.native_interpreter_exits,
        native_metrics.native_interpreter_exits,
        auto_metrics.native_type_environment_exits,
        native_metrics.native_type_environment_exits,
        native_metrics.native_type_environment_fallbacks,
    );
}

fn record_warm_round(
    engine: &Engine,
    elapsed: Duration,
    first: bool,
    runs: &mut Vec<Duration>,
    compiler: &mut EngineMetrics,
) {
    let metrics = engine.metrics();
    if first || metrics.compilation_attempts != 0 {
        add_compiler_metrics(compiler, metrics);
        runs.clear();
        engine.reset_metrics();
        return;
    }
    runs.push(elapsed);
}

fn add_compiler_metrics(total: &mut EngineMetrics, sample: EngineMetrics) {
    total.compilation_attempts += sample.compilation_attempts;
    total.compiled_regions += sample.compiled_regions;
    total.compiled_code_bytes += sample.compiled_code_bytes;
    total.compiled_segments += sample.compiled_segments;
    total.compiled_call_sites += sample.compiled_call_sites;
    total.compiled_heap_read_sites += sample.compiled_heap_read_sites;
    total.compiled_heap_write_sites += sample.compiled_heap_write_sites;
    total.compiled_allocation_sites += sample.compiled_allocation_sites;
    total.compiled_effect_sites += sample.compiled_effect_sites;
    total.compiled_interpreter_sites += sample.compiled_interpreter_sites;
}

fn with_compiler_metrics(mut runtime: EngineMetrics, compiler: EngineMetrics) -> EngineMetrics {
    assert_eq!(
        runtime.compilation_attempts, 0,
        "a warm benchmark compiled another region"
    );
    runtime.compilation_attempts = compiler.compilation_attempts;
    runtime.compiled_regions = compiler.compiled_regions;
    runtime.compiled_code_bytes = compiler.compiled_code_bytes;
    runtime.compiled_segments = compiler.compiled_segments;
    runtime.compiled_call_sites = compiler.compiled_call_sites;
    runtime.compiled_heap_read_sites = compiler.compiled_heap_read_sites;
    runtime.compiled_heap_write_sites = compiler.compiled_heap_write_sites;
    runtime.compiled_allocation_sites = compiler.compiled_allocation_sites;
    runtime.compiled_effect_sites = compiler.compiled_effect_sites;
    runtime.compiled_interpreter_sites = compiler.compiled_interpreter_sites;
    runtime
}

fn time_effect_program_engine(source: &str, mode: EngineMode) -> (Duration, EngineMetrics) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|error| panic!("the effect benchmark must compile:\n{error}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the effect benchmark must load");
    let engine = Arc::new(Engine::new(mode));
    let mut compiler_metrics = EngineMetrics::default();
    let mut runs = Vec::with_capacity(ROUNDS);
    let mut round = 0;
    while runs.len() < ROUNDS {
        assert!(round < MAX_WARM_RUNS, "native compilation did not settle");
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::RecordingHost::new(1)),
            Arc::clone(&engine),
        );
        world
            .allow("Clock.Now")
            .expect("the clock grant must exist");
        let start = Instant::now();
        let outcome = world.run_root();
        let elapsed = start.elapsed();
        assert!(matches!(outcome, lm_vm::Outcome::Done(_)));
        record_warm_round(
            &engine,
            elapsed,
            round == 0,
            &mut runs,
            &mut compiler_metrics,
        );
        round += 1;
    }
    (
        median(runs),
        with_compiler_metrics(engine.metrics(), compiler_metrics),
    )
}

fn time_effect_program_native_cold(source: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|error| panic!("the effect benchmark must compile:\n{error}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the effect benchmark must load");
    let mut runs = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let mut world = lm_vm::World::new_with_engine(
            arena.clone(),
            namespace,
            config(),
            Box::new(lm_vm::RecordingHost::new(1)),
            engine,
        );
        world
            .allow("Clock.Now")
            .expect("the clock grant must exist");
        let start = Instant::now();
        let outcome = world.run_root();
        let elapsed = start.elapsed();
        assert!(matches!(outcome, lm_vm::Outcome::Done(_)));
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

fn report_jit_effect(name: &str, source: &str, required_exits: u64) {
    if !selected(name) {
        return;
    }
    let (interpreted, _) = time_effect_program_engine(source, EngineMode::Interpreter);
    let cold = time_effect_program_native_cold(source);
    let (native, metrics) = time_effect_program_engine(source, EngineMode::Native);
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    assert!(metrics.compiled_effect_sites > 0);
    assert!(metrics.native_effect_exits >= required_exits);
    println!(
        "LOOM_JIT_EFFECT\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.2}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        cold.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / native.as_secs_f64(),
        metrics.compiled_effect_sites,
        metrics.native_effect_exits,
        metrics.native_entries,
    );
}

fn report_jit(name: &str, source: &str, required_call_sites: u64) {
    if !selected(name) {
        return;
    }
    let (interpreted, _) = time_program_engine(source, EngineMode::Interpreter);
    let cold = time_program_native_cold(source);
    let (native, metrics) = time_program_engine(source, EngineMode::Native);
    assert!(metrics.native_retired_instructions > 0);
    assert!(metrics.compiled_call_sites >= required_call_sites);
    println!(
        "LOOM_JIT\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.2}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        cold.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / native.as_secs_f64(),
        metrics.native_entries,
        metrics.guarded_values,
        metrics.compiled_call_sites,
        metrics.compiled_allocation_sites,
        metrics.native_allocations,
    );
}

fn report_jit_after_setup(name: &str, source: &str, setup: u32) {
    if !selected(name) {
        return;
    }
    let (interpreted, _) = time_program_engine_after_setup(source, EngineMode::Interpreter, setup);
    let cold = time_program_native_cold_after_setup(source, setup);
    let (native, metrics) = time_program_engine_after_setup(source, EngineMode::Native, setup);
    assert!(metrics.native_retired_instructions > 0, "{metrics:?}");
    println!(
        "LOOM_JIT\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.2}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        cold.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / native.as_secs_f64(),
        metrics.native_entries,
        metrics.guarded_values,
        metrics.compiled_call_sites,
        metrics.compiled_allocation_sites,
        metrics.native_allocations,
    );
}

fn report_jit_sliced(name: &str, source: &str, quantum: u32) {
    if !selected(name) {
        return;
    }
    let (interpreted, _) = time_program_engine_sliced(source, EngineMode::Interpreter, quantum);
    let (native, metrics) = time_program_engine_sliced(source, EngineMode::Native, quantum);
    assert!(metrics.native_retired_instructions > 0);
    println!(
        "LOOM_JIT\t{name}\t{:.3}\t-\t{:.3}\t{:.2}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / native.as_secs_f64(),
        metrics.native_entries,
        metrics.guarded_values,
        metrics.compiled_call_sites,
        metrics.compiled_allocation_sites,
        metrics.native_allocations,
    );
}

fn report_jit_scheduled(name: &str, source: &str) {
    if !selected(name) {
        return;
    }
    let (interpreted, _, _) = time_program_engine_scheduled(source, EngineMode::Interpreter);
    let (native, metrics, _) = time_program_engine_scheduled(source, EngineMode::Native);
    assert!(metrics.native_retired_instructions > 0);
    println!(
        "LOOM_JIT\t{name}\t{:.3}\t-\t{:.3}\t{:.2}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / native.as_secs_f64(),
        metrics.native_entries,
        metrics.guarded_values,
        metrics.compiled_call_sites,
        metrics.compiled_allocation_sites,
        metrics.native_allocations,
    );
}

fn report_jit_representative(name: &str, source: &str) {
    if !selected(name) {
        return;
    }
    let (interpreted, _, retired) = time_program_engine_scheduled(source, EngineMode::Interpreter);
    let (automatic, auto_metrics, auto_retired) =
        time_program_engine_scheduled(source, EngineMode::Auto);
    let (native, native_metrics, native_retired) =
        time_program_engine_scheduled(source, EngineMode::Native);
    assert_eq!(auto_retired, retired);
    assert_eq!(native_retired, retired);
    let auto_coverage = if retired == 0 {
        0.0
    } else {
        auto_metrics.native_retired_instructions as f64 / (retired * ROUNDS as u64) as f64
    };
    let native_coverage = if retired == 0 {
        0.0
    } else {
        native_metrics.native_retired_instructions as f64 / (retired * ROUNDS as u64) as f64
    };
    println!(
        "LOOM_JIT_PROGRAM\t{name}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{auto_coverage:.4}\t{native_coverage:.4}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        automatic.as_secs_f64() * 1e3,
        native.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / automatic.as_secs_f64(),
        interpreted.as_secs_f64() / native.as_secs_f64(),
        auto_metrics.compilation_attempts,
        auto_metrics.unproductive_native_demotions,
        auto_metrics.unsupported_region_fallbacks,
        native_metrics.unsupported_region_fallbacks,
        auto_metrics.native_interpreter_exits,
        native_metrics.native_interpreter_exits,
        auto_metrics.native_type_environment_exits,
        native_metrics.native_type_environment_exits,
        native_metrics.native_type_environment_fallbacks,
    );
    if std::env::var_os("LOOM_JIT_PROFILE").is_some() {
        println!("LOOM_JIT_METRICS\t{name}\tauto\t{auto_metrics:?}");
        println!("LOOM_JIT_METRICS\t{name}\tnative\t{native_metrics:?}");
        report_jit_profile(name, source);
    }
}

fn report_jit_cold(name: &str, source: &str) {
    if !selected(name) {
        return;
    }
    let (interpreted, _) = time_program_engine_scheduled_first(source, EngineMode::Interpreter);
    let (automatic, metrics) = time_program_engine_scheduled_first(source, EngineMode::Auto);
    println!(
        "LOOM_JIT_COLD\t{name}\t{:.3}\t{:.3}\t{:.3}\t{}\t{}",
        interpreted.as_secs_f64() * 1e3,
        automatic.as_secs_f64() * 1e3,
        interpreted.as_secs_f64() / automatic.as_secs_f64(),
        metrics.compiled_regions,
        metrics.compiled_code_bytes,
    );
}

fn many_hot_functions_source(functions: usize, rounds: usize) -> String {
    let mut source = String::new();
    for function in 0..functions {
        source.push_str(&format!(
            "def hot_{function}(value: Int): Int\n  if value < -1 then value - 1 else value + 1 end\nend\n"
        ));
    }
    source.push_str("value = 0\nround = 0\n");
    source.push_str(&format!("while round < {rounds}\n"));
    for function in 0..functions {
        source.push_str(&format!("  value = hot_{function}(value)\n"));
    }
    source.push_str("  round = round + 1\nend\nvalue\n");
    source
}

fn report_jit_profile(name: &str, source: &str) {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|error| panic!("the profile source must compile:\n{error}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the profile artifact must load");
    let engine = Arc::new(Engine::new(EngineMode::Auto));
    engine.set_jit_profiling(true);
    let mut world = lm_vm::World::new_with_engine(
        arena.clone(),
        namespace,
        config(),
        Box::new(lm_vm::NullHost),
        Arc::clone(&engine),
    );
    let outcome = lm_proc::Scheduler::default()
        .run(&mut world)
        .expect("the profile program must run");
    assert!(matches!(outcome, lm_vm::Outcome::Done(_)));
    let profile = engine.jit_profile();
    println!(
        "LOOM_JIT_PROFILE\t{name}\ttotal\t{}\tcandidate\t{}",
        profile.estimated_instructions, profile.candidate_instructions
    );
    for rejection in profile.rejections.iter().take(20) {
        println!(
            "LOOM_JIT_REJECTION\t{name}\t{}\t{}",
            rejection.reason, rejection.estimated_instructions
        );
    }
    for gap in profile.treatment_gaps.iter().take(20) {
        println!(
            "LOOM_JIT_TREATMENT_GAP\t{name}\t{}\t{}",
            gap.instruction, gap.estimated_instructions
        );
    }
    for site in profile.runtime_exits.iter().take(20) {
        println!(
            "LOOM_JIT_RUNTIME_EXIT\t{name}\t{}\t{}\t{}\t{}",
            site.function, site.instruction, site.exit, site.count
        );
    }
    let code = arena
        .namespace(namespace)
        .expect("the profile namespace exists");
    let mut boundaries = std::collections::BTreeMap::<String, u64>::new();
    for function in &profile.hot_functions {
        let Some(definition) = code.tables().funcs.get(function.function as usize) else {
            continue;
        };
        let instruction_count = definition.blocks.iter().map(Vec::len).sum::<usize>().max(1);
        let unit = function.estimated_instructions / instruction_count as u64;
        for instruction in definition.blocks.iter().flatten() {
            if lm_jit::instruction_treatment(instruction).class() == lm_jit::TreatmentClass::Exit {
                let name = format!("{instruction:?}");
                boundaries
                    .entry(name)
                    .and_modify(|weight| *weight = weight.saturating_add(unit))
                    .or_insert(unit);
            }
        }
    }
    let mut boundaries: Vec<_> = boundaries.into_iter().collect();
    boundaries.sort_by_key(|(_, weight)| std::cmp::Reverse(*weight));
    for (instruction, weight) in boundaries.into_iter().take(20) {
        println!("LOOM_JIT_BOUNDARY\t{name}\t{instruction}\t{weight}");
    }
    for function in profile.hot_functions.iter().take(20) {
        println!(
            "LOOM_JIT_FUNCTION\t{name}\t{}\t{}\t{}\t{}\t{}",
            function.name,
            function.estimated_instructions,
            function.candidate,
            function.rejections.join(","),
            function.treatment_gaps.join(",")
        );
    }
}

fn guard_source(extra_locals: usize) -> String {
    let mut source = String::new();
    for local in 0..extra_locals {
        source.push_str(&format!("v{local} = {local}\n"));
    }
    source.push_str("i = 0\nwhile i < 1000000\n  i = i + 1\nend\nsum = i\n");
    for local in 0..extra_locals {
        source.push_str(&format!("sum = sum + v{local}\n"));
    }
    source.push_str("sum\n");
    source
}

fn report_guard_upper_bound() {
    let name = "jit_guard_state";
    if !selected(name) {
        return;
    }
    let (small, small_metrics) =
        time_program_engine_sliced(&guard_source(0), EngineMode::Native, 4096);
    let (large, large_metrics) =
        time_program_engine_sliced(&guard_source(32), EngineMode::Native, 4096);
    let extra_guards = large_metrics
        .guarded_values
        .saturating_sub(small_metrics.guarded_values)
        / ROUNDS as u64;
    let extra_time = large.saturating_sub(small);
    let nanoseconds = if extra_guards == 0 {
        0.0
    } else {
        extra_time.as_nanos() as f64 / extra_guards as f64
    };
    println!(
        "LOOM_JIT_GUARD\t{name}\t{:.3}\t{:.3}\t{nanoseconds:.2}\t{extra_guards}",
        small.as_secs_f64() * 1e3,
        large.as_secs_f64() * 1e3,
    );
}

fn report_auto_mixed() {
    let name = "jit_mixed_auto";
    if !selected(name) {
        return;
    }
    let source = concat!(
        "text = \"loom\"\n",
        "i = 0\n",
        "while i < 1000000\n",
        "  i = i + 1\n",
        "end\n",
        "i + text.len()\n",
    );
    let (interpreted, _) = time_program_engine(source, EngineMode::Interpreter);
    let (automatic, metrics) = time_program_engine(source, EngineMode::Auto);
    assert!(metrics.native_retired_instructions > 0);
    assert!(metrics.native_interpreter_exits > 0);
    let ratio = automatic.as_secs_f64() / interpreted.as_secs_f64();
    println!(
        "LOOM_JIT_MIXED\t{name}\t{:.3}\t{:.3}\t{ratio:.3}",
        interpreted.as_secs_f64() * 1e3,
        automatic.as_secs_f64() * 1e3,
    );
}

/// The cost of machine construction and entry, with no workload.
fn baseline() -> Duration {
    time_program("0\n")
}

fn selected(name: &str) -> bool {
    let Ok(filter) = std::env::var("LOOM_BENCH_FILTER") else {
        return true;
    };
    filter.split(',').any(|item| item == name)
}

/// Report one case: the per-operation cost above the baseline.
fn report(name: &str, iterations: u64, source: &str, base: Duration) {
    if !selected(name) {
        return;
    }
    let total = time_program(source);
    let work = total.saturating_sub(base);
    let per = work.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.1}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// Report one case that runs inside a `World`.
///
/// The cases above build a bare `Vm`. Every tool builds a `World`,
/// and a `World` adds shared fuel, shared resources, and the activation loop.
/// A program with no proc runs one machine there, so this case
/// measures the path a plain `lm run` takes.
fn report_world(name: &str, iterations: u64, source: &str, expected: &str) {
    report_world_with(name, iterations, source, &[], expected);
}

fn report_world_with(name: &str, iterations: u64, source: &str, grants: &[&str], expected: &str) {
    if !selected(name) {
        return;
    }
    let total = time_world(source, grants, config(), expected);
    let per = total.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.1}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// Time one proc program. Compile and load stay outside the timed region.
fn time_world(source: &str, grants: &[&str], config: VmConfig, expected: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
        let start = Instant::now();
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        let mut world = lm_vm::World::new(arena, namespace, config, Box::new(host));
        for grant in grants {
            world.allow(grant).expect("the benchmark grant must exist");
        }
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(world.show_outcome(&outcome), expected);
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one proc program with the parallel coordinator.
fn time_parallel_world(source: &str, workers: usize, expected: &str) -> Duration {
    time_parallel_world_with(source, workers, &["Proc"], expected)
}

/// Time one parallel program with explicit grants.
fn time_parallel_world_with(
    source: &str,
    workers: usize,
    grants: &[&str],
    expected: &str,
) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("parallel-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        let mut world = lm_vm::World::new(arena, namespace, config(), Box::new(host));
        for grant in grants {
            world.allow(grant).expect("the benchmark grant must exist");
        }
        let mut scheduler = lm_proc::Scheduler::default();
        let start = Instant::now();
        let outcome = scheduler
            .run_parallel(&mut world, workers)
            .expect("the parallel benchmark must run");
        let elapsed = start.elapsed();
        assert_eq!(world.show_outcome(&outcome), expected);
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Run one parallel sample and return its clock-free counters.
fn sample_parallel_counters(
    source: &str,
    workers: usize,
    expected: &str,
) -> (
    lm_proc::SchedulerStats,
    lm_vm::WorldMetrics,
    lm_vm::MachineExecutionMetrics,
    lm_vm::TypeEnvMetrics,
) {
    let bytes = lm_testkit::compile_to_bytes("parallel-counter-bench.lm", source)
        .unwrap_or_else(|error| panic!("the benchmark source must compile:\n{error}"));
    let (arena, namespace) =
        lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
    let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
    let mut world = lm_vm::World::new(arena, namespace, config(), Box::new(host));
    world.allow("Proc").expect("the Proc grant must exist");
    let mut scheduler = lm_proc::Scheduler::default();
    let outcome = scheduler
        .run_parallel(&mut world, workers)
        .expect("the parallel benchmark must run");
    assert_eq!(world.show_outcome(&outcome), expected);
    (
        scheduler.stats(),
        world.metrics(),
        world.execution_metrics(),
        world.type_metrics(),
    )
}

fn report_parallel_counters(name: &str, source: &str, workers: usize, expected: &str) {
    let (scheduler, world, execution, types) = sample_parallel_counters(source, workers, expected);
    println!(
        "LOOM\tparallel_counters\t{name}\t{workers}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        scheduler.proc_slices,
        scheduler.local_continuations,
        scheduler.local_rotations,
        scheduler.worker_recalls,
        scheduler.global_quiescence,
        scheduler.collection_quiescence,
        world.retired_instructions,
        world.heap_growth_bytes,
        execution.native_calls,
        execution.collections,
        types.close_hits,
        types.close_misses,
        types.derive_hits,
        types.derive_misses,
    );
}

fn sample_message_world(source: &str, expected: &str, workers: Option<usize>) -> Vec<Duration> {
    let bytes = lm_testkit::compile_to_bytes("parallel-message-bench.lm", source)
        .unwrap_or_else(|error| panic!("the message source must compile:\n{error}"));
    let mut runs = Vec::with_capacity(MESSAGE_ROUNDS);
    for round in 0..=MESSAGE_ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the message artifact must load");
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        let mut world = lm_vm::World::new(arena, namespace, config(), Box::new(host));
        world.allow("Proc").expect("the Proc grant must exist");
        let start = Instant::now();
        let outcome = match workers {
            Some(workers) => lm_proc::Scheduler::default()
                .run_parallel(&mut world, workers)
                .expect("the parallel message benchmark must run"),
            None => lm_proc::run_world(&mut world),
        };
        let elapsed = start.elapsed();
        assert_eq!(world.show_outcome(&outcome), expected);
        if round > 0 {
            runs.push(elapsed);
        }
    }
    runs
}

fn p95(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    let index = values
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    values[index]
}

fn report_message_case(
    name: &str,
    messages: u64,
    source: &str,
    expected: &str,
) -> (Duration, Duration) {
    let deterministic = sample_message_world(source, expected, None);
    let parallel = sample_message_world(source, expected, Some(4));
    let deterministic_median = median(deterministic.clone());
    let parallel_median = median(parallel.clone());
    let ratio = deterministic_median.as_secs_f64() / parallel_median.as_secs_f64();
    println!(
        "LOOM\tparallel_message\t{name}\t{messages}\t4\t{:.3}\t{:.3}\t{:.3}\t{:.3}\t{ratio:.3}",
        deterministic_median.as_secs_f64() * 1e3,
        p95(deterministic).as_secs_f64() * 1e3,
        parallel_median.as_secs_f64() * 1e3,
        p95(parallel).as_secs_f64() * 1e3,
    );
    assert!(
        ratio >= 0.90,
        "{name} reached {ratio:.3}x deterministic throughput"
    );
    (deterministic_median, parallel_median)
}

fn parallel_cpu_source(tasks: usize, iterations: usize) -> (String, String) {
    let mut source = format!(
        "class Spinner < Proc\n  def on_spawn(self): Int\n    i = 0\n    \
         while i < {iterations}\n      i = i + 1\n    end\n    i\n  end\nend\n"
    );
    for task in 0..tasks {
        source.push_str(&format!("p{task} = Spinner.spawn()\n"));
    }
    source.push('(');
    for task in 0..tasks {
        if task > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("p{task}.done()"));
    }
    source.push_str(")\n");
    let values = (0..tasks)
        .map(|_| format!("Ok({iterations})"))
        .collect::<Vec<_>>()
        .join(", ");
    (source, format!("Done(({values}))"))
}

fn parallel_allocating_source(tasks: usize, iterations: usize) -> (String, String) {
    let mut source = format!(
        "class TextBatch < Proc\n  def on_spawn(self): Int\n    builder = StringBuilder()\n    \
         i = 0\n    while i < {iterations}\n      builder.append(\"abcd\")\n      \
         i = i + 1\n    end\n    builder.build().len()\n  end\nend\n"
    );
    for task in 0..tasks {
        source.push_str(&format!("p{task} = TextBatch.spawn()\n"));
    }
    source.push('(');
    for task in 0..tasks {
        if task > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("p{task}.done()"));
    }
    source.push_str(")\n");
    let length = iterations.saturating_mul(4);
    let values = (0..tasks)
        .map(|_| format!("Ok({length})"))
        .collect::<Vec<_>>()
        .join(", ");
    (source, format!("Done(({values}))"))
}

fn parallel_churn_source(tasks: usize, iterations: usize) -> (String, String) {
    let mut source = format!(
        "class TextChurn < Proc\n  def on_spawn(self): Int\n    builder = StringBuilder()\n    \
         i = 0\n    while i < {iterations}\n      builder.append(\"#{{i}}:\")\n      \
         i = i + 1\n    end\n    builder.build().len()\n  end\nend\n"
    );
    for task in 0..tasks {
        source.push_str(&format!("p{task} = TextChurn.spawn()\n"));
    }
    source.push('(');
    for task in 0..tasks {
        if task > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("p{task}.done()"));
    }
    source.push_str(")\n");
    let length: usize = (0..iterations)
        .map(|value| value.to_string().len() + 1)
        .sum();
    let values = (0..tasks)
        .map(|_| format!("Ok({length})"))
        .collect::<Vec<_>>()
        .join(", ");
    (source, format!("Done(({values}))"))
}

fn parallel_queens_source(size: usize) -> (String, String) {
    let mut source = format!(
        r#"def count_queens(columns: Int, left: Int, right: Int, mask: Int): Int
  if columns == mask
    return 1
  end
  available = mask & ~(columns | left | right)
  total = 0
  while available != 0
    bit = available & (0 - available)
    available = available ^ bit
    total = total + count_queens(
      columns | bit,
      ((left | bit) << 1) & mask,
      (right | bit) >>> 1,
      mask
    )
  end
  total
end

class QueenBranch < Proc
  first: Int
  mask: Int

  def init(mut self, first: Int, mask: Int)
    self.first = first
    self.mask = mask
  end

  def on_spawn(self): Int
    count_queens(
      self.first,
      (self.first << 1) & self.mask,
      self.first >>> 1,
      self.mask
    )
  end
end

mask = (1 << {size}) - 1
"#
    );
    for branch in 0..size {
        source.push_str(&format!(
            "p{branch} = QueenBranch.spawn(1 << {branch}, mask)\n"
        ));
    }
    source.push_str("total = 0\n");
    for branch in 0..size {
        source.push_str(&format!(
            "total = total + case p{branch}.done()\n\
             in Ok(value) then value\n\
             in Err(_) then 0\n\
             end\n"
        ));
    }
    source.push_str("total\n");
    let solutions = match size {
        12 => 14_200,
        13 => 73_712,
        _ => panic!("the benchmark needs a recorded queen count"),
    };
    (source, format!("Done({solutions})"))
}

fn iterable_queens_source(size: usize, parallel: bool) -> (String, String) {
    let mapping = if parallel { "par_map" } else { "map" };
    let source = format!(
        r#"def count_queens(columns: Int, left: Int, right: Int, mask: Int): Int
  if columns == mask
    return 1
  end
  available = mask & ~(columns | left | right)
  total = 0
  while available != 0
    bit = available & (0 - available)
    available = available ^ bit
    total = total + count_queens(
      columns | bit,
      ((left | bit) << 1) & mask,
      (right | bit) >>> 1,
      mask
    )
  end
  total
end

mask = (1 << {size}) - 1
counts = Range(0, {size}).{mapping}(do |branch: Int|: Int
  bit = 1 << branch
  count_queens(
    bit,
    (bit << 1) & mask,
    bit >>> 1,
    mask
  )
end)
counts.sum(0)
"#
    );
    let solutions = match size {
        6 => 4,
        7 => 40,
        12 => 14_200,
        13 => 73_712,
        _ => panic!("the benchmark needs a recorded queen count"),
    };
    (source, format!("Done({solutions})"))
}

fn multishot_queens_source(size: usize) -> (String, String) {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root().join("examples/14-vm-as-multishot-search/05-n-queens.lm"),
    )
    .expect("the multishot search source reads")
    .replace(
        "(solutions(4), solutions(5), solutions(6), solutions(7), solutions(8))",
        &format!("solutions({size})"),
    );
    let solutions = match size {
        9 => 352,
        10 => 724,
        _ => panic!("the benchmark needs a recorded queen count"),
    };
    (source, format!("Done({solutions})"))
}

fn manual_par_map_queens_source(size: usize) -> (String, String) {
    let source = format!(
        r#"def count_queens(columns: Int, left: Int, right: Int, mask: Int): Int
  if columns == mask
    return 1
  end
  available = mask & ~(columns | left | right)
  total = 0
  while available != 0
    bit = available & (0 - available)
    available = available ^ bit
    total = total + count_queens(
      columns | bit,
      ((left | bit) << 1) & mask,
      (right | bit) >>> 1,
      mask
    )
  end
  total
end

class QueenChunk < Proc
  branches: List[Int]
  mask: Int

  def init(mut self, branches: List[Int], mask: Int)
    self.branches = branches
    self.mask = mask
  end

  def on_spawn(self): List[Int]
    self.branches.map(do |branch: Int|: Int
      bit = 1 << branch
      count_queens(
        bit,
        (bit << 1) & self.mask,
        bit >>> 1,
        self.mask
      )
    end)
  end
end

values = Range(0, {size}).to_list()
chunk_count = values.len().min(16)
chunk_size = (values.len() + chunk_count - 1) / chunk_count
handles = List[Handle[Never, List[Int]]]()
for chunk in values.chunks(chunk_size)
  handles.push(QueenChunk.spawn(chunk, (1 << {size}) - 1))
end
counts = List[Int]()
for handle in handles
  counts.extend(handle.done().value())
end
counts.sum(0)
"#
    );
    let solutions = match size {
        12 => 14_200,
        13 => 73_712,
        _ => panic!("the benchmark needs a recorded queen count"),
    };
    (source, format!("Done({solutions})"))
}

fn parallel_ping_source(pairs: usize, limit: usize) -> (String, String, u64) {
    let mut source = format!(
        r#"enum PongMessage
  Connect(peer: Handle[PingMessage, Int])
  Request(value: Int)
  Stop(value: Int)
end

enum PingMessage
  Begin
  Reply(value: Int)
end

class Pong < Proc[PongMessage]
  def on_spawn(self): Int with Proc
    peer = case self.receive()
    in Msg(Connect(handle)) then handle
    in Msg(_) then panic("the first message was not Connect")
    in Closed then panic("the Pong mailbox closed")
    end
    loop do
      case self.receive()
      in Msg(Request(value))
        peer.send(Reply(value + 1))
      in Msg(Stop(value))
        return value
      in Msg(Connect(_))
        panic("the Pong proc received Connect twice")
      in Closed
        panic("the Pong mailbox closed")
      end
    end
  end
end

class Ping < Proc[PingMessage]
  pong: Handle[PongMessage, Int]

  def init(mut self, pong: Handle[PongMessage, Int])
    self.pong = pong
  end

  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(Begin) then ()
    in Msg(_) then panic("the first message was not Begin")
    in Closed then panic("the Ping mailbox closed")
    end
    self.pong.send(Request(0))
    loop do
      case self.receive()
      in Msg(Reply(value))
        if value >= {limit}
          self.pong.send(Stop(value))
          return value
        end
        self.pong.send(Request(value))
      in Msg(Begin)
        panic("the Ping proc received Begin twice")
      in Closed
        panic("the Ping mailbox closed")
      end
    end
  end
end

"#
    );
    for pair in 0..pairs {
        source.push_str(&format!(
            "pong{pair} = Pong.spawn()\nping{pair} = Ping.spawn(pong{pair})\n\
             pong{pair}.send(Connect(ping{pair}))\nping{pair}.send(Begin)\n"
        ));
    }
    source.push('(');
    let mut expected = Vec::new();
    for pair in 0..pairs {
        if pair > 0 {
            source.push_str(", ");
        }
        source.push_str(&format!("pong{pair}.done(), ping{pair}.done()"));
        expected.push(format!("Ok({limit})"));
        expected.push(format!("Ok({limit})"));
    }
    source.push_str(")\n");
    let messages = (pairs as u64).saturating_mul((limit as u64).saturating_mul(2) + 3);
    (source, format!("Done(({}))", expected.join(", ")), messages)
}

// ---------------------------------------------------------------
// Group 2: the type checker.
// ---------------------------------------------------------------

/// Generate a module with `n` small functions and one class.
fn checker_source(n: usize) -> String {
    let mut out = String::new();
    out.push_str("class Shape\n  size: Int = 1\n");
    for i in 0..n {
        out.push_str(&format!(
            "  def area{i}(self, k: Int): Int\n    self.size * k + {i}\n  end\n"
        ));
    }
    out.push_str("end\n");
    out.push_str("def f0(n: Int): Int\n  n + 1\nend\n");
    for i in 1..n {
        out.push_str(&format!(
            "def f{i}(n: Int): Int\n  f{} (n) + {i}\nend\n",
            i - 1
        ));
    }
    out.push_str(&format!("s = Shape()\nf{}(s.area0(1))\n", n - 1));
    out
}

/// `n` independent classes, each with two fields and two methods.
fn class_source(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&format!(
            "class C{i}\n  a: Int = {i}\n  b: String = \"c{i}\"\n  \
             def sum(self, k: Int): Int\n    self.a + k\n  end\n  \
             def name(self): String\n    self.b\n  end\nend\n"
        ));
    }
    out.push_str("x = C0()\nx.sum(1)\n");
    out
}

/// One inheritance chain `n` deep. Every level overrides nothing, so
/// the checker resolves each method through the chain.
fn inherit_source(n: usize) -> String {
    let mut out =
        String::from("class L0\n  v: Int = 0\n  def get(self): Int\n    self.v\n  end\nend\n");
    for i in 1..n {
        out.push_str(&format!("class L{i} < L{}\nend\n", i - 1));
    }
    out.push_str(&format!("x = L{}()\nx.get()\n", n - 1));
    out
}

/// `n` generic functions, each instantiated at two types.
fn generic_source(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        out.push_str(&format!("def g{i}[T](a: T, b: T): T\n  a\nend\n"));
    }
    let mut body = String::from("s = 0\n");
    for i in 0..n {
        body.push_str(&format!("s = s + g{i}(1, 2)\n"));
        body.push_str(&format!("t{i} = g{i}(\"a\", \"b\")\n"));
    }
    out.push_str(&body);
    out.push_str("s\n");
    out
}

/// A chain of `n` assignments whose types flow through generic calls.
/// Each step must infer its type argument from the step before.
fn inference_source(n: usize) -> String {
    let mut out = String::from("def thru[T](x: T): T\n  x\nend\nv0 = 1\n");
    for i in 1..n {
        out.push_str(&format!("v{i} = thru(v{}) + 1\n", i - 1));
    }
    out.push_str(&format!("v{}\n", n - 1));
    out
}

/// One enum of `n` arms and one `case` that covers every arm.
fn enum_source(n: usize) -> String {
    let mut out = String::from("enum E\n");
    for i in 0..n {
        out.push_str(&format!("  A{i}(v: Int)\n"));
    }
    out.push_str("end\ndef pick(e: E): Int\n  case e\n");
    for i in 0..n {
        out.push_str(&format!("  in A{i}(v) then v + {i}\n"));
    }
    out.push_str("  end\nend\ne: E = A0(1)\npick(e)\n");
    out
}

/// One function whose body is `n` statements, against `n` functions.
fn wide_body_source(n: usize) -> String {
    let mut out = String::from("def big(): Int\n  s = 0\n");
    for i in 0..n {
        out.push_str(&format!("  s = s + {i}\n"));
    }
    out.push_str("  s\nend\nbig()\n");
    out
}

/// Generate `n` conformances and calls through one interface bound.
fn interface_source(n: usize) -> String {
    let mut out = String::from(
        "interface Measured\n  type Item\n  def measure(self): Int\nend\n\
         def read[T: Measured](value: T): Int\n  value.measure()\nend\n",
    );
    for i in 0..n {
        out.push_str(&format!(
            "final class C{i} implements Measured\n  type Item = Int\n  \
             def measure(self): Int\n    {i}\n  end\nend\n"
        ));
    }
    out.push_str("sum = 0\n");
    for i in 0..n {
        out.push_str(&format!("sum = sum + read(C{i}())\n"));
    }
    out.push_str("sum\n");
    out
}

/// One generated shape: a name, a source generator, and the sizes
/// the benchmarks run it at.
type Shape = (&'static str, fn(usize) -> String, Vec<usize>);

/// Every generated shape, for the checker and the verifier.
fn shapes() -> Vec<Shape> {
    vec![
        (
            "methods_and_chain",
            checker_source as fn(usize) -> String,
            vec![16, 64, 256, 1024],
        ),
        ("classes", class_source, vec![16, 64, 256]),
        ("inherit_chain", inherit_source, vec![16, 64, 256]),
        ("generics", generic_source, vec![16, 64, 256]),
        ("inference_chain", inference_source, vec![16, 64, 256]),
        ("enum_case_arms", enum_source, vec![16, 64, 256]),
        ("wide_body", wide_body_source, vec![64, 256, 1024]),
        ("interfaces", interface_source, vec![16, 64, 256]),
    ]
}

#[path = "bench/compiler.rs"]
mod compiler;
#[path = "bench/filesystem.rs"]
mod filesystem;
#[path = "bench/frontend.rs"]
mod frontend;
#[path = "bench/jit_programs.rs"]
mod jit_programs;
#[path = "bench/jit_scalar.rs"]
mod jit_scalar;
#[path = "bench/language.rs"]
mod language;
#[path = "bench/linking.rs"]
mod linking;
#[path = "bench/parallel.rs"]
mod parallel;
#[path = "bench/runtime.rs"]
mod runtime;
