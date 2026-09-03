//! The bundled standard-library module catalog.
//!
//! Each entry is a literal Loom module. It has one module path, one
//! interface, one artifact, and explicit imports.
//!
//! The source-backed catalog compiles each selected module once per
//! process. Its callers depend only on decoded artifacts.

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
const TIME_PATH: &str = "std.time";
const RANDOM_PATH: &str = "std.random";
const PATH_PATH: &str = "std.path";
const URL_PATH: &str = "std.url";
const DIGEST_PATH: &str = "std.digest";
const UUID_PATH: &str = "std.uuid";
const COMPRESS_PATH: &str = "std.compress";

const IO_SOURCE: &str = include_str!("../../../std/io.lm");
const FS_SOURCE: &str = include_str!("../../../std/fs.lm");
const TERM_SOURCE: &str = include_str!("../../../std/term.lm");
const TLS_SOURCE: &str = include_str!("../../../std/tls.lm");
const HTTP_SOURCE: &str = include_str!("../../../std/http.lm");
const BASE64_SOURCE: &str = include_str!("../../../std/base64.lm");
const JSON_SOURCE: &str = include_str!("../../../std/json.lm");
const TIME_SOURCE: &str = include_str!("../../../std/time.lm");
const RANDOM_SOURCE: &str = include_str!("../../../std/random.lm");
const PATH_SOURCE: &str = include_str!("../../../std/path.lm");
const URL_SOURCE: &str = include_str!("../../../std/url.lm");
const DIGEST_SOURCE: &str = include_str!("../../../std/digest.lm");
const UUID_SOURCE: &str = include_str!("../../../std/uuid.lm");
const COMPRESS_SOURCE: &str = include_str!("../../../std/compress.lm");

static IO: OnceLock<CompiledModule> = OnceLock::new();
static FS: OnceLock<CompiledModule> = OnceLock::new();
static TERM: OnceLock<CompiledModule> = OnceLock::new();
static TLS: OnceLock<CompiledModule> = OnceLock::new();
static HTTP: OnceLock<CompiledModule> = OnceLock::new();
static BASE64: OnceLock<CompiledModule> = OnceLock::new();
static JSON: OnceLock<CompiledModule> = OnceLock::new();
static TIME: OnceLock<CompiledModule> = OnceLock::new();
static RANDOM: OnceLock<CompiledModule> = OnceLock::new();
static PATH: OnceLock<CompiledModule> = OnceLock::new();
static URL: OnceLock<CompiledModule> = OnceLock::new();
static DIGEST: OnceLock<CompiledModule> = OnceLock::new();
static UUID: OnceLock<CompiledModule> = OnceLock::new();
static COMPRESS: OnceLock<CompiledModule> = OnceLock::new();

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
            TIME_PATH,
            RANDOM_PATH,
            PATH_PATH,
            URL_PATH,
            DIGEST_PATH,
            UUID_PATH,
            COMPRESS_PATH,
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
            TIME_PATH => Some(time()),
            RANDOM_PATH => Some(random()),
            PATH_PATH => Some(path_module()),
            URL_PATH => Some(url_module()),
            DIGEST_PATH => Some(digest()),
            UUID_PATH => Some(uuid()),
            COMPRESS_PATH => Some(compress()),
            _ => None,
        }
    }

    /// Select a module and its dependencies in link order.
    pub fn select(self, paths: &[&str]) -> Result<Vec<&'static CompiledModule>, String> {
        let mut needs = StandardNeeds::default();
        for path in paths {
            match *path {
                IO_PATH => needs.io = true,
                FS_PATH => needs.fs = true,
                TERM_PATH => needs.term = true,
                TLS_PATH => needs.tls = true,
                HTTP_PATH => {
                    needs.tls = true;
                    needs.http = true;
                    needs.url = true;
                    needs.compress = true;
                }
                BASE64_PATH => needs.base64 = true,
                JSON_PATH => needs.json = true,
                TIME_PATH => needs.time = true,
                RANDOM_PATH => needs.random = true,
                PATH_PATH => needs.path = true,
                URL_PATH => needs.url = true,
                DIGEST_PATH => needs.digest = true,
                UUID_PATH => {
                    needs.time = true;
                    needs.uuid = true;
                }
                COMPRESS_PATH => needs.compress = true,
                _ => return Err(format!("`{path}` is not a bundled standard module")),
            }
        }
        Ok(selected_modules(needs))
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
    HTTP.get_or_init(|| {
        compile_bundled(
            HTTP_PATH,
            "std/http.lm",
            HTTP_SOURCE,
            &[tls(), url_module(), compress()],
        )
    })
}

fn base64() -> &'static CompiledModule {
    BASE64.get_or_init(|| compile_bundled(BASE64_PATH, "std/base64.lm", BASE64_SOURCE, &[]))
}

fn json() -> &'static CompiledModule {
    JSON.get_or_init(|| compile_bundled(JSON_PATH, "std/json.lm", JSON_SOURCE, &[]))
}

fn time() -> &'static CompiledModule {
    TIME.get_or_init(|| compile_bundled(TIME_PATH, "std/time.lm", TIME_SOURCE, &[]))
}

fn random() -> &'static CompiledModule {
    RANDOM.get_or_init(|| compile_bundled(RANDOM_PATH, "std/random.lm", RANDOM_SOURCE, &[]))
}

fn path_module() -> &'static CompiledModule {
    PATH.get_or_init(|| compile_bundled(PATH_PATH, "std/path.lm", PATH_SOURCE, &[]))
}

fn url_module() -> &'static CompiledModule {
    URL.get_or_init(|| compile_bundled(URL_PATH, "std/url.lm", URL_SOURCE, &[]))
}

fn digest() -> &'static CompiledModule {
    DIGEST.get_or_init(|| compile_bundled(DIGEST_PATH, "std/digest.lm", DIGEST_SOURCE, &[]))
}

fn uuid() -> &'static CompiledModule {
    UUID.get_or_init(|| compile_bundled(UUID_PATH, "std/uuid.lm", UUID_SOURCE, &[time()]))
}

fn compress() -> &'static CompiledModule {
    COMPRESS.get_or_init(|| compile_bundled(COMPRESS_PATH, "std/compress.lm", COMPRESS_SOURCE, &[]))
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
        TIME_PATH,
        RANDOM_PATH,
        PATH_PATH,
        URL_PATH,
        DIGEST_PATH,
        UUID_PATH,
        COMPRESS_PATH,
    ]
    .into_iter()
    .find(|module| text == *module || text.starts_with(&format!("{module}.")))
}

#[derive(Default)]
struct StandardNeeds {
    io: bool,
    fs: bool,
    term: bool,
    tls: bool,
    http: bool,
    base64: bool,
    json: bool,
    time: bool,
    random: bool,
    path: bool,
    url: bool,
    digest: bool,
    uuid: bool,
    compress: bool,
}

fn selected_modules(needs: StandardNeeds) -> Vec<&'static CompiledModule> {
    let mut modules = Vec::new();
    if needs.path {
        modules.push(path_module());
    }
    if needs.io {
        modules.push(io());
    }
    if needs.fs {
        modules.push(fs());
    }
    if needs.term {
        modules.push(term());
    }
    if needs.tls {
        modules.push(tls());
    }
    if needs.url {
        modules.push(url_module());
    }
    if needs.compress {
        modules.push(compress());
    }
    if needs.http {
        modules.push(http());
    }
    if needs.base64 {
        modules.push(base64());
    }
    if needs.json {
        modules.push(json());
    }
    if needs.time {
        modules.push(time());
    }
    if needs.random {
        modules.push(random());
    }
    if needs.digest {
        modules.push(digest());
    }
    if needs.uuid {
        modules.push(uuid());
    }
    modules
}

/// Select the standard-module closure named by source `use` paths.
pub(crate) fn modules_for_uses(uses: &[Vec<String>]) -> Vec<&'static CompiledModule> {
    let mut needs = StandardNeeds::default();
    for path in uses {
        match module_for_use(path) {
            Some(IO_PATH) => needs.io = true,
            Some(FS_PATH) => needs.fs = true,
            Some(TERM_PATH) => needs.term = true,
            Some(TLS_PATH) => needs.tls = true,
            Some(HTTP_PATH) => {
                needs.tls = true;
                needs.http = true;
                needs.url = true;
                needs.compress = true;
            }
            Some(BASE64_PATH) => needs.base64 = true,
            Some(JSON_PATH) => needs.json = true,
            Some(TIME_PATH) => needs.time = true,
            Some(RANDOM_PATH) => needs.random = true,
            Some(PATH_PATH) => needs.path = true,
            Some(URL_PATH) => needs.url = true,
            Some(DIGEST_PATH) => needs.digest = true,
            Some(UUID_PATH) => {
                needs.time = true;
                needs.uuid = true;
            }
            Some(COMPRESS_PATH) => needs.compress = true,
            _ => {}
        }
    }
    selected_modules(needs)
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
                TIME_PATH,
                RANDOM_PATH,
                PATH_PATH,
                URL_PATH,
                DIGEST_PATH,
                UUID_PATH,
                COMPRESS_PATH,
            ]
        );
        assert!(modules_for_uses(&[]).is_empty());
        let selected = catalog.select(&[HTTP_PATH]).expect("the module exists");
        let paths: Vec<&str> = selected.iter().map(|module| module.path.as_str()).collect();
        assert_eq!(paths, &[TLS_PATH, URL_PATH, COMPRESS_PATH, HTTP_PATH]);
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
        let compiled = compile(
            "use std.fs.read_dir_sorted\nread_dir_sorted(Path(\".\", PathStyle.Posix), 4)\n",
        );
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
        assert_eq!(
            compiled.standard_modules,
            &[TLS_PATH, URL_PATH, COMPRESS_PATH, HTTP_PATH]
        );
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

    #[test]
    fn time_source_selects_only_time() {
        let compiled = compile("use std.time.seconds\nseconds(2)\n");
        assert_eq!(compiled.standard_modules, &[TIME_PATH]);
    }

    #[test]
    fn random_source_selects_only_random() {
        let compiled = compile("use std.random.seeded\nseeded(1).next_bits()\n");
        assert_eq!(compiled.standard_modules, &[RANDOM_PATH]);
    }

    #[test]
    fn path_source_selects_only_path() {
        let compiled = compile("use std.path.normalize\nnormalize(Path(\"a\", PathStyle.Posix))\n");
        assert_eq!(compiled.standard_modules, &[PATH_PATH]);
    }

    #[test]
    fn url_source_selects_only_url() {
        let compiled = compile("use std.url.parse_url\nparse_url(\"https://example.com\")\n");
        assert_eq!(compiled.standard_modules, &[URL_PATH]);
    }

    #[test]
    fn compression_source_selects_only_compression() {
        let compiled = compile(
            "use std.compress.CompressionLevel\nuse std.compress.gzip_compress\ngzip_compress(b\"ready\", CompressionLevel.Fast)\n",
        );
        assert_eq!(compiled.standard_modules, &[COMPRESS_PATH]);
    }
}
