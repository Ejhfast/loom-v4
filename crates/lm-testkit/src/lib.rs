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

/// Compile source text to decoded bytecode. `name` appears in
/// diagnostics. `Err` holds the fully rendered diagnostic.
pub fn compile_text(name: &str, text: &str) -> Result<lm_bytecode::Module, String> {
    let source = SourceFile::new(name, text);
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(&source))?;
    let hir = lm_hir::check_module(&ast).map_err(|d| d.render(&source))?;
    Ok(lm_hir::lower_module(&hir))
}

/// Compile source text to serialized bytecode bytes.
pub fn compile_to_bytes(name: &str, text: &str) -> Result<Vec<u8>, String> {
    Ok(lm_bytecode::encode(&compile_text(name, text)?))
}

/// Compile, serialize, decode, verify, and run one program. Return the
/// stable outcome text, for example `Done(42)` or `Fault(OutOfFuel)`.
pub fn run_text(name: &str, text: &str, config: VmConfig) -> Result<String, String> {
    let bytes = compile_to_bytes(name, text)?;
    let loaded = lm_vm::load_bytes(&bytes).map_err(|e| format!("load error: {e}"))?;
    let mut vm = Vm::new(&loaded, config);
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
    let loaded = lm_vm::load_bytes(&bytes).map_err(|e| format!("load error: {e}"))?;
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(&loaded, config, Box::new(host.clone()));
    for grant in allow {
        world
            .allow(grant)
            .map_err(|e| format!("allow error: {e}"))?;
    }
    let outcome = world.run_root();
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
