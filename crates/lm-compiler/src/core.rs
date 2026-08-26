//! Build the canonical core link unit.

use crate::LinkEnv;
use lm_bytecode::artifact::{LinkUnit, CORE_MODULE_PATH};
use std::sync::{Arc, OnceLock};

static STANDARD_CORE: OnceLock<LinkUnit> = OnceLock::new();

/// Return the canonical core unit.
pub fn core_link_unit() -> Result<LinkUnit, String> {
    let bundle = lm_abi::standard_bundle();
    core_link_unit_with_bundle(&bundle)
}

/// Return the canonical core unit for one ABI bundle.
pub fn core_link_unit_with_bundle(bundle: &Arc<lm_abi::AbiBundle>) -> Result<LinkUnit, String> {
    let standard = lm_abi::standard_bundle();
    if bundle.digest() == standard.digest() {
        let unit = STANDARD_CORE.get_or_init(|| {
            build_core_link_unit(&standard).expect("the standard core link unit builds")
        });
        return Ok(unit.clone());
    }
    build_core_link_unit(bundle)
}

/// Create a link environment with the canonical core.
pub fn core_link_env() -> Result<LinkEnv, String> {
    let bundle = lm_abi::standard_bundle();
    core_link_env_with_bundle(&bundle)
}

/// Create a core link environment for one ABI bundle.
pub fn core_link_env_with_bundle(bundle: &Arc<lm_abi::AbiBundle>) -> Result<LinkEnv, String> {
    let mut env = LinkEnv::new();
    env.bind(core_link_unit_with_bundle(bundle)?)
        .map_err(|error| format!("error: {error}\n"))?;
    Ok(env)
}

fn build_core_link_unit(bundle: &Arc<lm_abi::AbiBundle>) -> Result<LinkUnit, String> {
    let (module, items) = lm_hir::core_image_with_bundle(bundle.clone());
    lm_verify::verify_module_with_bundle(&module, bundle)
        .map_err(|error| format!("error: the verifier rejected the core: {error}\n"))?;
    let identity = lm_bytecode::identity::module_identity_with_bundle(&module, bundle)
        .map_err(|error| format!("error: the core identity failed: {error}\n"))?;
    let interface = lm_bytecode::interface::build_interface_with_bundle(
        &module,
        &identity,
        CORE_MODULE_PATH,
        &items,
        bundle,
    )
    .map_err(|error| format!("error: the core interface failed: {error}\n"))?;
    LinkUnit::new_with_bundle(CORE_MODULE_PATH, module, interface, Vec::new(), bundle)
        .map_err(|error| format!("error: the core link unit failed: {error}\n"))
}
