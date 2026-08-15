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
pub use hir::dump_classes;
pub use lower::lower_module;

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
