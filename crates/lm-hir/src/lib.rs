//! Typed HIR, type checking, and CFG lowering.
//!
//! `check` turns a parsed module into a typed HIR module, with the
//! pinned core image compiled in by the same pipeline. `lower` turns
//! typed HIR into basic-block bytecode. `dump_cfg` renders the result
//! for humans.

pub mod check;
mod checkfn;
pub mod exhaust;
pub mod hir;
pub mod iface;
pub mod import;
pub mod lower;

pub use check::{check_module, check_module_with, CheckOptions, CORE_SOURCE};

/// The digest of the pinned core sources.
///
/// Every module embeds the core image, so a core edit changes every
/// artifact. A build cache must therefore key on this digest: the
/// compiler ABI version does not have to move when the core does.
pub fn core_source_digest() -> [u8; 32] {
    lm_bytecode::hash::hash256(CORE_SOURCE.as_bytes())
}
pub use hir::{dump_classes, HirImportDef, HirModule};
pub use lower::{
    lower_module, lower_module_with_linkage, LateCallable, LateCallableKind, LateClass,
    LowerLinkage,
};

/// Render the lowered control-flow graph in a stable readable form.
pub fn dump_cfg(module: &lm_bytecode::Module) -> String {
    lower::dump_cfg(module)
}

/// Compile the pinned core image alone: the core sources with an
/// empty user module. The result is the canonical core artifact whose
/// bytes are pinned by hash.
pub fn core_image() -> lm_bytecode::Module {
    let empty = lm_source::parse::parse("").expect("the empty module parses");
    let hir = check_module_with(
        &empty,
        CheckOptions {
            prelude: false,
            ..CheckOptions::default()
        },
    )
    .expect("the core image checks");
    lower_module(&hir)
}
