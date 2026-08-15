//! Typed HIR, type checking, and CFG lowering.
//!
//! `check` turns a parsed module into a typed HIR module. `lower`
//! turns typed HIR into basic-block bytecode. `dump_cfg` renders the
//! result for humans.

pub mod check;
pub mod hir;
pub mod lower;

pub use check::check_module;
pub use hir::dump_classes;
pub use lower::lower_module;

/// Render the lowered control-flow graph in a stable readable form.
pub fn dump_cfg(module: &lm_bytecode::Module) -> String {
    lower::dump_cfg(module)
}
