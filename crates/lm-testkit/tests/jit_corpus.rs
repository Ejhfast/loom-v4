//! Forced-native differential coverage for shipped standalone programs.

use lm_vm::{Engine, EngineMode, Outcome, RecordingHost, Vm, VmConfig, World};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

const GRANTS: &[&str] = &[
    "Args", "Choose", "Clock", "Compiler", "Dns", "Entropy", "Env", "Exec", "Fs", "Io", "Pipe",
    "Proc", "Rand", "Reflect", "Signal", "Tcp", "Tls", "Tty", "Udp", "Vm", "Wait",
];

type ObservedRun = (Outcome, String, Vec<u8>, Vec<u8>, Vec<u32>, u64);

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

fn run_direct(
    arena: lm_link::CodeArena,
    namespace: lm_link::NamespaceId,
    engine: Arc<Engine>,
) -> (Outcome, String) {
    let mut vm = Vm::new_with_engine(arena, namespace, VmConfig::default(), engine);
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    (outcome, dump)
}

fn run_scheduled(
    arena: lm_link::CodeArena,
    namespace: lm_link::NamespaceId,
    engine: Arc<Engine>,
) -> (ObservedRun, u64) {
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new_with_engine(
        arena,
        namespace,
        VmConfig::default(),
        Box::new(Rc::clone(&host)),
        Arc::clone(&engine),
    );
    for grant in GRANTS {
        world.allow(grant).expect("the corpus grant exists");
    }
    let outcome = lm_proc::run_world(&mut world);
    let dump = world.dump_live(&outcome);
    let retired = world.metrics().retired_instructions;
    let host = host.borrow();
    let observed = (
        outcome,
        dump,
        host.written_bytes.clone(),
        host.written_error_bytes.clone(),
        host.operations.clone(),
        retired,
    );
    (observed, engine.metrics().native_retired_instructions)
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
                        let (arena, namespace) = lm_testkit::publish_compiled_artifact(artifact)
                            .expect("the corpus artifact publishes");
                        let interpreted = run_direct(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Interpreter)),
                        );
                        let native = run_direct(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Native)),
                        );
                        if native != interpreted {
                            failures.push(format!(
                                "{name}: direct interpreter {interpreted:?}, native {native:?}"
                            ));
                        }
                        let (interpreted, _) = run_scheduled(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Interpreter)),
                        );
                        let (native, native_retired) = run_scheduled(
                            arena,
                            namespace,
                            Arc::new(Engine::new(EngineMode::Native)),
                        );
                        if native != interpreted {
                            failures.push(format!(
                                "{name}: scheduler interpreter {interpreted:?}, native {native:?}"
                            ));
                        }
                        if name == "tests/run-fault/option-expect.lm" && native_retired == 0 {
                            failures.push(format!(
                                "{name}: the scheduler did not execute native instructions"
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
