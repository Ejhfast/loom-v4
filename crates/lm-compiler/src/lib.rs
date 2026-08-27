//! The source-to-artifact pipeline and the package build loop.
//!
//! The crate holds four layers:
//!
//! - `manifest`: the `lm.package` manifest, a strict TOML subset;
//! - `graph`: packages, the module tree from files, and the
//!   dependency DAG;
//! - `env` and `module`: the explicit typed environments and the
//!   compilation of one module against dependency interfaces;
//! - `link`: artifact resolution and namespace publication;
//! - `cache` and `build`: the two-stage content-addressed build
//!   directory and the build loop.
//!
//! The layer above `lm-verify` never runs code. Every artifact this
//! crate produces meets the one verifier before it executes.

pub mod build;
pub mod cache;
mod core;
pub mod env;
pub mod graph;
pub use lm_link as link;
pub mod manifest;
pub mod module;
pub mod scaffold;
pub mod standard;

pub use build::{build_package, BuildReport, ModuleReport};
pub use cache::{compile_key_with_bundle, write_atomic};
pub use core::{
    core_link_env, core_link_env_with_bundle, core_link_unit, core_link_unit_with_bundle,
};
pub use env::{CompileEnv, LinkEnv, LinkUnit};
pub use lm_link::{resolve_artifact, CodeArena, CodeNamespace, NamespaceId};
pub use manifest::{parse_manifest, Manifest};
pub use module::{
    compile_module, compile_module_with_bundle, compile_module_with_options,
    compile_module_with_options_and_bundle, CompileOptions, CompiledModule,
};
pub use standard::{compile_source, CompiledSource, StandardCatalog};
