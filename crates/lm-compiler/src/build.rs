//! The build loop: compile every module of the dependency graph and
//! build the root artifact.
//!
//! The loop walks the packages in dependency order and the modules of
//! each package in import order. Every module compiles against the
//! interfaces the graph already produced, never against a source file
//! of another package. A cached module skips the compiler and keeps
//! its recorded artifact.

use crate::cache::{compile_key, interface_identity, write_atomic, BuildDir};
use crate::env::CompileEnv;
use crate::graph::{load_workspace, module_order, Workspace};
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
    /// The selected program artifact.
    pub artifact: Option<PathBuf>,
    pub artifact_id: Option<lm_bytecode::artifact::ArtifactId>,
    pub container_hash: Option<[u8; 32]>,
    /// True when the artifact cache supplied the LMAR bytes.
    pub artifact_cached: bool,
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
    build_package_for(start, build_root, BuildTarget::Program)
}

/// Build one package with a generated test entry.
pub fn build_test_package(start: &Path, build_root: &Path) -> Result<BuildReport, String> {
    build_package_for(start, build_root, BuildTarget::Tests)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildTarget {
    Program,
    Tests,
}

fn build_package_for(
    start: &Path,
    build_root: &Path,
    target: BuildTarget,
) -> Result<BuildReport, String> {
    let workspace = load_workspace(start)?;
    let dir = BuildDir::new(build_root);
    let mut env = CompileEnv::new();
    let mut interfaces: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    let mut link_env = crate::core_link_env()?;
    let mut modules: Vec<ModuleReport> = Vec::new();
    let mut standard_uses: Vec<Vec<String>> = workspace
        .order
        .iter()
        .flat_map(|name| workspace.package(name).modules.iter())
        .flat_map(|module| module.uses.iter().cloned())
        .collect();
    if target == BuildTarget::Tests {
        standard_uses.push(vec!["std".to_string(), "test".to_string()]);
    }
    let standard = crate::standard::modules_for_uses(&standard_uses);
    for module in &standard {
        let interface_id = interface_identity(&module.interface);
        interfaces.insert(module.path.clone(), interface_id);
        let unit = link_env
            .prepare_unit(
                module.path.clone(),
                module.module.clone(),
                module.interface.clone(),
            )
            .map_err(|error| format!("error: {error}\n"))?;
        env.bind_unit(&unit)
            .map_err(|error| format!("error: {error}\n"))?;
        link_env
            .bind_unit(unit)
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
                        module: decoded,
                        interface,
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
                    let module_bytes = lm_bytecode::encode(&entry.module);
                    let interface_bytes =
                        lm_bytecode::interface::encode_interface(&entry.interface);
                    dir.write(&key, &module_bytes, &interface_bytes)?;
                    entry
                }
            };
            let interface_id = interface_identity(&compiled.interface);
            let semantic_hash = compiled.semantic_hash;
            interfaces.insert(module.path.clone(), interface_id);
            let unit = link_env
                .prepare_unit(compiled.path, compiled.module, compiled.interface)
                .map_err(|error| format!("error: {error}\n"))?;
            env.bind_unit(&unit).map_err(|e| format!("error: {e}\n"))?;
            modules.push(ModuleReport {
                package: package_name.clone(),
                path: module.path.clone(),
                cached,
                semantic_hash,
                interface_id,
            });
            link_env
                .bind_unit(unit)
                .map_err(|error| format!("error: {error}\n"))?;
        }
    }
    let root_package = workspace.package(&workspace.root);
    let mut report = BuildReport {
        root: workspace.root.clone(),
        modules,
        artifact: None,
        artifact_id: None,
        container_hash: None,
        artifact_cached: false,
    };
    if target == BuildTarget::Program && !root_package.has_main() {
        return Ok(report);
    }
    let (main_path, artifact_name) = match target {
        BuildTarget::Program => (format!("{}.main", workspace.root), workspace.root.clone()),
        BuildTarget::Tests => {
            let module_paths: Vec<String> = root_package
                .modules
                .iter()
                .map(|module| module.path.clone())
                .collect();
            let runner_path = format!("{}.__loom_test_runner", workspace.root);
            let source = SourceFile::new("<test runner>", test_entry_source(&module_paths));
            let mut local = env.clone();
            local.bind_standard_root();
            local
                .bind_root(&workspace.root, &workspace.root)
                .map_err(|error| format!("error: {error}\n"))?;
            for (name, prefix) in &root_package.deps {
                local
                    .bind_root(name, prefix)
                    .map_err(|error| format!("error: {error}\n"))?;
            }
            let compiled = crate::module::compile_module_with_options(
                &runner_path,
                &source,
                &local.freeze(),
                true,
                &crate::module::CompileOptions::new(),
            )?;
            let unit = link_env
                .prepare_unit(compiled.path, compiled.module, compiled.interface)
                .map_err(|error| format!("error: {error}\n"))?;
            link_env
                .bind_unit(unit)
                .map_err(|error| format!("error: {error}\n"))?;
            (runner_path, format!("{}-tests", workspace.root))
        }
    };
    let artifact = link_env
        .freeze()
        .artifact(&main_path)
        .map_err(|e| format!("error: {e}\n"))?;
    let artifact_id = artifact.id();
    let bytes = match dir.read_artifact(&artifact_id) {
        Some(bytes) => {
            report.artifact_cached = true;
            bytes
        }
        None => {
            let bytes =
                lm_bytecode::artifact::encode(&artifact).map_err(|e| format!("error: {e}\n"))?;
            dir.write_artifact(&artifact_id, &bytes)?;
            bytes
        }
    };
    let container_hash = lm_bytecode::identity::container_hash(&bytes);
    let debug = dir.debug();
    std::fs::create_dir_all(&debug)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", debug.display()))?;
    let path = debug.join(format!("{artifact_name}.lma"));
    write_atomic(&path, &bytes)?;
    report.artifact = Some(path);
    report.artifact_id = Some(artifact_id);
    report.container_hash = Some(container_hash);
    Ok(report)
}

fn test_entry_source(modules: &[String]) -> String {
    let descriptors = modules
        .iter()
        .map(|module| format!("codeof({module})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "use std.test.run_matching\n\
         report = run_matching([{descriptors}], sys.args())\n\
         println(report)\n\
         report.failed()\n"
    )
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
