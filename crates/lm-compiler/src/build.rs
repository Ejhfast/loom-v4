//! The build loop: compile every module of the dependency graph and
//! link the program.
//!
//! The loop walks the packages in dependency order and the modules of
//! each package in import order. Every module compiles against the
//! interfaces the graph already produced, never against a source file
//! of another package. A cached module skips the compiler and keeps
//! its recorded artifact.

use crate::cache::{
    compile_key, interface_identity, program_key, write_atomic, BuildDir, ProgramEntry,
};
use crate::env::{CompileEnv, LinkEnv};
use crate::graph::{load_workspace, module_order, Workspace};
use crate::link::link;
use crate::module::compile_module;
use lm_source::SourceFile;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The result for one module.
#[derive(Debug, Clone)]
pub struct ModuleReport {
    pub package: String,
    pub path: String,
    /// True when the build directory answered instead of the compiler.
    pub cached: bool,
    pub semantic_hash: [u8; 32],
    pub interface_id: [u8; 32],
}

/// The result of one build.
#[derive(Debug, Clone)]
pub struct BuildReport {
    pub root: String,
    pub modules: Vec<ModuleReport>,
    /// The linked program artifact, when the root package has a
    /// `src/main.lm`.
    pub program: Option<PathBuf>,
    pub program_semantic: Option<[u8; 32]>,
    pub program_container: Option<[u8; 32]>,
    /// True when the stage-2 cache answered instead of the linker.
    /// A hit skips the link run and the verification of the merged
    /// program.
    pub program_cached: bool,
}

impl BuildReport {
    /// The number of modules the compiler ran on.
    pub fn compiled(&self) -> usize {
        self.modules.iter().filter(|m| !m.cached).count()
    }
}

/// Build one package and every package it needs.
///
/// `start` is any path inside the package. `build_root` is the build
/// directory, normally `build` beside the current directory.
pub fn build_package(start: &Path, build_root: &Path) -> Result<BuildReport, String> {
    let workspace = load_workspace(start)?;
    let dir = BuildDir::new(build_root);
    let mut env = CompileEnv::new();
    let mut interfaces: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    let mut link_env = LinkEnv::new();
    let mut modules: Vec<ModuleReport> = Vec::new();
    // The stage-2 key inputs, in link order.
    let mut contents: Vec<(String, [u8; 32])> = Vec::new();
    let standard_uses: Vec<Vec<String>> = workspace
        .order
        .iter()
        .flat_map(|name| workspace.package(name).modules.iter())
        .flat_map(|module| module.uses.iter().cloned())
        .collect();
    let standard = crate::standard::modules_for_uses(&standard_uses);
    for module in &standard {
        let interface_id = interface_identity(&module.interface);
        interfaces.insert(module.path.clone(), interface_id);
        env.bind_interface(module.interface.clone())
            .map_err(|error| format!("error: {error}\n"))?;
        contents.push((module.path.clone(), module.container_hash));
        link_env
            .bind_module(
                module.path.clone(),
                module.module.clone(),
                module.interface.clone(),
            )
            .map_err(|error| format!("error: {error}\n"))?;
    }
    for package_name in &workspace.order {
        let package = workspace.package(package_name);
        // The root set of every module of this package: the
        // dependency keys and this package's own top-level modules.
        let mut roots: Vec<(String, String)> = Vec::new();
        for (key, provided) in &package.deps {
            roots.push((key.clone(), provided.clone()));
        }
        for name in package.top_level() {
            roots.push((name.clone(), format!("{package_name}.{name}")));
        }
        for idx in module_order(package)? {
            let module = &package.modules[idx];
            let text = std::fs::read_to_string(&module.file)
                .map_err(|e| format!("error: cannot read `{}`: {e}\n", module.file.display()))?;
            let visible: Vec<(String, [u8; 32])> =
                interfaces.iter().map(|(k, v)| (k.clone(), *v)).collect();
            let uses_standard = module
                .uses
                .iter()
                .any(|path| path.first().map(String::as_str) == Some("std"));
            let mut compile_roots = roots.clone();
            if uses_standard {
                compile_roots.push(("std".to_string(), "std".to_string()));
            }
            let key = compile_key(
                &module.path,
                module.is_main,
                &text,
                &compile_roots,
                &visible,
            );
            let mut compiled = None;
            let mut cached = false;
            if let Some((artifact, interface_bytes)) = dir.read(&key) {
                // A cached entry still decodes through the ordinary
                // decoder, so a damaged file is a miss, not a trust
                // hole.
                if let (Ok(decoded), Ok(interface)) = (
                    lm_bytecode::decode(&artifact),
                    lm_bytecode::interface::decode_interface(&interface_bytes),
                ) {
                    cached = true;
                    compiled = Some(crate::module::CompiledModule {
                        path: module.path.clone(),
                        semantic_hash: interface.semantic_hash,
                        container_hash: lm_bytecode::identity::container_hash(&artifact),
                        module: decoded,
                        artifact,
                        interface,
                        interface_bytes,
                    });
                }
            }
            let compiled = match compiled {
                Some(entry) => entry,
                None => {
                    let mut local = env.clone();
                    if uses_standard {
                        local.bind_standard_root();
                    }
                    for (name, prefix) in &roots {
                        local
                            .bind_root(name, prefix)
                            .map_err(|e| format!("error: {e}\n"))?;
                    }
                    let source = SourceFile::new(display_name(&module.file), text.clone());
                    let entry =
                        compile_module(&module.path, &source, &local.freeze(), module.is_main)?;
                    dir.write(&key, &entry.artifact, &entry.interface_bytes)?;
                    entry
                }
            };
            let interface_id = interface_identity(&compiled.interface);
            interfaces.insert(module.path.clone(), interface_id);
            env.bind_interface(compiled.interface.clone())
                .map_err(|e| format!("error: {e}\n"))?;
            modules.push(ModuleReport {
                package: package_name.clone(),
                path: module.path.clone(),
                cached,
                semantic_hash: compiled.semantic_hash,
                interface_id,
            });
            contents.push((compiled.path.clone(), compiled.container_hash));
            // The unit takes the compiled module, so a build moves
            // one module and never copies it.
            link_env
                .bind_module(compiled.path, compiled.module, compiled.interface)
                .map_err(|error| format!("error: {error}\n"))?;
        }
    }
    let root_package = workspace.package(&workspace.root);
    let mut report = BuildReport {
        root: workspace.root.clone(),
        modules,
        program: None,
        program_semantic: None,
        program_container: None,
        program_cached: false,
    };
    if !root_package.has_main() {
        return Ok(report);
    }
    let main_path = format!("{}.main", workspace.root);
    // Stage 2: the module set fixes the merged program, so an
    // unchanged module set skips the link run. A damaged entry is a
    // miss.
    //
    // A hit does not skip the verification of the merged program. The
    // entry is a file, so a writer of the build directory decides what
    // this build emits. The verifier therefore runs on every hit, and
    // the two reported hashes come from the bytes, never from the
    // entry. A failing entry falls through to a fresh link, which
    // overwrites it, so a damaged or planted entry costs one link and
    // never emits an unverified program.
    let key = program_key(&main_path, &contents);
    let checked = dir
        .read_program(&key)
        .and_then(|entry| verified_program_entry(&entry.artifact));
    let entry = match checked {
        Some(entry) => {
            report.program_cached = true;
            entry
        }
        None => {
            let program =
                link(&main_path, &link_env.freeze()).map_err(|e| format!("error: {e}\n"))?;
            let entry = ProgramEntry {
                artifact: program.artifact,
                semantic_hash: program.semantic_hash,
                container_hash: program.container_hash,
            };
            dir.write_program(&key, &entry)?;
            entry
        }
    };
    let debug = dir.debug();
    std::fs::create_dir_all(&debug)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", debug.display()))?;
    let path = debug.join(format!("{}.lma", workspace.root));
    write_atomic(&path, &entry.artifact)?;
    report.program = Some(path);
    report.program_semantic = Some(entry.semantic_hash);
    report.program_container = Some(entry.container_hash);
    Ok(report)
}

/// The path a diagnostic names: the file path as the user wrote it.
fn display_name(path: &Path) -> String {
    path.display().to_string()
}

/// Load one workspace without building it, for `lm inspect` and the
/// tests.
pub fn workspace_of(start: &Path) -> Result<Workspace, String> {
    load_workspace(start)
}

/// Check one stage-2 entry and rebuild its two hashes from its bytes.
///
/// A stage-2 entry is a file in the build directory, so it carries no
/// trust. The check decodes the artifact, proves it holds no import
/// slot, runs the whole verifier over it, and computes the two hashes
/// from the bytes. `None` marks an entry the build must not emit, and
/// the caller then links again.
fn verified_program_entry(artifact: &[u8]) -> Option<ProgramEntry> {
    let module = lm_bytecode::decode(artifact).ok()?;
    if !module.imports.is_empty() {
        return None;
    }
    lm_verify::verify_module(&module).ok()?;
    let identity = lm_bytecode::identity::module_identity(&module).ok()?;
    Some(ProgramEntry {
        artifact: artifact.to_vec(),
        semantic_hash: identity.semantic_hash,
        container_hash: lm_bytecode::identity::container_hash(artifact),
    })
}
