use super::*;

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
