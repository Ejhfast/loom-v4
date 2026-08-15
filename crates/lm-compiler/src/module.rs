//! Compile one source module against a frozen compile environment.
//!
//! The result is the artifact container, the interface, and the
//! hashes. The step is pure: it reads no file and writes no file.

use crate::env::FrozenCompileEnv;
use lm_bytecode::interface::{IfaceItem, Interface};
use lm_bytecode::Module;
use lm_source::SourceFile;

/// One compiled module.
#[derive(Debug, Clone)]
pub struct CompiledModule {
    /// The full module path, for example `mathlib.matrix`.
    pub path: String,
    pub module: Module,
    pub artifact: Vec<u8>,
    pub interface: Interface,
    pub interface_bytes: Vec<u8>,
    pub semantic_hash: [u8; 32],
    pub container_hash: [u8; 32],
}

/// Compile one module. `Err` carries fully rendered diagnostic text.
///
/// `is_main` marks the module that holds the program entry. Every
/// other module must end without a trailing expression, because a
/// library module has no entry to run.
pub fn compile_module(
    path: &str,
    source: &SourceFile,
    env: &FrozenCompileEnv,
    is_main: bool,
) -> Result<CompiledModule, String> {
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(source))?;
    if !is_main && !ast.entry.is_empty() {
        let span = ast.entry[0].span;
        let diagnostic = lm_source::diag::Diagnostic::new(
            "E1053",
            format!(
                "the module `{path}` ends with an expression, and only \
                 `src/main.lm` holds the program entry"
            ),
            span,
        );
        return Err(diagnostic.render(source));
    }
    let hir = lm_hir::check_module_with(
        &ast,
        lm_hir::CheckOptions {
            prelude: true,
            module_path: path.to_string(),
            imports: env.imports().clone(),
        },
    )
    .map_err(|d| d.render(source))?;
    let module = lm_hir::lower_module(&hir);
    lm_verify::verify_module(&module)
        .map_err(|e| format!("error: the verifier rejected `{path}`: {e}\n"))?;
    let identity = lm_bytecode::identity::module_identity(&module)
        .map_err(|e| format!("error: `{path}`: {e}\n"))?;
    let items: Vec<IfaceItem> = hir.exports.iter().map(|e| e.item.clone()).collect();
    let interface = lm_bytecode::interface::build_interface(&module, &identity, path, &items)
        .map_err(|e| format!("error: `{path}`: {e}\n"))?;
    let interface_bytes = lm_bytecode::interface::encode_interface(&interface);
    let artifact = lm_bytecode::encode(&module);
    let container_hash = lm_bytecode::identity::container_hash(&artifact);
    Ok(CompiledModule {
        path: path.to_string(),
        module,
        artifact,
        interface,
        interface_bytes,
        semantic_hash: identity.semantic_hash,
        container_hash,
    })
}
