//! Build the canonical core link unit.

use crate::LinkEnv;
use lm_bytecode::artifact::{LinkUnit, CORE_MODULE_PATH};
use std::sync::{Arc, OnceLock};

static STANDARD_CORE: OnceLock<Arc<LinkUnit>> = OnceLock::new();

/// Return the canonical core unit.
pub fn core_link_unit() -> Result<Arc<LinkUnit>, String> {
    let bundle = lm_abi::standard_bundle();
    core_link_unit_with_bundle(&bundle)
}

/// Return the canonical core unit for one ABI bundle.
pub fn core_link_unit_with_bundle(
    bundle: &Arc<lm_abi::AbiBundle>,
) -> Result<Arc<LinkUnit>, String> {
    let standard = lm_abi::standard_bundle();
    if bundle.digest() == standard.digest() {
        let unit = STANDARD_CORE.get_or_init(|| {
            Arc::new(build_core_link_unit(&standard).expect("the standard core link unit builds"))
        });
        return Ok(Arc::clone(unit));
    }
    build_core_link_unit(bundle).map(Arc::new)
}

/// Create a link environment with the canonical core.
pub fn core_link_env() -> Result<LinkEnv, String> {
    let bundle = lm_abi::standard_bundle();
    core_link_env_with_bundle(&bundle)
}

/// Create a core link environment for one ABI bundle.
pub fn core_link_env_with_bundle(bundle: &Arc<lm_abi::AbiBundle>) -> Result<LinkEnv, String> {
    let mut env = LinkEnv::new();
    env.bind_unit(core_link_unit_with_bundle(bundle)?)
        .map_err(|error| format!("error: {error}\n"))?;
    Ok(env)
}

fn build_core_link_unit(bundle: &Arc<lm_abi::AbiBundle>) -> Result<LinkUnit, String> {
    let module = lm_hir::core_image_with_bundle(bundle.clone());
    LinkUnit::from_module_with_bundle(CORE_MODULE_PATH, module, Vec::new(), bundle)
        .map_err(|error| format!("error: the core link unit failed: {error}\n"))
}
