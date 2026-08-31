//! Forced-native differential coverage for shipped standalone programs.

use lm_vm::{Engine, EngineMode, Outcome, RecordingHost, Vm, VmConfig, World};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

const GRANTS: &[&str] = &[
    "Args", "Choose", "Clock", "Compiler", "Dns", "Entropy", "Env", "Exec", "Fs", "Io", "Pipe",
    "Proc", "Rand", "Reflect", "Signal", "Tcp", "Tls", "Tty", "Udp", "Vm", "Wait",
];

type ObservedRun = (Outcome, String, Vec<u8>, Vec<u8>, Vec<u32>, u64);

fn rejection_counts(metrics: &lm_vm::EngineMetrics) -> String {
    format!(
        "source={}, value={}, instruction={}, stack={}, control={}, limit={}",
        metrics.unsupported_missing_source,
        metrics.unsupported_value_representation,
        metrics.unsupported_instruction,
        metrics.unsupported_stack_analysis,
        metrics.unsupported_control_flow,
        metrics.unsupported_region_limit,
    )
}

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
) -> ((Outcome, String), lm_vm::EngineMetrics) {
    let mut vm = Vm::new_with_engine(arena, namespace, VmConfig::default(), Arc::clone(&engine));
    let outcome = vm.run();
    let dump = vm.dump_live(&outcome);
    ((outcome, dump), engine.metrics())
}

fn run_scheduled(
    arena: lm_link::CodeArena,
    namespace: lm_link::NamespaceId,
    engine: Arc<Engine>,
) -> (ObservedRun, lm_vm::EngineMetrics) {
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
    (observed, engine.metrics())
}

fn compile_function(
    compiler: &lm_jit::JitEngine,
    code: &lm_link::CodeNamespace,
    function: u32,
) -> Result<(), String> {
    let runtime = code
        .tables()
        .funcs
        .get(function as usize)
        .ok_or_else(|| "the runtime function is absent".to_string())?;
    let (unit, local) = code
        .function_unit(function)
        .map_err(|error| error.to_string())?;
    let relocation = code
        .relocation(unit.id())
        .ok_or_else(|| "the source unit relocation is absent".to_string())?;
    let mut input =
        lm_jit::FunctionInput::new(function, runtime, unit.module(), code.bundle(), local);
    input.set_runtime_string_count(code.tables().strings.len());
    input.set_runtime_core_roles(code.core_roles());
    input.set_class_relocation(relocation.classes());

    let mut callees = Vec::new();
    for instruction in runtime.blocks.iter().flatten() {
        if let lm_bytecode::Instr::Call(callee) | lm_bytecode::Instr::CallG { func: callee, .. } =
            instruction
        {
            if !callees.contains(callee) {
                callees.push(*callee);
            }
        }
    }
    for callee in callees {
        let callee_runtime = code
            .tables()
            .funcs
            .get(callee as usize)
            .ok_or_else(|| "the direct callee is absent".to_string())?;
        let (callee_unit, callee_local) = code
            .function_unit(callee)
            .map_err(|error| error.to_string())?;
        let callee_relocation = code
            .relocation(callee_unit.id())
            .ok_or_else(|| "the direct callee relocation is absent".to_string())?;
        input.add_relocated_direct_callee(
            callee,
            callee_runtime,
            callee_unit.module(),
            code.bundle(),
            callee_local,
            callee_relocation.classes(),
        );
    }

    compiler
        .compile(input)
        .map(|_| ())
        .map_err(|failure| format!("{failure:?}"))
}

fn compile_unseen_functions(
    compiler: &lm_jit::JitEngine,
    code: &lm_link::CodeNamespace,
    seen: &Mutex<HashSet<(lm_bytecode::artifact::ArtifactId, u32)>>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for function in 0..code.tables().funcs.len() {
        let function = function as u32;
        let (unit, local) = match code.function_unit(function) {
            Ok(parts) => parts,
            Err(error) => {
                failures.push(format!("function {function}: {error}"));
                continue;
            }
        };
        let key = (unit.id(), local);
        let first = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key);
        if !first {
            continue;
        }
        if let Err(error) = compile_function(compiler, code, function) {
            let name = code
                .tables()
                .funcs
                .get(function as usize)
                .map_or("<missing>", |definition| definition.name.as_str());
            failures.push(format!("function {function} ({name}): {error}"));
        }
    }
    failures
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
    if let Some(filter) = std::env::var_os("LOOM_JIT_CORPUS_FILTER") {
        let filter = filter.to_string_lossy();
        programs.retain(|path| path.to_string_lossy().contains(filter.as_ref()));
        assert!(
            !programs.is_empty(),
            "the JIT corpus filter matches a program"
        );
    } else {
        assert!(programs.len() >= 160, "the JIT corpus lost programs");
    }

    let seen_functions = Mutex::new(HashSet::new());
    let worker_count = 4.min(programs.len());
    let mut failures = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..worker_count)
            .map(|offset| {
                let programs = &programs;
                let root = &root;
                let seen_functions = &seen_functions;
                scope.spawn(move || {
                    let compiler = lm_jit::JitEngine::default();
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
                        let code = arena
                            .namespace(namespace)
                            .expect("the corpus namespace exists");
                        failures.extend(
                            compile_unseen_functions(&compiler, code, seen_functions)
                                .into_iter()
                                .map(|failure| format!("{name}: {failure}")),
                        );
                        let (interpreted, _) = run_direct(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Interpreter)),
                        );
                        let (native, direct_metrics) = run_direct(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Native)),
                        );
                        if native != interpreted {
                            failures.push(format!(
                                "{name}: direct interpreter {interpreted:?}, native {native:?}"
                            ));
                        }
                        if direct_metrics.unsupported_region_fallbacks != 0 {
                            failures.push(format!(
                                "{name}: direct native used {} unsupported fallbacks ({})",
                                direct_metrics.unsupported_region_fallbacks,
                                rejection_counts(&direct_metrics)
                            ));
                        }
                        let (interpreted, _) = run_scheduled(
                            arena.clone(),
                            namespace,
                            Arc::new(Engine::new(EngineMode::Interpreter)),
                        );
                        let (native, native_metrics) = run_scheduled(
                            arena,
                            namespace,
                            Arc::new(Engine::new(EngineMode::Native)),
                        );
                        if native != interpreted {
                            failures.push(format!(
                                "{name}: scheduler interpreter {interpreted:?}, native {native:?}, metrics {native_metrics:?}"
                            ));
                        }
                        if native_metrics.unsupported_region_fallbacks != 0 {
                            failures.push(format!(
                                "{name}: scheduler native used {} unsupported fallbacks ({})",
                                native_metrics.unsupported_region_fallbacks,
                                rejection_counts(&native_metrics)
                            ));
                        }
                        if name == "tests/run-fault/option-expect.lm"
                            && native_metrics.native_retired_instructions == 0
                        {
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
