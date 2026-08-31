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
    assert!(metrics.native_retired_instructions > 0);
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
    assert!(metrics.native_retired_instructions > 0);
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
// Group 0: guarded scalar JIT regions.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_jit_scalar_regions() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_int_loop",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        0,
    );
    report_jit(
        "jit_float_add",
        "i = 0\ns = 0.0\nwhile i < 1000000\n  s = s + 1.25\n  i = i + 1\nend\ns\n",
        0,
    );
    report_jit(
        "jit_int_eq",
        "i = 0\nsame = false\nwhile i < 1000000\n  same = i == i\n  i = i + 1\nend\nsame\n",
        0,
    );
    report_jit(
        "jit_value_eq",
        concat!(
            "enum Pair\n  Value(left: Int, right: (Int, String))\nend\n",
            "left: Pair = Value(1, (2, \"loom\"))\n",
            "right: Pair = Value(1, (2, \"loom\"))\n",
            "i = 0\nsame = false\n",
            "while i < 200000\n",
            "  same = left == right\n",
            "  i = i + 1\n",
            "end\nsame\n",
        ),
        0,
    );
    report_jit(
        "jit_text_bytes_compare",
        concat!(
            "text = \"alpha\"\nlater = \"omega\"\n",
            "bytes = b\"alpha\"\nlater_bytes = b\"omega\"\n",
            "i = 0\nvalid = false\nhash = 0\n",
            "while i < 100000\n",
            "  valid = text < later and bytes < later_bytes\n",
            "  hash = hash_of(text) ^ hash_of(bytes)\n",
            "  i = i + 1\n",
            "end\n(valid, hash)\n",
        ),
        0,
    );
    report_jit(
        "jit_graph_operations",
        concat!(
            "items = list_repeated[Int](7, 32)\nitems.freeze()\n",
            "expected = items.digest()\ni = 0\nsame = false\n",
            "while i < 20000\n",
            "  items.freeze()\n",
            "  same = expected == items.digest()\n",
            "  i = i + 1\n",
            "end\nsame\n",
        ),
        0,
    );
    report_jit(
        "jit_expression_stack",
        concat!(
            "i = 0\ns = 0\n",
            "while i < 1000000\n",
            "  s = s + i * 2 - 1\n",
            "  i = i + 1\n",
            "end\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_factorial",
        concat!(
            "def factorial(n: Int): Int\n",
            "  if n <= 1 then 1 else n * factorial(n - 1) end\n",
            "end\n",
            "i = 0\ns = 0\n",
            "while i < 10000\n",
            "  s = s + factorial(12)\n",
            "  i = i + 1\n",
            "end\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_fibonacci",
        concat!(
            "def fib(n: Int): Int\n",
            "  if n <= 1 then n else fib(n - 1) + fib(n - 2) end\n",
            "end\n",
            "fib(25)\n",
        ),
        0,
    );
    report_jit(
        "jit_deep_recursion",
        concat!(
            "def down(n: Int): Int\n",
            "  if n <= 0 then 0 else down(n - 1) + 1 end\n",
            "end\n",
            "i = 0\ns = 0\n",
            "while i < 1000\n",
            "  s = s + down(1000)\n  i = i + 1\n",
            "end\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_int_div",
        concat!(
            "i = 1\nd = 3\ns = 0\n",
            "while i < 1000000\n",
            "  q = i / d\n  s = s + q\n  d = d + 2\n",
            "  if d > 1009\n    d = 3\n  end\n",
            "  i = i + 1\nend\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_int_rem",
        concat!(
            "i = 1\nd = 3\ns = 0\n",
            "while i < 1000000\n",
            "  r = i % d\n  s = s + r\n  d = d + 2\n",
            "  if d > 1009\n    d = 3\n  end\n",
            "  i = i + 1\nend\ns\n",
        ),
        0,
    );
    report_jit(
        "jit_direct_call",
        concat!(
            "def add1(value: Int): Int\n  next = value + 1\n  next\nend\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
        1,
    );
    report_jit(
        "jit_call_branch",
        concat!(
            "def add1(value: Int): Int\n",
            "  if value < 0 then value - 1 else value + 1 end\n",
            "end\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
        1,
    );
    report_jit_after_setup(
        "jit_field_read",
        concat!(
            "class Pair\n",
            "  left: Int\n",
            "  def init(mut self, left: Int)\n    self.left = left\n  end\n",
            "end\n",
            "pair = Pair(7)\ni = 0\ns = 0\n",
            "while i < 1000000\n",
            "  value = pair.left\n  s = s + value\n  i = i + 1\n",
            "end\ns\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_field_write",
        concat!(
            "class Cell\n  value: Int = 0\nend\n",
            "def step(mut cell: Cell)\n  cell.value = cell.value + 1\nend\n",
            "cell = Cell()\ni = 0\n",
            "while i < 1000000\n  step(cell)\n  i = i + 1\nend\n",
            "cell.value\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_tuple_read",
        concat!(
            "pair = (7, 11)\ni = 0\nsum = 0\n",
            "while i < 1000000\n  sum = sum + pair[0]\n  i = i + 1\nend\n",
            "sum + pair[1]\n",
        ),
        32,
    );
    report_jit_after_setup(
        "jit_list_read",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  sum = sum + items.at(i % 8)\n  i = i + 1\n",
            "end\nsum + items.len()\n",
        ),
        48,
    );
    report_jit_after_setup(
        "jit_list_replace",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\n",
            "while i < 1000000\n",
            "  items.set(i % 8, i)\n  i = i + 1\n",
            "end\nitems.at(7)\n",
        ),
        48,
    );
    report_jit_after_setup(
        "jit_list_get",
        concat!(
            "items = [0, 1, 2, 3, 4, 5, 6, 7]\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  case items.get(i % 10)\n",
            "  in Some(value) then sum = sum + value\n",
            "  in None then sum = sum + 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit_after_setup(
        "jit_map_lookup",
        concat!(
            "table = {\"a\": 3, \"b\": 5}\ni = 0\nsum = 0\n",
            "while i < 1000000\n",
            "  if table.has(\"a\")\n",
            "    sum = sum + table.at(\"a\")\n",
            "  end\n",
            "  i = i + 1\n",
            "end\nsum\n",
        ),
        64,
    );
    report_jit(
        "jit_map_insert",
        concat!(
            "table: {Int: Int} = {}\ni = 0\n",
            "while i < 50000\n",
            "  table.put(i, i)\n",
            "  i = i + 1\n",
            "end\ntable.len()\n",
        ),
        0,
    );
    report_jit(
        "jit_map_mutations",
        concat!(
            "final class Key implements Hashable\n  value: Int\n",
            "  def init(mut self, value: Int)\n    self.value = value\n  end\n",
            "  def __eq__(self, other: Key): Bool\n    self.value == other.value\n  end\n",
            "  def __hash__(self): Int\n    self.value % 2\n  end\nend\n",
            "first = Key(1).freeze()\nsame = Key(1).freeze()\n",
            "collision = Key(3).freeze()\nraw = Map[Key, Int]()\n",
            "raw.put(first, 1)\nraw.put(collision, 3)\n",
            "direct = {\"a\": 1, \"b\": 2}\ni = 0\ntotal = 0\n",
            "while i < 100000\n",
            "  raw.put(same, i)\n  total = total + raw.at(same)\n",
            "  raw.remove(collision)\n  raw.put(collision, i + 1)\n",
            "  direct.put(\"a\", i)\n  total = total + direct.at(\"a\")\n",
            "  direct.remove(\"b\")\n  direct.put(\"b\", i + 1)\n",
            "  i = i + 1\nend\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_list_push",
        concat!(
            "items: [Int] = []\ni = 0\n",
            "while i < 100000\n",
            "  items.push(i)\n",
            "  i = i + 1\n",
            "end\nitems.len()\n",
        ),
        0,
    );
    report_jit(
        "jit_list_mutations",
        concat!(
            "items: [Int] = []\ni = 0\ntotal = 0\n",
            "while i < 100000\n",
            "  items.insert(0, i)\n",
            "  items.insert(items.len(), i + 1)\n",
            "  total = total + items.remove(0)\n",
            "  total = total + items.swap_remove(0)\n",
            "  items.push(i)\n",
            "  items.truncate(0)\n",
            "  case items.pop()\n",
            "  in Some(_) then total = total - 1000\n",
            "  in None then total = total + 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_list_reserve",
        concat!(
            "items = [1]\nitems.reserve(64)\ni = 0\n",
            "while i < 1000000\n",
            "  items.reserve(0)\n",
            "  i = i + 1\n",
            "end\nitems.capacity()\n",
        ),
        0,
    );
    report_jit(
        "jit_allocation",
        concat!(
            "class Token\nend\n",
            "i = 0\nwhile i < 100000\n",
            "  token = Token()\n  i = i + 1\n",
            "end\ni\n",
        ),
        0,
    );
    report_jit(
        "jit_generic_allocation",
        concat!(
            "class Token[T]\nend\n",
            "def make[T](): Token[T]\n  Token[T]()\nend\n",
            "i = 0\nwhile i < 100000\n",
            "  token = make[Int]()\n  i = i + 1\n",
            "end\ni\n",
        ),
        0,
    );
    println!(
        "LOOM_JIT_EFFECT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\teffect_sites\teffect_exits\tentries"
    );
    report_jit_effect(
        "jit_effect_mixed",
        concat!(
            "def go(): Int with Clock.Now\n",
            "  outer = 0\n  total = 0\n  observed = 0\n",
            "  while outer < 100\n",
            "    inner = 0\n",
            "    while inner < 10000\n",
            "      total = total + 1\n",
            "      inner = inner + 1\n",
            "    end\n",
            "    observed = sys.clock.now()\n",
            "    outer = outer + 1\n",
            "  end\n",
            "  total\n",
            "end\n",
            "go()\n",
        ),
        900,
    );
    report_jit_effect(
        "jit_effect_boundary",
        concat!(
            "def go(): Int with Clock.Now\n",
            "  i = 0\n  observed = 0\n",
            "  while i < 20000\n",
            "    observed = sys.clock.now()\n",
            "    i = i + 1\n",
            "  end\n",
            "  i\n",
            "end\n",
            "go()\n",
        ),
        180_000,
    );
    report_jit_sliced(
        "jit_int_loop_sliced",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        4096,
    );
    report_jit_scheduled(
        "jit_int_loop_scheduled",
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
    );
    report_guard_upper_bound();
    report_auto_mixed();
}

#[test]
#[ignore]
fn bench_jit_builder_construction() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_string_builder",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.append(\"x\")\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_string_builder_int",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.append_int(i)\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_string_builder_char",
        concat!(
            "builder = StringBuilder()\ni = 0\n",
            "while i < 200000\n",
            "  builder.push_char('é')\n  i = i + 1\n",
            "end\nbuilder.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_byte_buffer",
        concat!(
            "buffer = ByteBuffer()\ni = 0\n",
            "while i < 200000\n",
            "  buffer.append(i % 256)\n  i = i + 1\n",
            "end\nbuffer.build().len()\n",
        ),
        0,
    );
    report_jit(
        "jit_byte_construction",
        concat!(
            "left = b\"\\x0f\\xf0\"\nright = b\"\\x33\\x55\"\n",
            "i = 0\ntotal = 0\n",
            "while i < 20000\n",
            "  joined = left + right\n",
            "  total = total + (left & right).len() + (left | right).len()\n",
            "  total = total + (left ^ right).len() + (~joined).len()\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
}

#[test]
#[ignore]
fn bench_jit_text_and_conversion_operations() {
    println!(
        "LOOM_JIT\tcase\tinterpreter_ms\tnative_cold_ms\tnative_warm_ms\tspeedup\tentries\tguards\tcalls\talloc_sites\tallocations"
    );
    report_jit(
        "jit_text_search",
        concat!(
            "text: Text = \"alpha,beta,gamma\"\ni = 0\ntotal = 0\n",
            "while i < 200000\n",
            "  if text.starts_with(\"alpha\") then total = total + 1 end\n",
            "  if text.ends_with(\"gamma\") then total = total + 1 end\n",
            "  if text.contains(\"beta\") then\n",
            "    case text.find(\"beta\")\n",
            "    in Some(index) then total = total + index\n",
            "    in None then total = total - 1\n",
            "    end\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_text_transform",
        concat!(
            "text: Text = \"  Alpha,beta  \"\ni = 0\ntotal = 0\n",
            "while i < 20000\n",
            "  mapped = text.trim().to_lower_ascii().replace(\",\", \"|\")\n",
            "  total = total + mapped.len()\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
    report_jit(
        "jit_numeric_conversion",
        concat!(
            "i = 0\ntotal = 0\n",
            "while i < 50000\n",
            "  case \"7f\".parse_int(16)\n",
            "  in Ok(value) then total = total + value\n",
            "  in Err(_) then total = total - 1\n",
            "  end\n",
            "  case \"12.5\".parse_float()\n",
            "  in Ok(value) then total = total + value.fixed(1).len()\n",
            "  in Err(_) then total = total - 1\n",
            "  end\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
        0,
    );
}

// ---------------------------------------------------------------
// Group 1: representative JIT programs.
// ---------------------------------------------------------------

const JIT_JSON_PARSE_SOURCE: &str = r#"
use std.json.Json
use std.json.parse

source = "{\"name\":\"loom\",\"values\":[1,2,3,4],\"ready\":true}"
round = 0
total = 0
while round < 2000
  case parse(source)
  in Ok(Json.Object(fields)) then total = total + fields.len()
  in _ then total = total - 1000
  end
  round = round + 1
end
total
"#;

#[test]
#[ignore]
fn bench_jit_cold_start_and_cache_pressure() {
    println!(
        "LOOM_JIT_COLD\tcase\tinterpreter_ms\tauto_ms\tauto_speedup\tcompiled_regions\tcode_bytes"
    );
    report_jit_cold("jit_cold_json_parse", JIT_JSON_PARSE_SOURCE);
    let many = many_hot_functions_source(300, 1000);
    report_jit_cold("jit_many_hot_functions", &many);
    report_jit_representative("jit_many_hot_functions_warm", &many);
}

#[test]
#[ignore]
fn bench_jit_representative_programs() {
    println!(
        "LOOM_JIT_PROGRAM\tcase\tinterpreter_ms\tauto_ms\tnative_ms\tauto_speedup\tnative_speedup\tauto_coverage\tnative_coverage\tauto_compiles\tauto_demotions\tauto_unsupported\tnative_unsupported\tauto_interpreter_exits\tnative_interpreter_exits\tauto_env_exits\tnative_env_exits\tnative_env_fallbacks"
    );
    report_jit_slot_calls(
        "jit_slot_call",
        concat!(
            "final class Box\n  value: Int = 3\nend\n",
            "def identity[T](value: T): T\n  value\nend\n",
            "index = 0\ntotal = 0\n",
            "while index < 1000000\n",
            "  box = Box()\n",
            "  total = total + identity(box.value)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_deep_recursion",
        concat!(
            "def down(n: Int): Int\n",
            "  if n <= 0 then 0 else down(n - 1) + 1 end\n",
            "end\n",
            "i = 0\ns = 0\n",
            "while i < 1000\n",
            "  s = s + down(1000)\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_call_branch",
        concat!(
            "def add1(value: Int): Int\n",
            "  if value < 0 then value - 1 else value + 1 end\n",
            "end\n",
            "i = 0\nwhile i < 1000000\n  i = add1(i)\nend\ni\n",
        ),
    );
    report_jit_representative(
        "jit_virtual_call",
        concat!(
            "class Base\n",
            "  def step(self, value: Int): Int\n    value + 1\n  end\n",
            "end\n",
            "class Child < Base\n",
            "  def step(self, value: Int): Int\n    value + 2\n  end\n",
            "end\n",
            "def run(value: Base): Int\n",
            "  index = 0\n  total = 0\n",
            "  while index < 1000000\n",
            "    total = total + value.step(index)\n",
            "    index = index + 1\n",
            "  end\n  total\n",
            "end\n",
            "run(Child())\n",
        ),
    );
    report_jit_representative(
        "jit_interface_call",
        concat!(
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
            "while index < 500000\n",
            "  total = total + read(left) + read(right)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_generic_virtual_call",
        concat!(
            "class Counter\n",
            "  def keep[U](self, other: U): Int\n    7\n  end\n",
            "end\n",
            "counter = Counter()\n",
            "index = 0\ntotal = 0\n",
            "while index < 1000000\n",
            "  total = total + counter.keep(index)\n",
            "  index = index + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_generic_call",
        concat!(
            "def identity[T](value: T): T\n  value\nend\n",
            "def outer[T](value: T): T\n  identity(value)\nend\n",
            "i = 0\ns = 0\n",
            "while i < 1000000\n",
            "  s = s + outer(i)\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_closure_call",
        concat!(
            "base = 7\n",
            "stored = do |value: Int|: Int base + value end\n",
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + stored(i)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_quick_exit",
        concat!(
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
        ),
    );
    report_jit_representative(
        "jit_numeric_surface",
        concat!(
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + (i & 7)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_option_values",
        concat!(
            "def read(value: Option[Int]): Int\n",
            "  case value\n",
            "  in Some(found) then found\n",
            "  in None then 0\n",
            "  end\n",
            "end\n",
            "i = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  value: Option[Int] = if i % 2 == 0 then Some(i) else None end\n",
            "  total = total + read(value)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_literal_loads",
        concat!(
            "i = 0\ntext = \"\"\nbytes = b\"\"\n",
            "while i < 1000000\n",
            "  text = \"hello\"\n",
            "  bytes = b\"\\x01\\x02\"\n",
            "  i = i + 1\n",
            "end\n",
            "if text.byte_len() == 5 and bytes.len() == 2 then i else 0 end\n",
        ),
    );
    report_jit_representative(
        "jit_interpreter_site",
        concat!(
            "items: [Int] = []\ni = 0\n",
            "while i < 50000\n",
            "  items.push(i)\n",
            "  i = i + 1\n",
            "end\nitems.len()\n",
        ),
    );
    report_jit_representative(
        "jit_class_init",
        concat!(
            "class Point\n  x: Int = 0\n  y: Int = 0\n",
            "  def init(mut self, x: Int, y: Int)\n",
            "    self.x = x\n    self.y = y\n  end\nend\n",
            "i = 0\ns = 0\nwhile i < 500000\n",
            "  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\n",
            "end\ns\n",
        ),
    );
    report_jit_representative(
        "jit_class_guard",
        concat!(
            "class Shape\nend\n",
            "class Circle < Shape\n  radius: Int = 3\nend\n",
            "class LargeCircle < Circle\nend\n",
            "def radius(shape: Shape): Int\n",
            "  if shape is Circle then (shape as Circle).radius else 0 end\n",
            "end\n",
            "shape: Shape = LargeCircle()\ni = 0\ntotal = 0\n",
            "while i < 1000000\n",
            "  total = total + radius(shape)\n",
            "  i = i + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_list_sort",
        concat!(
            "source = [16, 7, 12, 3, 10, 1, 14, 5, 8, 15, 2, 11, 6, 13, 4, 9]\n",
            "i = 0\nfirst = 0\nwhile i < 20000\n",
            "  values = source.copy()\n  values.sort()\n",
            "  first = values.at(0)\n  i = i + 1\n",
            "end\nfirst\n",
        ),
    );
    report_jit_representative(
        "jit_list_iteration",
        concat!(
            "items = [1, 2, 3, 4, 5, 6, 7, 8]\n",
            "round = 0\ntotal = 0\n",
            "while round < 100000\n",
            "  total = total + items.capacity()\n",
            "  for item in items\n",
            "    total = total + item\n",
            "  end\n",
            "  round = round + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_text_metadata",
        concat!(
            "def measure_string(value: String): Int\n",
            "  value.byte_len() * 10 + value.len()\n",
            "end\n",
            "def measure_view(value: Substring): Int\n",
            "  value.byte_len() * 10 + value.len()\n",
            "end\n",
            "text = \"aé猫z\"\n",
            "view = text.slice(1, 2).expect(\"the text slice exists\")\n",
            "i = 0\ntotal = 0\nhash = 0\n",
            "while i < 1000000\n",
            "  total = total + measure_string(text) + measure_view(view)\n",
            "  hash = hash_combine(hash, i)\n",
            "  i = i + 1\n",
            "end\n",
            "(total, hash)\n",
        ),
    );
    report_jit_representative(
        "jit_text_scalar_read",
        concat!(
            "text = \"aé猫z\"\n",
            "round = 0\ntotal = 0\n",
            "while round < 250000\n",
            "  index = 0\n",
            "  while index < text.len()\n",
            "    total = total + text.at(index).expect(\"the scalar exists\").codepoint()\n",
            "    index = index + 1\n",
            "  end\n",
            "  round = round + 1\n",
            "end\ntotal\n",
        ),
    );
    report_jit_representative(
        "jit_bytes_read",
        concat!(
            "def scan(bytes: Bytes): Int\n",
            "  total = 0\n  round = 0\n",
            "  while round < 250000\n",
            "    index = 0\n",
            "    while index < bytes.len()\n",
            "      total = total + bytes.at(index)\n",
            "      index = index + 1\n",
            "    end\n",
            "    round = round + 1\n",
            "  end\n",
            "  total\n",
            "end\n",
            "scan(Bytes(\"loom\"))\n",
        ),
    );
    report_jit_representative("jit_json_parse", JIT_JSON_PARSE_SOURCE);
    report_jit_representative(
        "jit_json_stringify",
        r#"
use std.json.Json
use std.json.stringify

fields = Map[String, Json]()
fields.put("name", Json.Text("loom"))
fields.put("ready", Json.Boolean(true))
values: [Json] = [Json.Number(1.0), Json.Number(2.0), Json.Number(3.0)]
fields.put("values", Json.ListValue(values))
document = Json.Object(fields)
round = 0
total = 0
while round < 2000
  case stringify(document)
  in Ok(text) then total = total + text.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
    report_jit_representative(
        "jit_http_parse",
        r#"
use std.http.Http

http = Http()
limits = http.default_limits()
wire = Bytes("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-Loom: ready\r\n\r\nworld")
round = 0
total = 0
while round < 2000
  case http.parse_response(wire, "GET", limits)
  in Ok(response) then total = total + response.status + response.body.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
    report_jit_representative(
        "jit_http_serialize",
        r#"
use std.http.Http
use std.http.HttpHeader
use std.http.HttpRequest

http = Http()
limits = http.default_limits()
request = HttpRequest(
  "POST",
  "/echo",
  [HttpHeader("Content-Type", Bytes("text/plain"))],
  Bytes("hello")
)
round = 0
total = 0
while round < 2000
  case http.serialize_request("example.test", 80, request, limits)
  in Ok(wire) then total = total + wire.len()
  in Err(_) then total = total - 1000
  end
  round = round + 1
end
total
"#,
    );
}

// ---------------------------------------------------------------
// Group 2: the language operations.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_language_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // An integer while loop: the interpreter dispatch floor.
    report(
        "int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        base,
    );

    // A direct call to a top-level function.
    report(
        "direct_call",
        1_000_000,
        "def add1(n: Int): Int\n  n + 1\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = add1(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call through the dispatch row.
    report(
        "virtual_call",
        1_000_000,
        "class Adder\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         a = Adder()\ni = 0\ns = 0\nwhile i < 1000000\n  s = a.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A field read and a field write on a mutable receiver.
    report(
        "field_rw",
        1_000_000,
        "class Cell\n  v: Int = 0\n  def step(mut self)\n    self.v = self.v + 1\n  end\nend\n\
         c = Cell()\ni = 0\nwhile i < 1000000\n  c.step()\n  i = i + 1\nend\nc.v\n",
        base,
    );

    // Closure creation plus a call.
    report(
        "closure_call",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + 1 }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Object construction.
    report(
        "class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        base,
    );

    // List append.
    report(
        "list_push",
        500_000,
        "xs: [Int] = []\ni = 0\nwhile i < 500000\n  xs.push(i)\n  i = i + 1\nend\nxs.len()\n",
        base,
    );

    // List index on a built list.
    report(
        "list_index",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + xs.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Map insert with integer keys.
    report(
        "map_insert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 200000\n  m.put(i, i)\n  i = i + 1\nend\nm.len()\n",
        base,
    );

    // Map lookup on a built map.
    report(
        "map_lookup",
        1_000_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  s = s + m.at(j % 1000)\n  j = j + 1\nend\ns\n",
        base,
    );

    // Each map removal leaves one tombstone. Reinsertion keeps the
    // live map size stable and exercises periodic compaction.
    report(
        "map_remove_reinsert",
        200_000,
        "m: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(i, i)\n  i = i + 1\nend\n\
         j = 0\nwhile j < 200000\n  key = j % 1000\n  m.remove(key)\n  m.put(key, key)\n  j = j + 1\nend\nm.len()\n",
        base,
    );

    // String interpolation formats one integer into new short text.
    // Accumulation here would measure quadratic copying instead.
    report(
        "string_interp",
        200_000,
        "s = \"\"\ni = 0\nwhile i < 200000\n  s = \"v#{i}\"\n  i = i + 1\nend\ns\n",
        base,
    );

    // Mixed integer arithmetic: multiply, divide, and modulo.
    report(
        "arith_mix",
        1_000_000,
        "i = 1\ns = 0\nwhile i < 1000001\n  s = s + i * 3 / 2 % 7\n  i = i + 1\nend\ns\n",
        base,
    );

    // One integer bitwise operation in a hot loop.
    report(
        "int_bitwise",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s ^ i\n  i = i + 1\nend\ns\n",
        base,
    );

    // One binary64 addition in a hot loop.
    report(
        "float_add",
        1_000_000,
        "i = 0\ns = 0.0\nwhile i < 1000000\n  s = s + 1.25\n  i = i + 1\nend\ns\n",
        base,
    );

    // Bytewise XOR allocates one frozen 32-byte result.
    report(
        "bytes_xor_32",
        20_000,
        "left = b\"0123456789abcdef0123456789abcdef\"\n\
         right = b\"ffffffffffffffffffffffffffffffff\"\n\
         value = left\ni = 0\nwhile i < 20000\n  value = left ^ right\n  i = i + 1\nend\nvalue.len()\n",
        base,
    );

    // One taken branch and one untaken branch per iteration.
    report(
        "branch",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  if i % 2 == 0\n    s = s + 1\n  else\n    s = s - 1\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // Integer equality keeps its sealed instruction inside a hot loop.
    report(
        "int_eq",
        1_000_000,
        "i = 0\nsame = false\nwhile i < 1000000\n  same = i == i\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Text equality keeps its native content instruction.
    report(
        "text_eq",
        1_000_000,
        "a = \"loom\"\nb = \"loom\"\ni = 0\nsame = false\nwhile i < 1000000\n  same = a == b\n  i = i + 1\nend\nsame\n",
        base,
    );

    // Generic equality measures one verified interface call.
    report(
        "partial_eq",
        1_000_000,
        "final class Token implements PartialEq\n  value: Int\n  def init(mut self, value: Int)\n    self.value = value\n  end\n  def __eq__(self, other: Token): Bool\n    self.value == other.value\n  end\nend\ndef same[T: PartialEq](a: T, b: T): Bool\n  a == b\nend\na = Token(7)\nb = Token(7)\ni = 0\nequal = false\nwhile i < 1000000\n  equal = same(a, b)\n  i = i + 1\nend\nequal\n",
        base,
    );

    // A generic interface call selects one default method.
    report(
        "interface_default",
        1_000_000,
        "interface Valued\n  def value(self): Int\n    7\n  end\nend\nfinal class Token implements Valued\nend\ndef read[T: Valued](value: T): Int\n  value.value()\nend\ntoken = Token()\ni = 0\nvalue = 0\nwhile i < 1000000\n  value = read(token)\n  i = i + 1\nend\nvalue\n",
        base,
    );

    // Conditional list equality compares all elements.
    report(
        "list_eq",
        200_000,
        "left = [1, 2, 3, 4, 5, 6, 7, 8]\nright = left.copy()\n\
         i = 0\nequal = false\nwhile i < 200000\n  equal = left == right\n  i = i + 1\nend\nequal\n",
        base,
    );

    // Conditional list hashing combines all elements.
    report(
        "list_hash",
        200_000,
        "values = [1, 2, 3, 4, 5, 6, 7, 8]\ni = 0\nhash = 0\n\
         while i < 200000\n  hash = hash_of(values)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Tuple hashing uses the ordinary conditional interface path.
    report(
        "tuple_hash",
        200_000,
        "value = (1, 2, 3, 4)\ni = 0\nhash = 0\nwhile i < 200000\n  \
         hash = hash_of(value)\n  i = i + 1\nend\nhash\n",
        base,
    );

    // Closure-free sorting copies and sorts sixteen integers.
    report(
        "list_sort",
        20_000,
        "source = [16, 7, 12, 3, 10, 1, 14, 5, 8, 15, 2, 11, 6, 13, 4, 9]\n\
         i = 0\nfirst = 0\nwhile i < 20000\n  values = source.copy()\n  values.sort()\n  first = values.at(0)\n  i = i + 1\nend\nfirst\n",
        base,
    );

    // Recursion: the call path with a growing activation stack.
    report(
        "recursion",
        1_000_000,
        "def down(n: Int): Int\n  if n <= 0\n    0\n  else\n    down(n - 1) + 1\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 1000\n  s = s + down(1000)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A virtual call that resolves on an inherited method.
    report(
        "inherit_call",
        1_000_000,
        "class Base\n  step: Int = 1\n  def bump(self, n: Int): Int\n    n + self.step\n  end\nend\n\
         class Derived < Base\nend\n\
         d = Derived()\ni = 0\ns = 0\nwhile i < 1000000\n  s = d.bump(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A closure that captures a local, against the free closure above.
    report(
        "closure_capture",
        1_000_000,
        "k = 7\ni = 0\ns = 0\nwhile i < 1000000\n  f = { |x: Int|: Int x + k }\n  s = f(s)\n  i = i + 1\nend\ns\n",
        base,
    );

    // A generic call: the type application path.
    report(
        "generic_call",
        1_000_000,
        "def pick[T](a: T, b: T): T\n  a\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  s = pick(s + 1, 0)\n  i = i + 1\nend\ns\n",
        base,
    );

    // Enum construction plus a `case` dispatch over two arms.
    report(
        "enum_case",
        1_000_000,
        "enum Step\n  Up(v: Int)\n  Down(v: Int)\nend\n\
         i = 0\ns = 0\nwhile i < 1000000\n  e: Step = Up(1)\n  \
         s = s + case e\n  in Up(v) then v\n  in Down(v) then 0 - v\n  end\n  i = i + 1\nend\ns\n",
        base,
    );

    // The non-faulting list access: a native op that builds a core
    // `Option`, then a `case` over it.
    report(
        "option_case",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 1000000\n  \
         s = s + case xs.get(j % 1000)\n  in Some(v) then v\n  in None then 0\n  end\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with string keys, against the integer-key cases above.
    report(
        "map_str_lookup",
        500_000,
        "m: {String: Int} = {}\ni = 0\nwhile i < 1000\n  m.put(\"k#{i}\", i)\n  i = i + 1\nend\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(\"k500\")\n  j = j + 1\nend\ns\n",
        base,
    );

    // A map with immutable byte keys uses the native byte hash path.
    report(
        "map_bytes_lookup",
        500_000,
        "key = Bytes(\"loom\")\nm: {Bytes: Int} = {}\nm.put(key, 7)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // A user key uses one hash call and one equality call per lookup.
    report(
        "map_hashable_lookup",
        500_000,
        "final class Key implements Hashable\n  value: Int\n  \
         def init(mut self, value: Int)\n    self.value = value\n  end\n  \
         def __eq__(self, other: Key): Bool\n    self.value == other.value\n  end\n  \
         def __hash__(self): Int\n    self.value\n  end\nend\n\
         key = Key(7).freeze()\nm = Map[Key, Int]()\nm.put(key, 9)\n\
         j = 0\ns = 0\nwhile j < 500000\n  s = s + m.at(key)\n  j = j + 1\nend\ns\n",
        base,
    );

    // The string builder uses the growable text path.
    report(
        "string_builder",
        500_000,
        "b = StringBuilder()\ni = 0\nwhile i < 500000\n  b.append(\"x\")\n  i = i + 1\nend\nb.build()\n",
        base,
    );

    // Scalar traversal uses one forward UTF-8 byte cursor.
    report(
        "text_each",
        600_000,
        "def ignore(value: Char): ()\n  ()\nend\n\
         text = \"aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫aé猫\"\n\
         i = 0\nwhile i < 10000\n  text.each(ignore)\n  i = i + 1\nend\ntext.len()\n",
        base,
    );

    // Split a document into fields. This case measures the design and
    // not the search: a Loom piece shares the source allocation, and
    // a CPython piece is a copy. The count is pieces, not iterations.
    report(
        "text_split",
        320_000,
        "row = \"alpha,beta,gamma,delta,epsilon,zeta,eta,theta,iota,kappa\"\n\
         total = 0\ni = 0\nwhile i < 32000\n  total = total + row.split(\",\").len()\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Split a line into a key and a value. This is the shape a
    // configuration or header parser writes, and it allocates one
    // Option and one tuple for each line.
    report(
        "text_split_once",
        200_000,
        "line = \"content-length: 4096\"\ntotal = 0\ni = 0\nwhile i < 200000\n\
         \x20 total = total + case line.split_once(\": \")\n\
         \x20 in Some((key, _)) then key.byte_len()\n  in None then 0\n  end\n\
         \x20 i = i + 1\nend\ntotal\n",
        base,
    );

    // Narrow one piece and keep it as a view. Loom copies nothing.
    report(
        "text_trim",
        500_000,
        "padded = \"   content-length   \"\ntotal = 0\ni = 0\nwhile i < 500000\n\
         \x20 total = total + padded.trim().byte_len()\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // Decode bytes to text. Loom validates once and shares the
    // allocation. CPython allocates and copies.
    report(
        "bytes_decode",
        200_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 512\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 200000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // The same decode over a large buffer. The Loom cost is one
    // validation plus one allocation and does not grow with the copy
    // CPython must make, so this pair locates the crossing point.
    report(
        "bytes_decode_large",
        20_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 65536\n  b.append(97)\n  i = i + 1\nend\n\
         raw = b.finish()\ntotal = 0\nj = 0\nwhile j < 20000\n\
         \x20 total = total + case raw.utf8_view()\n  in Ok(text) then text.byte_len()\n\
         \x20 in Err(_) then 0\n  end\n  j = j + 1\nend\ntotal\n",
        base,
    );

    // Compare two strings. The ordering hooks reach one intrinsic.
    report(
        "text_compare",
        1_000_000,
        "a = \"content-length\"\nb = \"content-type\"\ntotal = 0\ni = 0\n\
         while i < 1000000\n  if a < b\n    total = total + 1\n  end\n  i = i + 1\nend\ntotal\n",
        base,
    );

    // The byte buffer.
    report(
        "byte_buffer",
        500_000,
        "b = ByteBuffer()\ni = 0\nwhile i < 500000\n  b.append(65)\n  i = i + 1\nend\nb.len()\n",
        base,
    );

    // The two cases below run the same workload inside a `World`.
    // The allocating case reports local heap work. The integer case
    // reports the activation loop cost alone.
    report_world(
        "world_class_init",
        500_000,
        "class Point\n  x: Int = 0\n  y: Int = 0\n  def init(mut self, x: Int, y: Int)\n    \
         self.x = x\n    self.y = y\n  end\nend\n\
         i = 0\ns = 0\nwhile i < 500000\n  p = Point(i, i)\n  s = s + p.x\n  i = i + 1\nend\ns\n",
        "Done(124999750000)",
    );
    report_world(
        "world_int_loop",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + i\n  i = i + 1\nend\ns\n",
        "Done(499999500000)",
    );
    report_world_with(
        "direct_clock",
        1_000_000,
        "i = 0\ns = 0\nwhile i < 1000000\n  s = s + sys.clock.now()\n  i = i + 1\nend\ns\n",
        &["Clock"],
        "Done(501000500000)",
    );
}

#[test]
#[ignore]
fn bench_collection_operations() {
    let base = baseline();
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    println!(
        "LOOM\t_baseline\t1\t{:.1}\t{:.3}",
        base.as_nanos() as f64,
        base.as_secs_f64() * 1e3
    );

    // Native list traversal creates no iterator or Option per element.
    report(
        "list_for",
        1_000_000,
        "xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
         rounds = 0\ns = 0\nwhile rounds < 1000\n  for value in xs\n    s = s + value\n  end\n\
           rounds = rounds + 1\nend\ns\n",
        base,
    );

    // A nonescaping callback avoids one closure object per call.
    report(
        "list_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, n: Int)\n    self.value = self.value + n\n  end\nend\n\
         xs: [Int] = []\ni = 0\nwhile i < 1000\n  xs.push(i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  xs.each() { |value: Int| total.add(value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );

    // This eager pipeline applies three ordinary core algorithms.
    report(
        "list_pipeline",
        60_000,
        "xs: [Int] = []\ni = 0\nwhile i < 20000\n  xs.push(i)\n  i = i + 1\nend\n\
         mapped = xs.map[Int]() { |value: Int| value + 1 }\n\
         filtered = mapped.filter() { |value: Int| value % 2 == 0 }\n\
         filtered.fold[Int](0) { |sum: Int, value: Int| sum + value }\n",
        base,
    );

    // Map traversal passes the key and value without a tuple object.
    report(
        "map_each",
        1_000_000,
        "class Total\n  value: Int = 0\n  def add(mut self, key: Int, value: Int)\n    self.value = self.value + key + value\n  end\nend\n\
         table: {Int: Int} = {}\ni = 0\nwhile i < 1000\n  table.put(i, i)\n  i = i + 1\nend\n\
           total = Total()\nrounds = 0\nwhile rounds < 1000\n  table.each() { |key: Int, value: Int| total.add(key, value) }\n\
           rounds = rounds + 1\nend\ntotal.value\n",
        base,
    );
}

#[test]
#[ignore]
fn bench_proc_operations() {
    let source = "class Adder < Proc[Int]\n\
                  \x20 total: Int = 0\n\
                  \x20 def on_spawn(mut self): Int with Proc\n\
                  \x20   loop do\n\
                  \x20     case self.receive()\n\
                  \x20     in Msg(n)\n\
                  \x20       self.total = self.total + n\n\
                  \x20     in Closed\n\
                  \x20       return self.total\n\
                  \x20     end\n\
                  \x20   end\n\
                  \x20 end\n\
                  end\n\
                  h = Adder.spawn()\n\
                  i = 0\n\
                  while i < 20000\n  h.send(1)\n  i = i + 1\nend\n\
                  h.close()\n\
                  case h.done()\n\
                  in Ok(v)  then v\n\
                  in Err(_) then -1\n\
                  end\n";
    let elapsed = time_world(source, &["Proc"], config(), "Done(20000)");
    println!(
        "LOOM\tproc_send_receive\t20000\t{:.1}\t{:.3}",
        elapsed.as_nanos() as f64 / 20_000.0,
        elapsed.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_in_memory_branch() {
    let snapshot_reuse = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  image = case original.snapshot()
  in Ok(value) then value
  in Err(error) then panic(display(error))
  end
  total = 0
  index = 0
  while index < 100
    copy = case sys.vm.Vm().restore(image)
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let snapshot_fresh = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  total = 0
  index = 0
  while index < 100
    image = case original.snapshot()
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    copy = case sys.vm.Vm().restore(image)
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let branch = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

def finish(run: Run[Int], answer: Int): Int with Vm
  case run.drive()
  in Asked(request)
    case request
    in Call(Rand.Int, call, (_, _))
      run.answer(call, answer)
      run.run().value_or(-1000)
    in _ then -2000
    end
  in Done(value) then value
  in Fault(_) then -3000
  end
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(_)
  total = 0
  index = 0
  while index < 100
    copy = case original.branch()
    in Ok(value) then value
    in Err(error) then panic(display(error))
    end
    total = total + finish(copy, index)
    index = index + 1
  end
  finish(original, 100)
  total
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let answered_branch = r#"
def choose(): Int with Rand.Int
  sys.rand.int(0, 100)
end

original = sys.vm.Vm().activate_or_fault(choose, args: ())
case original.drive()
in Asked(request)
  case request
  in Call(Rand.Int, call, (_, _))
    total = 0
    index = 0
    while index < 100
      copy = case original.branch_answer(call, index)
      in Ok(value) then value
      in Err(error) then panic(display(error))
      end
      total = total + copy.run().value_or(-1000)
      index = index + 1
    end
    original.answer(call, 100)
    original.run().value_or(-2000)
    total
  in _ then -3000
  end
in Done(value) then value
in Fault(fault) then raise(fault)
end
"#;
    let reused = time_world(snapshot_reuse, &["Vm"], config(), "Done(4950)");
    let fresh = time_world(snapshot_fresh, &["Vm"], config(), "Done(4950)");
    let branched = time_world(branch, &["Vm"], config(), "Done(4950)");
    let answered = time_world(answered_branch, &["Vm"], config(), "Done(4950)");
    let reuse_ratio = branched.as_secs_f64() / reused.as_secs_f64();
    let fresh_ratio = branched.as_secs_f64() / fresh.as_secs_f64();
    assert!(
        fresh_ratio <= 1.0,
        "an in-memory branch must beat a fresh snapshot and restore"
    );
    println!(
        "LOOM\tvm_branch\t100\t{:.3}\t{:.3}\t{:.3}\t{reuse_ratio:.3}\t{fresh_ratio:.3}",
        reused.as_secs_f64() * 1e3,
        fresh.as_secs_f64() * 1e3,
        branched.as_secs_f64() * 1e3
    );
    let answered_ratio = answered.as_secs_f64() / branched.as_secs_f64();
    println!(
        "LOOM\tvm_branch_answer\t100\t{:.3}\t{answered_ratio:.3}",
        answered.as_secs_f64() * 1e3
    );
    assert!(
        answered_ratio <= 1.05,
        "answered branching took {answered_ratio:.3} times plain branching"
    );
}

#[test]
#[ignore]
fn bench_vm_machine_lifecycle() {
    let (source, expected) = multishot_queens_source(9);
    let adaptive = time_world(&source, &["Vm"], config(), &expected);
    let former_limit = time_world(
        &source,
        &["Vm"],
        VmConfig {
            max_children: 1_024,
            ..config()
        },
        &expected,
    );
    let ratio = adaptive.as_secs_f64() / former_limit.as_secs_f64();
    println!("LOOM\tcase\tsize\tadaptive_ms\tformer_limit_ms\tratio");
    println!(
        "LOOM\tvm_machine_lifecycle\t9\t{:.3}\t{:.3}\t{ratio:.3}",
        adaptive.as_secs_f64() * 1e3,
        former_limit.as_secs_f64() * 1e3,
    );
    assert!(
        ratio <= 1.20,
        "adaptive reclamation took {ratio:.3} times limit-driven reclamation"
    );
}

#[test]
#[ignore]
fn bench_parallel_multishot_queens() {
    let source = std::fs::read_to_string(
        lm_testkit::repo_root().join("examples/14-vm-as-multishot-search/07-parallel-n-queens.lm"),
    )
    .expect("the multishot benchmark source reads")
    .replace("parallel_solutions(5)", "parallel_solutions(7)");
    let (direct_source, direct_expected) = iterable_queens_source(7, false);
    let direct = time_world(&direct_source, &[], config(), &direct_expected);
    let deterministic = time_world(&source, &["Vm", "Wait"], config(), "Done(40)");
    let parallel = time_parallel_world_with(&source, 4, &["Vm", "Wait"], "Done(40)");
    let speedup = deterministic.as_secs_f64() / parallel.as_secs_f64();
    let overhead = deterministic.as_secs_f64() / direct.as_secs_f64();
    println!(
        "LOOM\tcase\tsize\tworkers\tdirect_ms\tdeterministic_ms\tparallel_ms\tspeedup\toverhead"
    );
    println!(
        "LOOM\tparallel_multishot_queens\t7\t4\t{:.3}\t{:.3}\t{:.3}\t{speedup:.3}\t{overhead:.3}",
        direct.as_secs_f64() * 1e3,
        deterministic.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_parallel_cpu_scaling() {
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    for (tasks, workers, gate) in [(2, 2, 1.7), (4, 4, 3.0)] {
        let (source, expected) = parallel_cpu_source(tasks, 1_000_000);
        let serial = time_parallel_world(&source, 1, &expected);
        let parallel = time_parallel_world(&source, workers, &expected);
        let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
        println!(
            "LOOM\tparallel_cpu\t{tasks}\t{workers}\t{:.3}\t{:.3}\t{speedup:.3}",
            serial.as_secs_f64() * 1e3,
            parallel.as_secs_f64() * 1e3
        );
        assert!(
            speedup >= gate,
            "{tasks} tasks reached {speedup:.3}x, below the {gate:.1}x gate"
        );
    }
}

#[test]
#[ignore]
fn bench_parallel_allocating_scaling() {
    let (source, expected) = parallel_allocating_source(8, 250_000);
    let serial = time_parallel_world(&source, 1, &expected);
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    for (workers, gate) in [(4, 3.0), (8, 5.0)] {
        let parallel = time_parallel_world(&source, workers, &expected);
        let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
        println!(
            "LOOM\tparallel_allocating\t8\t{workers}\t{:.3}\t{:.3}\t{speedup:.3}",
            serial.as_secs_f64() * 1e3,
            parallel.as_secs_f64() * 1e3
        );
        assert!(
            speedup >= gate,
            "eight allocating tasks reached {speedup:.3}x on {workers} workers"
        );
    }
}

#[test]
#[ignore]
fn bench_parallel_allocation_churn() {
    let (source, expected) = parallel_churn_source(8, 250_000);
    let serial = time_parallel_world(&source, 1, &expected);
    let parallel = time_parallel_world(&source, 8, &expected);
    let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    println!(
        "LOOM\tparallel_allocation_churn\t8\t8\t{:.3}\t{:.3}\t{speedup:.3}",
        serial.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tparallel_counters\tcase\tworkers\tproc_slices\tcontinuations\trotations\trecalls\tquiescence\tcollection_quiescence\tinstructions\theap_growth\tnative_calls\tcollections\tclose_hits\tclose_misses\tderive_hits\tderive_misses"
    );
    report_parallel_counters("allocation_churn", &source, 1, &expected);
    report_parallel_counters("allocation_churn", &source, 8, &expected);
    let (steady_source, steady_expected) = parallel_allocating_source(8, 250_000);
    report_parallel_counters("steady_allocation", &steady_source, 1, &steady_expected);
    report_parallel_counters("steady_allocation", &steady_source, 8, &steady_expected);
    assert!(
        speedup >= 5.0,
        "eight churn tasks reached {speedup:.3}x on eight workers"
    );
}

#[test]
#[ignore]
fn bench_parallel_split_queens() {
    let (source, expected) = parallel_queens_source(12);
    let serial = time_parallel_world(&source, 1, &expected);
    let parallel = time_parallel_world(&source, 12, &expected);
    let speedup = serial.as_secs_f64() / parallel.as_secs_f64();
    println!("LOOM\tcase\ttasks\tworkers\tserial_ms\tparallel_ms\tspeedup");
    println!(
        "LOOM\tparallel_split_queens\t12\t12\t{:.3}\t{:.3}\t{speedup:.3}",
        serial.as_secs_f64() * 1e3,
        parallel.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_parallel_par_map_queens() {
    let (manual, expected) = manual_par_map_queens_source(13);
    let (library, library_expected) = iterable_queens_source(13, true);
    assert_eq!(library_expected, expected);
    println!("LOOM\tcase\tworkers\tmanual_ms\tpar_map_ms\tratio");
    for workers in [4, 12] {
        let manual_time = time_parallel_world(&manual, workers, &expected);
        let library_time = time_parallel_world(&library, workers, &expected);
        let ratio = library_time.as_secs_f64() / manual_time.as_secs_f64();
        println!(
            "LOOM\tpar_map_queens\t{workers}\t{:.3}\t{:.3}\t{ratio:.3}",
            manual_time.as_secs_f64() * 1e3,
            library_time.as_secs_f64() * 1e3
        );
        assert!(
            ratio <= 1.08,
            "par_map took {ratio:.3} times the manual implementation"
        );
    }

    let (sequential, sequential_expected) = iterable_queens_source(13, false);
    let map_time = time_world(&sequential, &[], config(), &sequential_expected);
    let par_map_time = time_world(&library, &["Proc"], config(), &expected);
    let ratio = par_map_time.as_secs_f64() / map_time.as_secs_f64();
    println!(
        "LOOM\tpar_map_deterministic\t1\t{:.3}\t{:.3}\t{ratio:.3}",
        map_time.as_secs_f64() * 1e3,
        par_map_time.as_secs_f64() * 1e3
    );
    assert!(
        ratio <= 1.08,
        "deterministic par_map took {ratio:.3} times sequential map"
    );
}

#[test]
#[ignore]
fn bench_parallel_messages() {
    println!(
        "LOOM\tgroup\tcase\tmessages\tworkers\tdeterministic_ms\t\
         deterministic_p95_ms\tparallel_ms\tparallel_p95_ms\tratio"
    );

    let mut deterministic_total = Duration::ZERO;
    let mut parallel_total = Duration::ZERO;
    let mut measured = 0;
    let mut record = |result: (Duration, Duration)| {
        deterministic_total += result.0;
        parallel_total += result.1;
        measured += 1;
    };

    let (ping, ping_expected, ping_messages) = parallel_ping_source(1, 2_000);
    if selected("ping_pong") {
        record(report_message_case(
            "ping_pong",
            ping_messages,
            &ping,
            &ping_expected,
        ));
    }

    let stream = r#"
class StreamSink < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed
        return total
      end
    end
  end
end

sink = StreamSink.spawn()
i = 0
while i < 500
  sink.send(1)
  i = i + 1
end
sink.close()
sink.done()
"#;
    if selected("stream") {
        record(report_message_case("stream", 500, stream, "Done(Ok(500))"));
    }

    let (pairs, pairs_expected, pair_messages) = parallel_ping_source(4, 500);
    if selected("independent_pairs") {
        record(report_message_case(
            "independent_pairs",
            pair_messages,
            &pairs,
            &pairs_expected,
        ));
    }

    let many_senders = r#"
class ManySink < Proc[Int]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(value)
        total = total + value
      in Closed
        return total
      end
    end
  end
end

class ManySender < Proc
  sink: Handle[Int, Int]

  def init(mut self, sink: Handle[Int, Int])
    self.sink = sink
  end

  def on_spawn(self): Int with Proc
    i = 0
    while i < 100
      self.sink.send(1)
      i = i + 1
    end
    i
  end
end

sink = ManySink.spawn()
s0 = ManySender.spawn(sink)
s1 = ManySender.spawn(sink)
s2 = ManySender.spawn(sink)
s3 = ManySender.spawn(sink)
s4 = ManySender.spawn(sink)
s5 = ManySender.spawn(sink)
s6 = ManySender.spawn(sink)
s7 = ManySender.spawn(sink)
s0.done()
s1.done()
s2.done()
s3.done()
s4.done()
s5.done()
s6.done()
s7.done()
sink.close()
sink.done()
"#;
    if selected("many_senders") {
        record(report_message_case(
            "many_senders",
            800,
            many_senders,
            "Done(Ok(800))",
        ));
    }

    let allocated = r#"
class PayloadSink < Proc[[Int]]
  def on_spawn(self): Int with Proc
    total = 0
    loop do
      case self.receive()
      in Msg(values)
        total = total + values.len()
      in Closed
        return total
      end
    end
  end
end

payload = list_repeated[Int](7, 32).freeze()
sink = PayloadSink.spawn()
i = 0
while i < 200
  sink.send(payload)
  i = i + 1
end
sink.close()
sink.done()
"#;
    if selected("allocated_stream") {
        record(report_message_case(
            "allocated_stream",
            200,
            allocated,
            "Done(Ok(6400))",
        ));
    }

    if measured > 0 {
        let aggregate = deterministic_total.as_secs_f64() / parallel_total.as_secs_f64();
        assert!(
            aggregate >= 0.95,
            "message throughput reached {aggregate:.3}x in aggregate"
        );
    }
}

// ---------------------------------------------------------------
// Group 2: the type checker.
// ---------------------------------------------------------------

/// Generate a module of `n` small functions that call their
/// predecessor, plus a class with `n` methods.
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

// ---------------------------------------------------------------
// Group 2: the bundled core image.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_core_compilation() {
    let empty = lm_source::parse::parse("").expect("the empty module parses");
    let check = || {
        lm_hir::check_module_with(
            &empty,
            lm_hir::CheckOptions {
                prelude: false,
                build_core_provider: true,
                ..lm_hir::CheckOptions::default()
            },
        )
        .expect("the core image checks")
    };
    let mut check_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let hir = check();
        let elapsed = start.elapsed();
        std::hint::black_box(hir.funcs.len());
        if round > 0 {
            check_runs.push(elapsed);
        }
    }

    let hir = check();
    let mut lower_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let module = lm_hir::lower_module(&hir);
        let elapsed = start.elapsed();
        std::hint::black_box(module.funcs.len());
        if round > 0 {
            lower_runs.push(elapsed);
        }
    }

    let mut compile_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let module = lm_hir::core_image();
        let elapsed = start.elapsed();
        std::hint::black_box(module.funcs.len());
        if round > 0 {
            compile_runs.push(elapsed);
        }
    }

    let module = lm_hir::core_image();
    let instruction_count: usize = module
        .funcs
        .iter()
        .flat_map(|function| &function.blocks)
        .map(Vec::len)
        .sum();
    let bytes = lm_bytecode::encode(&module);
    let artifact_unit = lm_compiler::core_link_unit().expect("the core artifact has an identity");
    let artifact = lm_bytecode::artifact::Artifact::new(artifact_unit.as_ref().clone(), Vec::new())
        .expect("the core artifact graph is valid");
    let artifact_bytes =
        lm_bytecode::artifact::encode(&artifact).expect("the core artifact encodes");
    let mut artifact_encode_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let encoded = lm_bytecode::artifact::encode(&artifact).expect("the core artifact encodes");
        let elapsed = start.elapsed();
        std::hint::black_box(encoded.len());
        if round > 0 {
            artifact_encode_runs.push(elapsed);
        }
    }
    let mut artifact_decode_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let decoded =
            lm_bytecode::artifact::decode(&artifact_bytes).expect("the core artifact decodes");
        let elapsed = start.elapsed();
        std::hint::black_box(decoded.units().len());
        if round > 0 {
            artifact_decode_runs.push(elapsed);
        }
    }
    let mut decode_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let decoded = lm_bytecode::decode(&bytes).expect("the core image decodes");
        let elapsed = start.elapsed();
        std::hint::black_box(decoded.funcs.len());
        if round > 0 {
            decode_runs.push(elapsed);
        }
    }

    let decoded = lm_bytecode::decode(&bytes).expect("the core image decodes");
    let mut verify_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        lm_verify::verify_module(&decoded).expect("the core image verifies");
        let elapsed = start.elapsed();
        if round > 0 {
            verify_runs.push(elapsed);
        }
    }

    let mut structure_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        lm_verify::verify_structure_only(&decoded).expect("the core structure verifies");
        let elapsed = start.elapsed();
        if round > 0 {
            structure_runs.push(elapsed);
        }
    }

    let mut hash_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let hash = lm_bytecode::identity::verification_hash(&decoded);
        let elapsed = start.elapsed();
        std::hint::black_box(hash);
        if round > 0 {
            hash_runs.push(elapsed);
        }
    }

    let mut identity_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let identity =
            lm_bytecode::identity::module_identity(&decoded).expect("the core image has identity");
        let elapsed = start.elapsed();
        std::hint::black_box(identity.semantic_hash);
        if round > 0 {
            identity_runs.push(elapsed);
        }
    }

    let mut publish_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let mut arena = lm_link::CodeArena::new();
        let start = Instant::now();
        let namespace = arena
            .publish(artifact.clone(), None)
            .expect("the core artifact publishes");
        let elapsed = start.elapsed();
        std::hint::black_box(
            arena
                .namespace(namespace)
                .expect("the core namespace exists")
                .tables()
                .funcs
                .len(),
        );
        if round > 0 {
            publish_runs.push(elapsed);
        }
    }

    let mut load_runs: Vec<Duration> = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let decoded =
            lm_bytecode::artifact::decode(&artifact_bytes).expect("the core artifact decodes");
        let mut arena = lm_link::CodeArena::new();
        let namespace = arena
            .publish(decoded, None)
            .expect("the core artifact publishes");
        let elapsed = start.elapsed();
        std::hint::black_box(namespace);
        if round > 0 {
            load_runs.push(elapsed);
        }
    }

    let mut arena = lm_link::CodeArena::new();
    arena
        .publish(artifact.clone(), None)
        .expect("the core artifact publishes");
    let mut repeat_publish_runs: Vec<Duration> = Vec::new();
    for _ in 0..ROUNDS {
        let start = Instant::now();
        let namespace = arena
            .publish(artifact.clone(), None)
            .expect("the core artifact republishes");
        let elapsed = start.elapsed();
        std::hint::black_box(namespace);
        repeat_publish_runs.push(elapsed);
    }

    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact.clone(), None)
        .expect("the core artifact publishes");
    let vm = Vm::new(arena, namespace, VmConfig::default());
    let interface_witness_entries = vm.interface_witness_entries();

    println!(
        "LOOM\tcore_check\t{}\t{}\t{:.3}\tms",
        hir.classes.len(),
        hir.funcs.len(),
        median(check_runs).as_secs_f64() * 1e3
    );
    println!("LOOM\tcore_hir_types\t{}", hir.store.type_count());
    println!(
        "LOOM\tcore_lower\t{}\t{}\t{:.3}\tms",
        hir.classes.len(),
        hir.funcs.len(),
        median(lower_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_compile\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(compile_runs).as_secs_f64() * 1e3
    );
    println!("LOOM\tcore_instructions\t{instruction_count}");
    println!(
        "LOOM\tcore_instruction_width\t{}\tbytes",
        std::mem::size_of::<lm_bytecode::Instr>()
    );
    println!(
        "LOOM\tcore_decode\t{}\t{}\t{:.3}\tms",
        bytes.len(),
        module.funcs.len(),
        median(decode_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_artifact_encode\t{}\t{}\t{:.3}\tms",
        artifact_bytes.len(),
        artifact.units().len(),
        median(artifact_encode_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_artifact_decode\t{}\t{}\t{:.3}\tms",
        artifact_bytes.len(),
        artifact.units().len(),
        median(artifact_decode_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_verify\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(verify_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_verify_structure\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(structure_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_verify_hash\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(hash_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_identity\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(identity_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_publish\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(publish_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_interface_witnesses\t{}\t{}\t{}\tentries",
        module.classes.len(),
        module.interfaces.len(),
        interface_witness_entries
    );
    println!(
        "LOOM\tcore_load\t{}\t{}\t{:.3}\tms",
        bytes.len(),
        module.funcs.len(),
        median(load_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tcore_repeat_publish\t{}\t{}\t{:.3}\tms",
        module.classes.len(),
        module.funcs.len(),
        median(repeat_publish_runs).as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_program_artifact_linking() {
    let source = lm_source::SourceFile::new("tiny.lm", "1\n");
    let compiled = lm_compiler::compile_source("bench.main", &source, true)
        .expect("the tiny program compiles");
    let mut source_env = lm_compiler::core_link_env().expect("the core environment builds");
    lm_testkit::bind_compiled_unit(&mut source_env, compiled.root.clone())
        .expect("the tiny module binds");
    let source_env = source_env.freeze();
    let artifact = compiled.artifact;
    let bytes = lm_bytecode::artifact::encode(&artifact).expect("the artifact encodes");
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let mut decode_runs = Vec::new();
    let mut link_runs = Vec::new();
    let mut cold_runs = Vec::new();
    let mut compile_runs = Vec::new();
    let mut collect_runs = Vec::new();
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let compiled = lm_compiler::compile_source("bench.main", &source, true)
            .expect("the tiny program compiles");
        let elapsed = start.elapsed();
        std::hint::black_box(compiled.artifact.id());
        if round > 0 {
            compile_runs.push(elapsed);
        }

        let start = Instant::now();
        let collected = source_env
            .artifact("bench.main")
            .expect("the tiny artifact collects");
        let elapsed = start.elapsed();
        std::hint::black_box(collected.id());
        if round > 0 {
            collect_runs.push(elapsed);
        }

        let start = Instant::now();
        let decoded = lm_bytecode::artifact::decode(&bytes).expect("the artifact decodes");
        let elapsed = start.elapsed();
        std::hint::black_box(decoded.id());
        if round > 0 {
            decode_runs.push(elapsed);
        }

        let start = Instant::now();
        let mut arena = lm_link::CodeArena::new();
        let namespace = arena
            .publish(artifact.clone(), Some(core.clone()))
            .expect("the artifact publishes");
        let elapsed = start.elapsed();
        std::hint::black_box(
            arena
                .namespace(namespace)
                .expect("the namespace exists")
                .tables()
                .funcs
                .len(),
        );
        if round > 0 {
            link_runs.push(elapsed);
        }

        let start = Instant::now();
        let decoded = lm_bytecode::artifact::decode(&bytes).expect("the artifact decodes");
        let mut arena = lm_link::CodeArena::new();
        let namespace = arena
            .publish(decoded, Some(core.clone()))
            .expect("the artifact publishes");
        let elapsed = start.elapsed();
        std::hint::black_box(namespace);
        if round > 0 {
            cold_runs.push(elapsed);
        }
    }
    println!(
        "LOOM\tprogram_artifact\t{}\t{}\t{}\t{}\tbytes_units_classes_functions",
        bytes.len(),
        artifact.units().len(),
        artifact.root().module().classes.len(),
        artifact.root().module().funcs.len()
    );
    println!(
        "LOOM\tprogram_artifact_decode\t{:.3}\tms",
        median(decode_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tprogram_artifact_compile\t{:.3}\tms",
        median(compile_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tprogram_artifact_collect\t{:.3}\tms",
        median(collect_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tprogram_artifact_publish\t{:.3}\tms",
        median(link_runs).as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tprogram_artifact_cold_load\t{:.3}\tms",
        median(cold_runs).as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_arena_publication_scaling() {
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let core_artifact = lm_bytecode::artifact::Artifact::new(core.as_ref().clone(), Vec::new())
        .expect("the core artifact builds");
    let artifacts: Vec<_> = (0..100)
        .map(|index| {
            let path = format!("bench.unit{index}");
            let source = lm_source::SourceFile::new(format!("unit-{index}.lm"), "1\n");
            lm_compiler::compile_source(&path, &source, true)
                .expect("the tiny unit compiles")
                .artifact
        })
        .collect();
    let mut first = Vec::with_capacity(ROUNDS);
    let mut tenth = Vec::with_capacity(ROUNDS);
    let mut hundredth = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let mut arena = lm_link::CodeArena::new();
        arena
            .publish(core_artifact.clone(), None)
            .expect("the core artifact publishes");
        for (index, artifact) in artifacts.iter().enumerate() {
            let start = Instant::now();
            arena
                .publish(artifact.clone(), Some(core.clone()))
                .expect("the tiny artifact publishes");
            let elapsed = start.elapsed();
            if round == 0 {
                continue;
            }
            match index {
                0 => first.push(elapsed),
                9 => tenth.push(elapsed),
                99 => hundredth.push(elapsed),
                _ => {}
            }
        }
    }
    let first = median(first);
    let tenth = median(tenth);
    let hundredth = median(hundredth);
    let ratio = hundredth.as_secs_f64() / first.as_secs_f64();
    println!("LOOM\tarena_publish\tcount\ttime_us\tratio_to_first");
    println!(
        "LOOM\tarena_publish\t1\t{:.3}\t1.000",
        first.as_secs_f64() * 1e6
    );
    println!(
        "LOOM\tarena_publish\t10\t{:.3}\t{:.3}",
        tenth.as_secs_f64() * 1e6,
        tenth.as_secs_f64() / first.as_secs_f64()
    );
    println!(
        "LOOM\tarena_publish\t100\t{:.3}\t{ratio:.3}",
        hundredth.as_secs_f64() * 1e6
    );
    assert!(
        ratio <= 2.0,
        "the hundredth publication took {ratio:.3} times the first publication"
    );
}

#[test]
#[ignore]
fn bench_late_compilation() {
    use lm_compiler::{compile_module_with_options, CompileEnv, CompileOptions};
    use lm_source::SourceFile;

    let source = SourceFile::new("late-bench.lm", checker_source(256));
    let env = CompileEnv::new().freeze();
    let cases = [
        ("static_compile", CompileOptions::new()),
        ("late_compile", CompileOptions::new().late_definitions()),
    ];
    for (name, options) in cases {
        let mut runs = Vec::with_capacity(ROUNDS);
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let compiled = compile_module_with_options("bench.late", &source, &env, true, &options)
                .expect("the late benchmark compiles");
            let elapsed = start.elapsed();
            std::hint::black_box(compiled.semantic_hash);
            if round > 0 {
                runs.push(elapsed);
            }
        }
        println!(
            "LOOM\t{name}\t256\t{:.3}\tms",
            median(runs).as_secs_f64() * 1e3
        );
    }
}

#[test]
#[ignore]
fn bench_public_syntax() {
    let mut source = String::from("value = 0\n");
    for _ in 1..5000 {
        source.push_str("value = value + 1\n");
    }

    let mut parse_runs = Vec::with_capacity(ROUNDS);
    let mut parsed = None;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let result = lm_source::syntax::parse_public_syntax(&source);
        let elapsed = start.elapsed();
        assert_eq!(result.status, lm_source::syntax::ParseStatus::Complete);
        if round > 0 {
            parse_runs.push(elapsed);
        }
        parsed = Some(result);
    }
    let parsed = parsed.expect("one syntax parse completes");
    let view = lm_abi::syntax::SyntaxView::new(&parsed.records, source.len())
        .expect("the syntax records are valid");

    let part_source = "value = value + 1\n";
    let part = lm_source::syntax::parse_public_syntax(part_source);
    let part_view = lm_abi::syntax::SyntaxView::new(&part.records, part_source.len())
        .expect("the syntax records are valid");
    let part_root = part_view
        .record(part_view.root())
        .expect("the syntax root is valid");
    let statement = part_view.child(part_root, 0).expect("the statement exists");
    let parts: Vec<_> = (0..5000)
        .map(|_| lm_abi::syntax::SyntaxPart {
            source: part_source,
            records: &part.records,
            index: statement,
        })
        .collect();
    let mut construction_runs = Vec::with_capacity(ROUNDS);
    let mut built_count = 0u64;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let built = lm_abi::syntax::build_syntax_node(lm_abi::syntax::KIND_MODULE, &parts)
            .expect("the syntax build completes");
        let elapsed = start.elapsed();
        let built_view = lm_abi::syntax::SyntaxView::new(&built.records, built.source.len())
            .expect("the built syntax records are valid");
        built_count = u64::from(built_view.item_count());
        std::hint::black_box(built);
        if round > 0 {
            construction_runs.push(elapsed);
        }
    }

    let mut traversal_runs = Vec::with_capacity(ROUNDS);
    let mut item_count = 0u64;
    for round in 0..=ROUNDS {
        let start = Instant::now();
        let mut stack = vec![view.root()];
        let mut visited = 0u64;
        while let Some(index) = stack.pop() {
            let record = view.record(index).expect("the syntax item is valid");
            visited += 1;
            for offset in 0..record.child_len {
                stack.push(
                    view.child(record, offset)
                        .expect("the syntax child is valid"),
                );
            }
        }
        let elapsed = start.elapsed();
        std::hint::black_box(visited);
        item_count = visited;
        if round > 0 {
            traversal_runs.push(elapsed);
        }
    }

    let parse = median(parse_runs);
    let construction = median(construction_runs);
    let traversal = median(traversal_runs);
    println!(
        "LOOM\tsyntax_parse\t{}\t{:.1}\t{:.3}",
        item_count,
        parse.as_nanos() as f64 / item_count as f64,
        parse.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tsyntax_construct\t{}\t{:.1}\t{:.3}",
        built_count,
        construction.as_nanos() as f64 / built_count as f64,
        construction.as_secs_f64() * 1e3
    );
    println!(
        "LOOM\tsyntax_traverse\t{}\t{:.1}\t{:.3}",
        item_count,
        traversal.as_nanos() as f64 / item_count as f64,
        traversal.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn bench_typechecking() {
    println!("LOOM\tshape\tn\tlines\tms\tlines_per_s");
    for (name, make, sizes) in shapes() {
        for n in sizes {
            let source = make(n);
            let lines = source.lines().count();
            let mut runs: Vec<Duration> = Vec::new();
            for round in 0..=ROUNDS {
                let start = Instant::now();
                let module = lm_testkit::compile_module_text("bench.lm", &source)
                    .unwrap_or_else(|e| panic!("the generated `{name}` must compile:\n{e}"));
                let elapsed = start.elapsed();
                std::hint::black_box(module.funcs.len());
                if round > 0 {
                    runs.push(elapsed);
                }
            }
            let ms = median(runs).as_secs_f64() * 1e3;
            println!(
                "LOOM\t{name}\t{n}\t{lines}\t{ms:.3}\t{:.0}",
                lines as f64 / (ms / 1e3)
            );
        }
    }
}

// ---------------------------------------------------------------
// Group 3: artifact verification.
// ---------------------------------------------------------------

#[test]
#[ignore]
fn bench_verification() {
    println!("LOOM\tcase\tbytes\tfuncs\tms\tmib_per_s");
    let mut cases: Vec<(String, String)> = vec![(
        "tiny".to_string(),
        "def f(n: Int): Int\n  n + 1\nend\nf(41)\n".to_string(),
    )];
    // Every generated shape at two sizes, so verification meets the
    // same variety the checker does.
    for (name, make, sizes) in shapes() {
        for n in [sizes[0], *sizes.last().expect("a shape has a size")] {
            cases.push((format!("{name}_{n}"), make(n)));
        }
    }
    for (name, source) in cases {
        let module = lm_testkit::compile_module_text("bench.lm", &source).expect("compiles");
        let bytes = lm_bytecode::encode(&module);
        let mut runs: Vec<Duration> = Vec::new();
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let result = lm_verify::verify_module(&module);
            let elapsed = start.elapsed();
            assert!(result.is_ok(), "{name} must verify");
            if round > 0 {
                runs.push(elapsed);
            }
        }
        let ms = median(runs).as_secs_f64() * 1e3;
        let mib = bytes.len() as f64 / (1024.0 * 1024.0);
        println!(
            "LOOM\tverify\t{}\t{}\t{ms:.4}\t{:.1}\t{name}",
            bytes.len(),
            module.funcs.len(),
            mib / (ms / 1e3)
        );
    }

    // The load path as a whole: decode, identity preflight, verify,
    // and the dispatch rows.
    println!("LOOM\tcase\tbytes\tms\tnote");
    for (name, source) in [
        (
            "load_tiny",
            "def f(n: Int): Int\n  n + 1\nend\nf(41)\n".to_string(),
        ),
        ("load_generated_256", checker_source(256)),
    ] {
        let bytes = lm_testkit::compile_to_bytes("bench.lm", &source).expect("compiles");
        let mut runs: Vec<Duration> = Vec::new();
        for round in 0..=ROUNDS {
            let start = Instant::now();
            let (arena, namespace) = lm_testkit::publish_artifact_bytes(&bytes).expect("loads");
            let elapsed = start.elapsed();
            std::hint::black_box(
                arena
                    .namespace(namespace)
                    .expect("the namespace exists")
                    .tables()
                    .funcs
                    .len(),
            );
            if round > 0 {
                runs.push(elapsed);
            }
        }
        println!(
            "LOOM\t{name}\t{}\t{:.4}\tload_bytes",
            bytes.len(),
            median(runs).as_secs_f64() * 1e3
        );
    }
}

// ---------------------------------------------------------------
// Group 5: the filesystem effect.
//
// Every case runs under `CliHost`, the host `lm run` uses. A file
// operation there is asynchronous: the machine performs, the host
// sends the request to a worker thread, the thread makes the call,
// and the machine resumes on a later poll. CPython calls the same
// system calls directly.
//
// So a ratio here reads differently from the language table. It
// measures what the effect boundary costs above the call, not how
// fast the call is. Both sides run a warm page cache: the file is
// written, then a warm-up round reads it before any measured round.
// ---------------------------------------------------------------

/// The scratch directory of one filesystem case.
struct FsTree {
    root: std::path::PathBuf,
}

impl FsTree {
    fn new(label: &str) -> FsTree {
        let root = std::env::temp_dir().join(format!("lm-fs-bench-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("the scratch directory is created");
        FsTree { root }
    }

    /// Write one file of `bytes` filler and return its path as text.
    fn file(&self, name: &str, bytes: usize) -> String {
        let path = self.root.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("the scratch file is written");
        path.display().to_string()
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }
}

impl Drop for FsTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Time one filesystem program under the command-line host.
fn time_fs(source: &str, expected: &str) -> Duration {
    let bytes = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) =
            lm_testkit::publish_artifact_bytes(&bytes).expect("the benchmark artifact must load");
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(arena, namespace, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Time one filesystem program against the in-memory host.
///
/// `RecordingHost` defers a reply to a later poll exactly as
/// `CliHost` does, and it makes no system call and starts no worker
/// thread. The difference between the two hosts is therefore the cost
/// of the call and the thread, and what remains is the effect
/// boundary itself.
fn time_fs_memory(source: &str, file: &str, bytes: usize, expected: &str) -> Duration {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=ROUNDS {
        let (arena, namespace) = lm_testkit::publish_artifact_bytes(&artifact)
            .expect("the benchmark artifact must load");
        let host = Rc::new(RefCell::new(lm_vm::RecordingHost::new(1)));
        host.borrow_mut().set_file(file, vec![b'x'; bytes]);
        let mut world = lm_vm::World::new(arena, namespace, config(), Box::new(host));
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    median(runs)
}

/// Report one case as throughput in mebibytes per second.
fn report_fs_throughput(name: &str, bytes: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        mib / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

/// Drop the page cache for one file. `posix_fadvise` needs no root.
///
/// A benchmark that writes a file and reads it back measures the page
/// cache and not the filesystem. The cold case evicts first, so the
/// device takes part.
fn evict_page_cache(path: &str) {
    let script = format!(
        "import os\nfd = os.open({path:?}, os.O_RDONLY)\n\
         os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)\nos.close(fd)\n"
    );
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .status()
        .expect("the eviction helper runs");
    assert!(status.success(), "the eviction helper failed");
}

/// Report one throughput case that evicts the page cache each round.
fn report_fs_cold(name: &str, bytes: u64, path: &str, source: &str, expected: &str) {
    let artifact = lm_testkit::compile_to_bytes("fs-bench.lm", source)
        .unwrap_or_else(|e| panic!("the benchmark source must compile:\n{e}"));
    let mut runs: Vec<Duration> = Vec::with_capacity(ROUNDS);
    for round in 0..=3 {
        evict_page_cache(path);
        let (arena, namespace) = lm_testkit::publish_artifact_bytes(&artifact)
            .expect("the benchmark artifact must load");
        let host = Box::new(lm_host::CliHost::new(1));
        let mut world = lm_vm::World::new(arena, namespace, config(), host);
        world.allow("Fs").expect("the Fs grant exists");
        let start = Instant::now();
        let outcome = lm_proc::run_world(&mut world);
        let elapsed = start.elapsed();
        assert_eq!(
            world.show_outcome(&outcome),
            expected,
            "the case answered wrong"
        );
        if round > 0 {
            runs.push(elapsed);
        }
    }
    let total = median(runs);
    println!(
        "LOOM\t{name}\t{bytes}\t{:.0}\t{:.3}",
        bytes as f64 / (1024.0 * 1024.0) / total.as_secs_f64(),
        total.as_secs_f64() * 1e3
    );
}

fn report_fs_memory(
    name: &str,
    iterations: u64,
    source: &str,
    file: &str,
    bytes: usize,
    expected: &str,
) {
    let total = time_fs_memory(source, file, bytes, expected);
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        total.as_nanos() as f64 / iterations as f64,
        total.as_secs_f64() * 1e3
    );
}

fn report_fs(name: &str, iterations: u64, source: &str, expected: &str) {
    let total = time_fs(source, expected);
    let per = total.as_nanos() as f64 / iterations as f64;
    println!(
        "LOOM\t{name}\t{iterations}\t{:.0}\t{:.3}",
        per,
        total.as_secs_f64() * 1e3
    );
}

/// The buffered line reader under test.
const READER: &str = r#"# A buffered line reader written in ordinary Loom code.
#
# The buffer is one Bytes value. A line is a slice of it, so a hit
# copies nothing and crosses no effect boundary. Only a refill
# performs `Fs.Read`, and the row says so.

class BufReader
  file: FileHandle
  buffer: Bytes
  eof: Bool

  def init(mut self, file: FileHandle)
    self.file = file
    self.buffer = "".bytes()
    self.eof = false
  end

  def read_line(mut self): Option[Bytes] with Fs.Read
    nl = "\n".bytes()
    out: Option[Bytes] = None
    going = true
    while going
      case self.buffer.find(nl)
      in Some(at)
        line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end
        tail = self.buffer.len() - at - 1
        self.buffer = case self.buffer.slice(at + 1, tail) in Ok(b) then b in Err(_) then self.buffer end
        out = Some(line)
        going = false
      in None
        if self.eof
          if not self.buffer.is_empty()
            out = Some(self.buffer)
            self.buffer = "".bytes()
          end
          going = false
        else
          case self.file.read(65536)
          in Ok(chunk)
            if chunk.is_empty()
              self.eof = true
            else
              self.buffer = self.buffer + chunk
            end
          in Err(_)
            self.eof = true
          end
        end
      end
    end
    out
  end
end

def count_lines(path: String): Int with Fs.Open, Fs.Read, Fs.Close
  case sys.fs.open(path, ReadOnly)
  in Ok(f)
    r = BufReader(f)
    n = 0
    going = true
    while going
      case r.read_line()
      in Some(_)
        n = n + 1
      in None
        going = false
      end
    end
    f.close()
    n
  in Err(_) then -1
  end
end

"#;

#[test]
#[ignore]
fn bench_filesystem_operations() {
    println!("LOOM\tcase\titers\tns_per_op\ttotal_ms");
    let tree = FsTree::new("read");
    let data = tree.file("data.bin", 8 * 1024 * 1024);

    // The handle lifecycle alone: one open and one close.
    report_fs(
        "fs_open_close",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 2000\n\
             \x20   n = n + case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
             \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(2000)",
    );

    // One read of 1 KiB from an open handle. The file is large
    // enough that no read reaches its end.
    report_fs(
        "fs_read_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(2048000)",
    );

    // The same read at 64 KiB. The call does more work and the
    // boundary costs the same, so the ratio moves.
    report_fs(
        "fs_read_64k",
        100,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{data}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 100\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(6553600)",
    );

    // Read one whole small file: open, one read, close. This is the
    // shape a program writes most often.
    let small = tree.file("small.txt", 4096);
    report_fs(
        "fs_read_file",
        1_000,
        &format!(
            "def once(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{small}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = case f.read(8192) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20   f.close()\n    n\n  in Err(_) then 0\n  end\nend\n\
             def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 n = 0\n  i = 0\n  while i < 1000\n    n = n + once()\n    i = i + 1\n  end\n  n\nend\ngo()\n"
        ),
        "Done(4096000)",
    );

    // One write of 1 KiB to an open handle.
    let out = tree.path("out.bin");
    report_fs(
        "fs_write_1k",
        2_000,
        &format!(
            "def go(): Int with Fs.Open, Fs.Write, Fs.Close\n\
             \x20 case sys.fs.open(\"{out}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    chunk = \"{}\".bytes()\n    n = 0\n    i = 0\n    while i < 2000\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n",
            "y".repeat(1024)
        ),
        "Done(2048000)",
    );

    // The same 1 KiB read against the in-memory host. No system call
    // and no worker thread run, so this is the effect boundary alone.
    report_fs_memory(
        "fs_read_1k_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
         \x20 case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 2000\n\
         \x20     n = n + case f.read(1024) in Ok(b) then b.len() in Err(_) then 0 end\n\
         \x20     i = i + 1\n    end\n    f.close()\n    n\n\
         \x20 in Err(_) then 0\n  end\nend\ngo()\n",
        "mem.bin",
        8 * 1024 * 1024,
        "Done(2048000)",
    );

    // A buffered line reader written in ordinary Loom code. The
    // buffer is one Bytes value and a line is a slice of it, so a
    // buffer hit copies nothing and crosses no effect boundary. Only
    // a refill performs `Fs.Read`.
    let lines_path = tree.path("lines.txt");
    {
        let body = b"a short protocol line\n".repeat(200_000);
        std::fs::write(&lines_path, body).expect("the line file is written");
    }
    report_fs(
        "fs_read_lines",
        200_000,
        &format!("{}count_lines(\"{lines_path}\")\n", READER),
        "Done(200000)",
    );

    // The same reader with the line slice removed: one slice for
    // each line instead of two. The difference names what producing
    // one line value costs.
    report_fs(
        "fs_read_lines_advance",
        200_000,
        &format!(
            "{}count_lines(\"{lines_path}\")\n",
            READER.replace(
                "line = case self.buffer.slice(0, at) in Ok(b) then b in Err(_) then self.buffer end",
                "line = self.buffer"
            )
        ),
        "Done(200000)",
    );

    // Sequential throughput over a 64 MiB file, at two chunk sizes.
    // The unit is mebibytes per second, not nanoseconds.
    let big = tree.file("big.bin", 64 * 1024 * 1024);
    println!("LOOM\tcase\tbytes\tmib_per_s\ttotal_ms");
    report_fs_throughput(
        "fs_tput_read_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.read(65536) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    report_fs_throughput(
        "fs_tput_read_1m",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );
    let sink = tree.path("sink.bin");
    report_fs_throughput(
        "fs_tput_write_64k",
        64 * 1024 * 1024,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Write, Fs.Close\n\
             \x20 chunk = case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(src)\n    c = case src.read(65536) in Ok(b) then b in Err(_) then \"\".bytes() end\n\
             \x20   src.close()\n    c\n  in Err(_) then \"\".bytes()\n  end\n\
             \x20 case sys.fs.open(\"{sink}\", CreateTruncate)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 1024\n\
             \x20     n = n + case f.write(chunk) in Ok(w) then w in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The same read with the page cache evicted first. This is the
    // cost of loading a file, and the warm case above is the cost of
    // copying one out of memory.
    report_fs_cold(
        "fs_tput_read_cold",
        64 * 1024 * 1024,
        &big,
        &format!(
            "def go(): Int with Fs.Open, Fs.Read, Fs.Close\n\
             \x20 case sys.fs.open(\"{big}\", ReadOnly)\n\
             \x20 in Ok(f)\n    n = 0\n    i = 0\n    while i < 64\n\
             \x20     n = n + case f.read(1048576) in Ok(b) then b.len() in Err(_) then 0 end\n\
             \x20     i = i + 1\n    end\n    f.close()\n    n\n\
             \x20 in Err(_) then 0\n  end\nend\ngo()\n"
        ),
        "Done(67108864)",
    );

    // The handle lifecycle against the in-memory host.
    report_fs_memory(
        "fs_open_close_memory",
        2_000,
        "def go(): Int with Fs.Open, Fs.Close\n\
         \x20 n = 0\n  i = 0\n  while i < 2000\n\
         \x20   n = n + case sys.fs.open(\"mem.bin\", ReadOnly)\n\
         \x20   in Ok(f)\n      case f.close() in Ok(_) then 1 in Err(_) then 0 end\n\
         \x20   in Err(_) then 0\n    end\n    i = i + 1\n  end\n  n\nend\ngo()\n",
        "mem.bin",
        4096,
        "Done(2000)",
    );
}
