//! Test harness for UI tests and run tests.
//!
//! A UI case is one `.lm` file with an `.expected` file that holds the
//! exact diagnostic text. A run case is one `.lm` file with an
//! `.expected` file that holds the exact outcome line, for example
//! `Done(4950)`.

pub mod oracle;

use lm_source::SourceFile;
use lm_vm::{RecordingHost, Vm, VmConfig, World};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Compile source text to one artifact. `name` appears in diagnostics.
/// `Err` holds the fully rendered diagnostic.
pub fn compile_text(name: &str, text: &str) -> Result<lm_bytecode::artifact::Artifact, String> {
    let source = SourceFile::new(name, text);
    lm_compiler::compile_source("", &source, true).map(|compiled| compiled.artifact)
}

/// Compile source text and return its root module for compiler tests.
pub fn compile_module_text(name: &str, text: &str) -> Result<lm_bytecode::Module, String> {
    let source = SourceFile::new(name, text);
    lm_compiler::compile_source("", &source, true).map(|compiled| compiled.root.module)
}

/// Compile source text and assemble one verifier mutation fixture.
///
/// This helper joins the published tables with their namespace data.
/// A VM never executes the returned `Module`.
pub fn compile_verifier_fixture_text(
    name: &str,
    text: &str,
) -> Result<lm_bytecode::Module, String> {
    let artifact = compile_text(name, text)?;
    let (arena, namespace) = publish_artifact(&artifact)?;
    let namespace = arena
        .namespace(namespace)
        .ok_or_else(|| "the published namespace is missing".to_string())?;
    let tables = namespace.tables();
    let mut slots = tables.slots.clone();
    for (slot, initial) in slots.iter_mut().zip(namespace.slot_initials()) {
        slot.initial = *initial;
    }
    Ok(lm_bytecode::Module {
        strings: tables.strings.clone(),
        bytes: tables.bytes.clone(),
        types: tables.types.clone(),
        selectors: tables.selectors.clone(),
        apps: tables.apps.clone(),
        interfaces: tables.interfaces.clone(),
        conformances: tables.conformances.clone(),
        class_bounds: tables.class_bounds.clone(),
        func_bounds: tables.func_bounds.clone(),
        imports: Vec::new(),
        slots,
        core_roles: *namespace.core_roles(),
        classes: tables.classes.clone(),
        funcs: tables.funcs.clone(),
        entry: namespace.entry(),
        exports: namespace.exports().to_vec(),
        bindings: namespace.bindings().to_vec(),
        debug: tables.debug.clone(),
    })
}

/// Compile source text to serialized artifact bytes.
pub fn compile_to_bytes(name: &str, text: &str) -> Result<Vec<u8>, String> {
    lm_bytecode::artifact::encode(&compile_text(name, text)?)
        .map_err(|error| format!("artifact encode error: {error}"))
}

/// Decode and publish one artifact against the exact runtime core.
pub fn publish_artifact_bytes(
    bytes: &[u8],
) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let artifact = lm_bytecode::artifact::decode(bytes)
        .map_err(|error| format!("artifact decode error: {error}"))?;
    publish_artifact(&artifact)
}

/// Load snapshot bytes with the exact runtime core of artifact bytes.
pub fn load_snapshot_for_artifact_bytes(
    artifact_bytes: &[u8],
    snapshot_bytes: &[u8],
    limits: lm_vm::snapshot::LoadLimits,
) -> Result<lm_vm::snapshot::SnapshotImage, lm_vm::snapshot::ImageError> {
    let artifact = lm_bytecode::artifact::decode(artifact_bytes).map_err(|error| {
        lm_vm::snapshot::ImageError::admission(
            lm_vm::snapshot::ImageReason::Code,
            format!("artifact decode error: {error}"),
        )
    })?;
    load_snapshot_for_artifact(&artifact, snapshot_bytes, limits)
}

/// Load snapshot bytes with the exact runtime core of one artifact.
pub fn load_snapshot_for_artifact(
    artifact: &lm_bytecode::artifact::Artifact,
    snapshot_bytes: &[u8],
    limits: lm_vm::snapshot::LoadLimits,
) -> Result<lm_vm::snapshot::SnapshotImage, lm_vm::snapshot::ImageError> {
    let (arena, namespace) = publish_artifact(artifact).map_err(|message| {
        lm_vm::snapshot::ImageError::admission(lm_vm::snapshot::ImageReason::Code, message)
    })?;
    let available = arena
        .namespace(namespace)
        .cloned()
        .expect("the published namespace exists");
    lm_vm::snapshot::codec::load_external(snapshot_bytes, Some(available), limits)
}

/// Verify and publish one artifact against the exact runtime core.
pub fn publish_artifact(
    artifact: &lm_bytecode::artifact::Artifact,
) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let core = lm_compiler::core_link_unit()?;
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact.clone(), Some(core))
        .map_err(|error| format!("artifact publish error: {error}"))?;
    Ok((arena, namespace))
}

/// Verify and publish one crafted module as a test-only core unit.
pub fn unit_from_module(
    module: lm_bytecode::Module,
) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let artifact = artifact_from_module(module)?;
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact, None)
        .map_err(|error| error.to_string())?;
    Ok((arena, namespace))
}

/// Wrap one crafted module in a test-only artifact.
pub fn artifact_from_module(
    module: lm_bytecode::Module,
) -> Result<lm_bytecode::artifact::Artifact, String> {
    let unit = lm_bytecode::artifact::LinkUnit::from_module(
        lm_bytecode::artifact::CORE_MODULE_PATH,
        module,
        Vec::new(),
    )
    .map_err(|error| error.to_string())?;
    let artifact = lm_bytecode::artifact::Artifact::new(unit, Vec::new())
        .map_err(|error| error.to_string())?;
    Ok(artifact)
}

/// Replace the root payload of one artifact for a bytecode test.
pub fn replace_artifact_root(
    artifact: &lm_bytecode::artifact::Artifact,
    module: lm_bytecode::Module,
) -> Result<lm_bytecode::artifact::Artifact, String> {
    let old = artifact.root();
    let root = lm_bytecode::artifact::LinkUnit::from_module(
        old.module_path(),
        module,
        old.dependencies().to_vec(),
    )
    .map_err(|error| error.to_string())?;
    let embedded = artifact
        .units()
        .iter()
        .filter(|unit| unit.id() != old.id())
        .cloned()
        .collect();
    lm_bytecode::artifact::Artifact::new(root, embedded).map_err(|error| error.to_string())
}

/// Build one artifact from a compiler result.
pub fn artifact_from_compiled(
    compiled: lm_compiler::CompiledModule,
) -> Result<lm_bytecode::artifact::Artifact, String> {
    let root = compiled.path.clone();
    let mut env = lm_compiler::core_link_env()?;
    bind_compiled_unit(&mut env, compiled)?;
    env.freeze()
        .complete_artifact(&root)
        .map_err(|error| error.to_string())
}

/// Encode one artifact from a compiler result.
pub fn encode_compiled_artifact(compiled: lm_compiler::CompiledModule) -> Result<Vec<u8>, String> {
    lm_bytecode::artifact::encode(&artifact_from_compiled(compiled)?)
        .map_err(|error| error.to_string())
}

/// Build a test artifact from one crafted source module.
pub fn artifact_with_core_from_module(
    path: &str,
    module: lm_bytecode::Module,
) -> Result<lm_bytecode::artifact::Artifact, String> {
    let identity =
        lm_bytecode::identity::module_identity(&module).map_err(|error| error.to_string())?;
    let interface = lm_bytecode::interface::derive_interface(&module, &identity, path)?;
    let mut env = lm_compiler::core_link_env()?;
    let unit = env
        .prepare_unit(path, module, interface)
        .map_err(|error| error.to_string())?;
    env.bind_unit(unit).map_err(|error| error.to_string())?;
    env.freeze()
        .complete_artifact(path)
        .map_err(|error| error.to_string())
}

/// Encode a test artifact from one crafted source module.
pub fn encode_artifact_with_core_from_module(
    path: &str,
    module: lm_bytecode::Module,
) -> Result<Vec<u8>, String> {
    lm_bytecode::artifact::encode(&artifact_with_core_from_module(path, module)?)
        .map_err(|error| error.to_string())
}

/// Convert one compiler result to a link unit and bind it.
pub fn bind_compiled_unit(
    env: &mut lm_link::LinkEnv,
    compiled: lm_compiler::CompiledModule,
) -> Result<(), String> {
    let unit = compiled
        .into_link_unit(env)
        .map_err(|error| error.to_string())?;
    env.bind_unit(unit).map_err(|error| error.to_string())
}

/// Compile, serialize, decode, verify, and run one program. Return the
/// stable outcome text, for example `Done(42)` or `Fault(OutOfFuel)`.
pub fn run_text(name: &str, text: &str, config: VmConfig) -> Result<String, String> {
    let bytes = compile_to_bytes(name, text)?;
    let (arena, namespace) = publish_artifact_bytes(&bytes)?;
    let mut vm = Vm::new(arena, namespace, config);
    let outcome = vm.run();
    Ok(vm.show_outcome(&outcome))
}

/// Compile and run one program in a world with root grants and the
/// deterministic recording host. Return the outcome text and the
/// shared host for inspection.
pub fn run_world(
    name: &str,
    text: &str,
    allow: &[&str],
    config: VmConfig,
) -> Result<(String, Rc<RefCell<RecordingHost>>), String> {
    let bytes = compile_to_bytes(name, text)?;
    let (arena, namespace) = publish_artifact_bytes(&bytes)?;
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(arena, namespace, config, Box::new(host.clone()));
    for grant in allow {
        world
            .allow(grant)
            .map_err(|e| format!("allow error: {e}"))?;
    }
    let outcome = lm_proc::run_world(&mut world);
    Ok((world.show_outcome(&outcome), host))
}

/// `run_world` with the default limits, returning the outcome text.
pub fn run_allowed(name: &str, text: &str, allow: &[&str]) -> Result<String, String> {
    run_world(name, text, allow, VmConfig::default()).map(|(out, _)| out)
}

/// The repository root, two levels above this crate.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root exists")
}

/// The diagnostic-facing name for a file: its path relative to the
/// repository root, with `/` separators.
fn display_name(path: &Path) -> String {
    let root = repo_root();
    let rel = path.strip_prefix(&root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn read_case(path: &Path) -> (String, String) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let expected_path = path.with_extension("expected");
    let expected = std::fs::read_to_string(&expected_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", expected_path.display()));
    (text, expected)
}

/// Run one UI case: compilation must fail with the expected diagnostic.
pub fn ui_case(path: &Path) -> Result<(), String> {
    let (text, expected) = read_case(path);
    let name = display_name(path);
    match compile_text(&name, &text) {
        Ok(_) => Err(format!("{name}: expected a diagnostic, compilation passed")),
        Err(rendered) => {
            if rendered == expected {
                Ok(())
            } else {
                Err(format!(
                    "{name}: diagnostic mismatch\n--- expected ---\n{expected}\
                     --- found ---\n{rendered}"
                ))
            }
        }
    }
}

/// Run one run case: execution must produce the expected outcome line.
pub fn run_case(path: &Path, config: VmConfig) -> Result<(), String> {
    let (text, expected) = read_case(path);
    let name = display_name(path);
    let found = run_text(&name, &text, config).map_err(|e| format!("{name}: {e}"))?;
    let expected = expected.trim_end();
    if found == expected {
        Ok(())
    } else {
        Err(format!(
            "{name}: outcome mismatch: expected `{expected}`, found `{found}`"
        ))
    }
}

/// Collect the `.lm` files in a directory in stable name order.
pub fn lm_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("directory entry").path())
        .filter(|p| p.extension().map(|e| e == "lm").unwrap_or(false))
        .collect();
    files.sort();
    files
}

/// Run every case in a directory with one case function. Panic with a
/// combined report when any case fails.
pub fn run_suite(dir: &Path, case: impl Fn(&Path) -> Result<(), String>) {
    let files = lm_files(dir);
    assert!(!files.is_empty(), "no .lm cases in {}", dir.display());
    let failures: Vec<String> = files.iter().filter_map(|path| case(path).err()).collect();
    if !failures.is_empty() {
        panic!(
            "{} case(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}
