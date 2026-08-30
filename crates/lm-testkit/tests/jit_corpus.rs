//! Forced-native differential coverage for shipped standalone programs.

use lm_vm::{Engine, EngineMode, Outcome, Vm, VmConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn collect_programs(path: &Path, programs: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(path).expect("the corpus directory reads") {
        let path = entry.expect("the corpus entry reads").path();
        if path.is_dir() {
            collect_programs(&path, programs);
        } else if path.extension().is_some_and(|extension| extension == "lm") {
            programs.push(path);
        }
    }
}

fn run(artifact: lm_bytecode::artifact::Artifact, mode: EngineMode) -> (Outcome, String) {
    let (arena, namespace) =
        lm_testkit::publish_compiled_artifact(artifact).expect("the corpus artifact publishes");
    let engine = Arc::new(Engine::new(mode));
    let mut vm = Vm::new_with_engine(arena, namespace, VmConfig::default(), engine);
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    (outcome, dump)
}

#[test]
fn forced_native_matches_the_standalone_program_corpus() {
    let root = lm_testkit::repo_root();
    let mut programs = Vec::new();
    collect_programs(&root.join("tests/run-pass"), &mut programs);
    collect_programs(&root.join("tests/run-fault"), &mut programs);
    collect_programs(&root.join("examples"), &mut programs);
    programs.retain(|path| {
        let name = path.strip_prefix(&root).unwrap_or(path).to_string_lossy();
        !name.starts_with("examples/05-modules/") && !name.starts_with("examples/16-text-editor/")
    });
    programs.sort();
    assert!(programs.len() >= 160, "the JIT corpus lost programs");

    let worker_count = 4.min(programs.len());
    let mut failures = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..worker_count)
            .map(|offset| {
                let programs = &programs;
                let root = &root;
                scope.spawn(move || {
                    let mut failures = Vec::new();
                    for path in programs.iter().skip(offset).step_by(worker_count) {
                        let name = path
                            .strip_prefix(root)
                            .unwrap_or(path)
                            .to_string_lossy()
                            .replace('\\', "/");
                        let source =
                            std::fs::read_to_string(path).expect("the corpus program reads");
                        let artifact = lm_testkit::compile_text(&name, &source)
                            .unwrap_or_else(|error| panic!("{name} does not compile: {error}"));
                        let interpreted = run(artifact.clone(), EngineMode::Interpreter);
                        let native = run(artifact, EngineMode::Native);
                        if native != interpreted {
                            failures.push(format!(
                                "{name}: interpreter {interpreted:?}, native {native:?}"
                            ));
                        }
                    }
                    failures
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("the JIT corpus worker completes"))
            .collect::<Vec<_>>()
    });
    failures.sort();

    assert!(
        failures.is_empty(),
        "{} JIT corpus programs differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
