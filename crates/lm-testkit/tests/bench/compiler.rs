use super::*;

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
