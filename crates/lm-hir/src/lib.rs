//! Typed HIR, type checking, and CFG lowering.
//!
//! `check` uses the pinned core source to create typed HIR. `lower`
//! turns typed HIR into basic-block bytecode. `dump_cfg` renders the
//! result for humans.

pub mod check;
mod checkfn;
pub mod exhaust;
pub mod hir;
pub mod import;
pub mod lower;

pub use check::{check_module, check_module_with, CheckOptions, CORE_SOURCE};

/// The digest of the pinned core sources.
///
/// Every module pins the core interface. A core edit can therefore
/// change every artifact. The build cache keys on this digest.
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
    let bundle = lm_abi::standard_bundle();
    core_image_with_bundle(bundle)
}

/// Compile the core provider under one ABI bundle.
pub fn core_image_with_bundle(bundle: std::sync::Arc<lm_abi::AbiBundle>) -> lm_bytecode::Module {
    let empty = lm_source::parse::parse("").expect("the empty module parses");
    let mut hir = check_module_with(
        &empty,
        CheckOptions {
            prelude: false,
            build_core_provider: true,
            bundle,
            ..CheckOptions::default()
        },
    )
    .expect("the core image checks");
    let exports = std::mem::take(&mut hir.core_exports);
    hir.exports = exports;
    lower_module(&hir)
}

/// Replace checked core bodies with exact import declarations.
pub fn externalize_core(
    hir: &mut HirModule,
    interface: &lm_bytecode::interface::Interface,
) -> Result<(), String> {
    use lm_bytecode::{ImportKind, CORE_MODULE};

    if interface.module_path != CORE_MODULE {
        return Err("the core interface has another module path".to_string());
    }
    let mut imports = Vec::new();
    let mut methods = std::collections::BTreeSet::new();
    for (class_index, class) in hir.classes.iter().enumerate() {
        if !class.key.starts_with("core.") {
            continue;
        }
        let export = interface
            .find(&class.name)
            .ok_or_else(|| format!("the core exports no class `{}`", class.name))?;
        if !export.kind.is_class() {
            return Err(format!("the core export `{}` is not a class", class.name));
        }
        imports.push(hir::HirImport {
            module: CORE_MODULE.to_string(),
            name: class.name.clone(),
            kind: ImportKind::Class,
            def: hir::HirImportDef::Class(class_index as u32),
            hash: export.iface_hash,
        });
        imports.push(hir::HirImport {
            module: CORE_MODULE.to_string(),
            name: class.name.clone(),
            kind: ImportKind::Ctor,
            def: hir::HirImportDef::Ctor(class_index as u32),
            hash: export.iface_hash,
        });
        for (name, function) in &class.methods {
            methods.insert(*function);
            imports.push(hir::HirImport {
                module: CORE_MODULE.to_string(),
                name: format!("{}.{name}", class.name),
                kind: ImportKind::Method,
                def: hir::HirImportDef::Func(*function),
                hash: export.iface_hash,
            });
        }
    }
    let mut core_ordinal = 0u32;
    for (index, function) in hir.funcs.iter().enumerate() {
        if !function.core {
            continue;
        }
        let ordinal = core_ordinal;
        core_ordinal += 1;
        if methods.contains(&(index as u32)) {
            continue;
        }
        let name = interface
            .find(&function.name)
            .filter(|export| export.kind == lm_bytecode::ExportKind::Function)
            .map(|_| function.name.clone())
            .unwrap_or_else(|| format!("$internal.function.{ordinal}"));
        let provider = interface
            .find(&name)
            .ok_or_else(|| format!("core function {index} has no provider export"))?;
        imports.push(hir::HirImport {
            module: CORE_MODULE.to_string(),
            name,
            kind: ImportKind::Func,
            def: hir::HirImportDef::Func(index as u32),
            hash: provider.iface_hash,
        });
    }
    for class in &mut hir.classes {
        if class.key.starts_with("core.") {
            class.imported = true;
        }
    }
    for function in &mut hir.funcs {
        if function.core {
            function.imported = true;
        }
    }
    hir.bindings
        .retain(|binding| !binding.key.starts_with("core."));
    hir.imports.extend(imports);
    Ok(())
}
