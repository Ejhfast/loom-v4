use super::*;

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
