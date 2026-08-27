//! Build the canonical core link unit.

use crate::LinkEnv;
use lm_bytecode::artifact::{LinkUnit, CORE_MODULE_PATH};
use std::sync::{Arc, OnceLock};

struct CoreProvider {
    unit: Arc<LinkUnit>,
    intrinsics: Arc<[Option<lm_abi::IntrinsicSlot>]>,
}

static STANDARD_CORE: OnceLock<CoreProvider> = OnceLock::new();

type CoreProviderParts = (Arc<LinkUnit>, Arc<[Option<lm_abi::IntrinsicSlot>]>);

/// Return the canonical core unit.
pub fn core_link_unit() -> Result<Arc<LinkUnit>, String> {
    let bundle = lm_abi::standard_bundle();
    core_link_unit_with_bundle(&bundle)
}

/// Return the canonical core unit for one ABI bundle.
pub fn core_link_unit_with_bundle(
    bundle: &Arc<lm_abi::AbiBundle>,
) -> Result<Arc<LinkUnit>, String> {
    core_provider_with_bundle(bundle).map(|provider| provider.0)
}

pub(crate) fn core_provider_with_bundle(
    bundle: &Arc<lm_abi::AbiBundle>,
) -> Result<CoreProviderParts, String> {
    let standard = lm_abi::standard_bundle();
    if bundle.digest() == standard.digest() {
        let provider = STANDARD_CORE.get_or_init(|| {
            build_core_provider(&standard).expect("the standard core link unit builds")
        });
        return Ok((Arc::clone(&provider.unit), Arc::clone(&provider.intrinsics)));
    }
    let provider = build_core_provider(bundle)?;
    Ok((provider.unit, provider.intrinsics))
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

fn build_core_provider(bundle: &Arc<lm_abi::AbiBundle>) -> Result<CoreProvider, String> {
    let (module, intrinsics) = lm_hir::core_image_with_intrinsics(bundle.clone());
    lm_verify::verify_module_with_bundle(&module, bundle)
        .map_err(|error| format!("error: the verifier rejected the core: {error}\n"))?;
    let unit = LinkUnit::from_module_with_bundle(CORE_MODULE_PATH, module, Vec::new(), bundle)
        .map_err(|error| format!("error: the core link unit failed: {error}\n"))?;
    Ok(CoreProvider {
        unit: Arc::new(unit),
        intrinsics: intrinsics.into(),
    })
}
