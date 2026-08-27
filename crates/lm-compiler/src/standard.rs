//! The bundled standard-library module catalog.
//!
//! Each entry is a literal Loom module. It has one module path, one
//! interface, one artifact, and explicit imports.
//!
//! The source-backed catalog compiles each selected module once per
//! process. A later release bundle can replace the source builder
//! with decoded artifacts without changing its callers.

use crate::{compile_module, CompileEnv, CompiledModule};
use lm_bytecode::artifact::{Artifact, LinkUnit};
use lm_source::SourceFile;
use std::sync::OnceLock;

const IO_PATH: &str = "std.io";
const FS_PATH: &str = "std.fs";
const TERM_PATH: &str = "std.term";
const TLS_PATH: &str = "std.tls";
const HTTP_PATH: &str = "std.http";
const BASE64_PATH: &str = "std.base64";
const JSON_PATH: &str = "std.json";

const IO_SOURCE: &str = include_str!("../../../std/io.lm");
const FS_SOURCE: &str = include_str!("../../../std/fs.lm");
const TERM_SOURCE: &str = include_str!("../../../std/term.lm");
const TLS_SOURCE: &str = include_str!("../../../std/tls.lm");
const HTTP_SOURCE: &str = include_str!("../../../std/http.lm");
const BASE64_SOURCE: &str = include_str!("../../../std/base64.lm");
const JSON_SOURCE: &str = include_str!("../../../std/json.lm");

static IO: OnceLock<CompiledModule> = OnceLock::new();
static FS: OnceLock<CompiledModule> = OnceLock::new();
static TERM: OnceLock<CompiledModule> = OnceLock::new();
static TLS: OnceLock<CompiledModule> = OnceLock::new();
static HTTP: OnceLock<CompiledModule> = OnceLock::new();
static BASE64: OnceLock<CompiledModule> = OnceLock::new();
static JSON: OnceLock<CompiledModule> = OnceLock::new();

/// One source compilation and its exact artifact graph.
#[derive(Debug, Clone)]
pub struct CompiledSource {
    /// The independently compiled source module.
    pub root: CompiledModule,
    /// The artifact that contains the selected source units.
    pub artifact: Artifact,
    /// The bundled modules selected by the source imports.
    pub standard_modules: Vec<String>,
}

/// The immutable bundled standard-library catalog.
#[derive(Debug, Clone, Copy, Default)]
pub struct StandardCatalog;

impl StandardCatalog {
    pub fn bundled() -> StandardCatalog {
        StandardCatalog
    }

    /// The module paths supplied by this catalog.
    pub fn paths(self) -> &'static [&'static str] {
        &[
            IO_PATH,
            FS_PATH,
            TERM_PATH,
            TLS_PATH,
            HTTP_PATH,
            BASE64_PATH,
            JSON_PATH,
        ]
    }

    /// Compile and return one bundled module.
    ///
    /// The cache runs each standard-module compiler once per process.
    pub fn module(self, path: &str) -> Option<&'static CompiledModule> {
        match path {
            IO_PATH => Some(io()),
            FS_PATH => Some(fs()),
            TERM_PATH => Some(term()),
            TLS_PATH => Some(tls()),
            HTTP_PATH => Some(http()),
            BASE64_PATH => Some(base64()),
            JSON_PATH => Some(json()),
            _ => None,
        }
    }

    /// Select a module and its dependencies in link order.
    pub fn select(self, paths: &[&str]) -> Result<Vec<&'static CompiledModule>, String> {
        let mut needs_io = false;
        let mut needs_fs = false;
        let mut needs_term = false;
        let mut needs_tls = false;
        let mut needs_http = false;
        let mut needs_base64 = false;
        let mut needs_json = false;
        for path in paths {
            match *path {
                IO_PATH => needs_io = true,
                FS_PATH => needs_fs = true,
                TERM_PATH => needs_term = true,
                TLS_PATH => needs_tls = true,
                HTTP_PATH => {
                    needs_tls = true;
                    needs_http = true;
                }
                BASE64_PATH => needs_base64 = true,
                JSON_PATH => needs_json = true,
                _ => return Err(format!("`{path}` is not a bundled standard module")),
            }
        }
        Ok(selected_modules(
            needs_io,
            needs_fs,
            needs_term,
            needs_tls,
            needs_http,
            needs_base64,
            needs_json,
        ))
    }

    /// Bind selected standard units into one compile environment.
    pub fn bind(self, env: &mut CompileEnv, paths: &[&str]) -> Result<Vec<LinkUnit>, String> {
        let modules = self.select(paths)?;
        let mut links = crate::core_link_env()?;
        let mut units = Vec::with_capacity(modules.len());
        env.bind_standard_root();
        for module in modules {
            let unit = module
                .clone()
                .into_link_unit(&links)
                .map_err(|error| error.to_string())?;
            env.bind_unit(&unit).map_err(|error| error.to_string())?;
            links
                .bind_unit(unit.clone())
                .map_err(|error| error.to_string())?;
            units.push(unit);
        }
        Ok(units)
    }
}

fn compile_bundled(
    path: &str,
    file: &str,
    source: &str,
    dependencies: &[&CompiledModule],
) -> CompiledModule {
    let mut env = CompileEnv::new();
    let mut links = crate::core_link_env()
        .unwrap_or_else(|error| panic!("the core link environment is invalid: {error}"));
    env.bind_standard_root();
    for dependency in dependencies {
        let unit = (*dependency)
            .clone()
            .into_link_unit(&links)
            .unwrap_or_else(|error| panic!("the bundled dependency is invalid: {error}"));
        env.bind_unit(&unit)
            .unwrap_or_else(|error| panic!("the bundled module environment is invalid: {error}"));
        links
            .bind_unit(unit)
            .unwrap_or_else(|error| panic!("the bundled link environment is invalid: {error}"));
    }
    compile_module(path, &SourceFile::new(file, source), &env.freeze(), false)
        .unwrap_or_else(|error| panic!("the bundled module `{path}` does not compile:\n{error}"))
}

fn tls() -> &'static CompiledModule {
    TLS.get_or_init(|| compile_bundled(TLS_PATH, "std/tls.lm", TLS_SOURCE, &[]))
}

fn io() -> &'static CompiledModule {
    IO.get_or_init(|| compile_bundled(IO_PATH, "std/io.lm", IO_SOURCE, &[]))
}

fn fs() -> &'static CompiledModule {
    FS.get_or_init(|| compile_bundled(FS_PATH, "std/fs.lm", FS_SOURCE, &[]))
}

fn term() -> &'static CompiledModule {
    TERM.get_or_init(|| compile_bundled(TERM_PATH, "std/term.lm", TERM_SOURCE, &[]))
}

fn http() -> &'static CompiledModule {
    HTTP.get_or_init(|| compile_bundled(HTTP_PATH, "std/http.lm", HTTP_SOURCE, &[tls()]))
}

fn base64() -> &'static CompiledModule {
    BASE64.get_or_init(|| compile_bundled(BASE64_PATH, "std/base64.lm", BASE64_SOURCE, &[]))
}

fn json() -> &'static CompiledModule {
    JSON.get_or_init(|| compile_bundled(JSON_PATH, "std/json.lm", JSON_SOURCE, &[]))
}

fn module_for_use(path: &[String]) -> Option<&'static str> {
    let text = path.join(".");
    [
        IO_PATH,
        FS_PATH,
        TERM_PATH,
        HTTP_PATH,
        TLS_PATH,
        BASE64_PATH,
        JSON_PATH,
    ]
    .into_iter()
    .find(|module| text == *module || text.starts_with(&format!("{module}.")))
}

fn selected_modules(
    needs_io: bool,
    needs_fs: bool,
    needs_term: bool,
    needs_tls: bool,
    needs_http: bool,
    needs_base64: bool,
    needs_json: bool,
) -> Vec<&'static CompiledModule> {
    let mut modules = Vec::new();
    if needs_io {
        modules.push(io());
    }
    if needs_fs {
        modules.push(fs());
    }
    if needs_term {
        modules.push(term());
    }
    if needs_tls {
        modules.push(tls());
    }
    if needs_http {
        modules.push(http());
    }
    if needs_base64 {
        modules.push(base64());
    }
    if needs_json {
        modules.push(json());
    }
    modules
}

/// Select the standard-module closure named by source `use` paths.
pub(crate) fn modules_for_uses(uses: &[Vec<String>]) -> Vec<&'static CompiledModule> {
    let mut needs_io = false;
    let mut needs_fs = false;
    let mut needs_term = false;
    let mut needs_tls = false;
    let mut needs_http = false;
    let mut needs_base64 = false;
    let mut needs_json = false;
    for path in uses {
        match module_for_use(path) {
            Some(IO_PATH) => needs_io = true,
            Some(FS_PATH) => needs_fs = true,
            Some(TERM_PATH) => needs_term = true,
            Some(TLS_PATH) => needs_tls = true,
            Some(HTTP_PATH) => {
                needs_tls = true;
                needs_http = true;
            }
            Some(BASE64_PATH) => needs_base64 = true,
            Some(JSON_PATH) => needs_json = true,
            _ => {}
        }
    }
    selected_modules(
        needs_io,
        needs_fs,
        needs_term,
        needs_tls,
        needs_http,
        needs_base64,
        needs_json,
    )
}

/// Compile one source module and link its requested standard modules.
///
/// This path serves single-file tools and the test harness. Package
/// builds use the same catalog through their existing module graph.
pub fn compile_source(
    path: &str,
    source: &SourceFile,
    is_main: bool,
) -> Result<CompiledSource, String> {
    let ast = lm_source::parse::parse(&source.text).map_err(|error| error.render(source))?;
    let uses: Vec<Vec<String>> = ast.uses.iter().map(|item| item.path.clone()).collect();
    let standard = modules_for_uses(&uses);
    let mut env = CompileEnv::new();
    let mut link_env = crate::core_link_env()?;
    env.bind_standard_root();
    for module in &standard {
        let unit = (*module)
            .clone()
            .into_link_unit(&link_env)
            .map_err(|error| format!("error: {error}\n"))?;
        env.bind_unit(&unit)
            .map_err(|error| format!("error: {error}\n"))?;
        link_env
            .bind_unit(unit)
            .map_err(|error| format!("error: {error}\n"))?;
    }
    let root = compile_module(path, source, &env.freeze(), is_main)?;
    let root_unit = root
        .clone()
        .into_link_unit(&link_env)
        .map_err(|error| format!("error: {error}\n"))?;
    link_env
        .bind_unit(root_unit)
        .map_err(|error| format!("error: {error}\n"))?;
    let artifact = link_env
        .freeze()
        .artifact(path)
        .map_err(|error| format!("error: {error}\n"))?;
    Ok(CompiledSource {
        artifact,
        standard_modules: standard.iter().map(|module| module.path.clone()).collect(),
        root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(text: &str) -> CompiledSource {
        let source = SourceFile::new("standard_test.lm", text);
        compile_source("standard.test", &source, true).expect("the source compiles")
    }

    #[test]
    fn catalog_lists_selective_modules() {
        let catalog = StandardCatalog::bundled();
        assert_eq!(
            catalog.paths(),
            &[
                IO_PATH,
                FS_PATH,
                TERM_PATH,
                TLS_PATH,
                HTTP_PATH,
                BASE64_PATH,
                JSON_PATH,
            ]
        );
        assert!(modules_for_uses(&[]).is_empty());
        let selected = catalog.select(&[HTTP_PATH]).expect("the module exists");
        let paths: Vec<&str> = selected.iter().map(|module| module.path.as_str()).collect();
        assert_eq!(paths, &[TLS_PATH, HTTP_PATH]);
        assert!(catalog.select(&["std.missing"]).is_err());
    }

    #[test]
    fn catalog_binds_an_explicit_compile_environment() {
        let mut env = CompileEnv::new();
        let modules = StandardCatalog::bundled()
            .bind(&mut env, &[TLS_PATH])
            .expect("the catalog binds");
        assert_eq!(modules.len(), 1);
        assert_eq!(env.roots(), &["std"]);
        assert!(env.freeze().interface(TLS_PATH).is_some());
    }

    #[test]
    fn core_source_selects_no_standard_module() {
        assert!(compile("1\n").standard_modules.is_empty());
    }

    #[test]
    fn tls_source_selects_only_tls() {
        let compiled = compile("use std.tls.TlsVersion\nTlsVersion.Tls13\n");
        assert_eq!(compiled.standard_modules, &[TLS_PATH]);
    }

    #[test]
    fn io_source_selects_only_io() {
        let compiled = compile("use std.io.write_all\nwrite_all(b\"ready\")\n");
        assert_eq!(compiled.standard_modules, &[IO_PATH]);
    }

    #[test]
    fn file_source_selects_only_file_helpers() {
        let compiled = compile("use std.fs.read_dir_sorted\nread_dir_sorted(\".\", 4)\n");
        assert_eq!(compiled.standard_modules, &[FS_PATH]);
    }

    #[test]
    fn terminal_source_selects_only_terminal_helpers() {
        let compiled = compile("use std.term.clear_screen\nclear_screen()\n");
        assert_eq!(compiled.standard_modules, &[TERM_PATH]);
    }

    #[test]
    fn http_source_selects_its_dependency_closure() {
        let compiled = compile("use std.http.Http\nHttp().default_limits().max_headers\n");
        assert_eq!(compiled.standard_modules, &[TLS_PATH, HTTP_PATH]);
    }

    #[test]
    fn base64_source_selects_only_base64() {
        let compiled = compile("use std.base64.encode\nencode(b\"ready\")\n");
        assert_eq!(compiled.standard_modules, &[BASE64_PATH]);
    }

    #[test]
    fn json_source_selects_only_json() {
        let compiled = compile("use std.json.parse\nparse(\"null\")\n");
        assert_eq!(compiled.standard_modules, &[JSON_PATH]);
    }
}
