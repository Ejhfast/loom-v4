//! Build the canonical core link unit.

use crate::LinkEnv;
use lm_bytecode::artifact::{LinkUnit, CORE_MODULE_PATH};
use std::sync::{Arc, OnceLock};

struct CoreProvider {
    unit: Arc<LinkUnit>,
    intrinsics: Arc<[Option<lm_abi::IntrinsicSlot>]>,
}

static STANDARD_CORE: OnceLock<Result<CoreProvider, String>> = OnceLock::new();

const PINNED_CORE: &[u8] = include_bytes!("../../../core/pinned-core.lmbc");
const PINNED_INTRINSICS: &[u8] = include_bytes!("../../../core/pinned-core-intrinsics.bin");

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
        let provider = STANDARD_CORE.get_or_init(|| load_pinned_core_provider(&standard));
        return match provider {
            Ok(provider) => Ok((Arc::clone(&provider.unit), Arc::clone(&provider.intrinsics))),
            Err(error) => Err(error.clone()),
        };
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

fn load_pinned_core_provider(bundle: &Arc<lm_abi::AbiBundle>) -> Result<CoreProvider, String> {
    // The pin generator verifies these bytes before it writes them.
    // The core-image gate compares them with one fresh verified build.
    let module = lm_bytecode::decode_with_bundle(PINNED_CORE, bundle)
        .map_err(|error| format!("error: the pinned core did not decode: {error}\n"))?;
    let intrinsics = decode_pinned_intrinsics(PINNED_INTRINSICS, module.funcs.len())?;
    let unit = LinkUnit::from_module_with_bundle(CORE_MODULE_PATH, module, Vec::new(), bundle)
        .map_err(|error| format!("error: the pinned core link unit failed: {error}\n"))?;
    Ok(CoreProvider {
        unit: Arc::new(unit),
        intrinsics,
    })
}

fn decode_pinned_intrinsics(
    bytes: &[u8],
    function_count: usize,
) -> Result<Arc<[Option<lm_abi::IntrinsicSlot>]>, String> {
    if !bytes.len().is_multiple_of(4) || bytes.len() / 4 > function_count {
        return Err("error: the pinned core intrinsic table has another length\n".to_string());
    }
    let mut intrinsics = Vec::with_capacity(bytes.len() / 4);
    for encoded in bytes.chunks_exact(4) {
        let &[a, b, c, d] = encoded else {
            return Err("error: the pinned core intrinsic table is malformed\n".to_string());
        };
        let slot = u32::from_le_bytes([a, b, c, d]);
        if slot == u32::MAX {
            intrinsics.push(None);
        } else if slot < lm_abi::INTRINSIC_COUNT {
            intrinsics.push(Some(slot));
        } else {
            return Err("error: the pinned core intrinsic table has an invalid slot\n".to_string());
        }
    }
    Ok(intrinsics.into())
}
