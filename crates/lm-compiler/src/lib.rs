//! The source-to-artifact pipeline and the package build loop.
//!
//! The crate holds four layers:
//!
//! - `manifest`: the `lm.package` manifest, a strict TOML subset;
//! - `graph`: packages, the module tree from files, and the
//!   dependency DAG;
//! - `env` and `module`: the explicit typed environments and the
//!   compilation of one module against dependency interfaces;
//! - `link`: the pure linker that merges modules into one program;
//! - `cache` and `build`: the three-stage content-addressed build
//!   directory and the build loop.
//!
//! The layer above `lm-verify` never runs code. Every artifact this
//! crate produces meets the one verifier before it executes.

pub mod build;
pub mod cache;
pub mod env;
pub mod graph;
pub mod link;
pub mod manifest;
pub mod module;
pub mod scaffold;
pub mod standard;

pub use build::{build_package, BuildReport, ModuleReport};
pub use cache::{
    load_through_store, user_cache_dir, write_atomic, Verdict, VerdictKey, VerifiedStore,
};
pub use env::{CompileEnv, LinkEnv, LinkUnit};
pub use link::{link, LinkedProgram};
pub use manifest::{parse_manifest, Manifest};
pub use module::{compile_module, compile_module_with_options, CompileOptions, CompiledModule};
pub use standard::{compile_program, compile_source, CompiledSource, StandardCatalog};
