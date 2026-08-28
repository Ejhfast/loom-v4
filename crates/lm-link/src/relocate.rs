//! Unit relocation into append-only arena tables.

use crate::arena::{CodeNamespace, Merged, NamespaceBuild};
use crate::env::{fail, LinkError};
use crate::reloc_tables::{
    reloc_bounds, reloc_class, reloc_conformance, reloc_func, reloc_interface, reloc_row,
    reloc_slot_contract, reloc_slot_target, reloc_type, Reloc,
};
use std::collections::BTreeSet;
use std::sync::Arc;

use lm_bytecode::artifact::{ArtifactId, LinkUnit};
use lm_bytecode::identity::ModuleIdentity;
use lm_bytecode::interface::Interface;
use lm_bytecode::{
    BcClass, BcInterface, Export, Func, Import, ImportKind, Module, SlotSpec, TypeApp, NO_PARENT,
};

pub(crate) fn relocated_exports(module: &Module, reloc: &Reloc) -> Result<Vec<Export>, LinkError> {
    module
        .exports
        .iter()
        .map(|export| {
            let def = if export.kind.is_class() {
                reloc.classes.get(export.def as usize)
            } else if export.kind.is_interface() {
                reloc.interfaces.get(export.def as usize)
            } else {
                reloc.funcs.get(export.def as usize)
            }
            .copied()
            .ok_or_else(|| fail("an export names a missing relocated definition"))?;
            let ctor = if export.ctor == lm_bytecode::NO_CTOR {
                lm_bytecode::NO_CTOR
            } else {
                reloc
                    .funcs
                    .get(export.ctor as usize)
                    .copied()
                    .ok_or_else(|| fail("an export names a missing relocated constructor"))?
            };
            Ok(Export {
                kind: export.kind,
                name: export.name.clone(),
                def,
                ctor,
            })
        })
        .collect()
}

/// Merge one validated unit.
pub(crate) fn merge_unit(
    merged: &mut Merged,
    view: &mut NamespaceBuild,
    unit: &LinkUnit,
    path: &str,
    slot_scope: ArtifactId,
    bundle: &std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Reloc, LinkError> {
    if unit.interface().bundle_digest != bundle.digest() {
        return Err(fail(format!("the module `{path}` uses another ABI bundle")));
    }
    let identity = unit.identity();
    let reloc = relocate(merged, view, unit.module(), identity, path, slot_scope)?;
    bind_unit(view, merged, unit, path, &reloc)?;
    Ok(reloc)
}

/// Bind one relocated unit into one artifact namespace.
pub(crate) fn bind_unit(
    view: &mut NamespaceBuild,
    merged: &Merged,
    unit: &LinkUnit,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let module = unit.module();
    let identity = unit.identity();
    let extern_classes = module.extern_classes();
    for (index, class) in module.classes.iter().enumerate() {
        if extern_classes[index] {
            continue;
        }
        let target = reloc.classes[index];
        let hash = identity.class_hashes[index];
        match view.class_version.get(&class.key) {
            Some((found, _, provider)) if *found != target => {
                return Err(fail(format!(
                    "the class `{}` is provided by `{provider}` and `{path}`",
                    class.key
                )));
            }
            Some(_) => {}
            None => {
                view.class_version
                    .insert(class.key.clone(), (target, hash, path.to_string()));
            }
        }
    }
    for (index, interface) in module.interfaces.iter().enumerate() {
        let target = reloc.interfaces[index];
        match view.interface_by_key.get(&interface.key) {
            Some((found, provider)) if *found != target => {
                return Err(fail(format!(
                    "the interface `{}` is provided by `{provider}` and `{path}`",
                    interface.key
                )));
            }
            Some(_) => {}
            None => {
                view.interface_by_key
                    .insert(interface.key.clone(), (target, path.to_string()));
            }
        }
    }
    view.slot_initials.resize(merged.slots.len(), None);
    for (index, source) in module.slots.iter().enumerate() {
        let target = reloc.slots[index] as usize;
        let initial = source.initial.map(|value| reloc_slot_target(value, reloc));
        match (view.slot_initials[target], initial) {
            (Some(found), Some(wanted)) if found != wanted => {
                return Err(fail(format!(
                    "the slot {index} of `{path}` has another initial target"
                )));
            }
            (None, Some(wanted)) => view.slot_initials[target] = Some(wanted),
            _ => {}
        }
    }
    for (role, source) in module.core_roles.iter().enumerate() {
        if *source == lm_bytecode::NO_ROLE {
            continue;
        }
        let target = reloc
            .classes
            .get(*source as usize)
            .copied()
            .ok_or_else(|| fail(format!("the core role {role} of `{path}` is invalid")))?;
        match view.core_roles[role] {
            lm_bytecode::NO_ROLE => view.core_roles[role] = target,
            found if found == target => {}
            _ => {
                return Err(fail(format!(
                    "the module `{path}` uses another class for core role {role}"
                )));
            }
        }
    }
    merge_bindings(view, module, identity, path, reloc)?;
    register_exports(view, module, unit.interface(), path, reloc)
}

/// Seed type-provider checks from the namespace being extended.
///
/// Function bindings keep their separate base-wins policy. This
/// helper adds no code and performs no verification or relocation.
pub(crate) fn seed_extension_providers(
    view: &mut NamespaceBuild,
    base: &CodeNamespace,
    replaced_paths: &BTreeSet<String>,
) -> Result<(), LinkError> {
    for (path, id) in &base.active_units {
        if replaced_paths.contains(path) {
            continue;
        }
        let unit = base
            .units
            .get(id)
            .ok_or_else(|| fail(format!("the base module `{path}` has no stored unit")))?;
        let reloc = base
            .relocations
            .get(id)
            .ok_or_else(|| fail(format!("the base module `{path}` has no relocation")))?;
        let module = unit.module();
        let extern_classes = module.extern_classes();
        for (index, class) in module.classes.iter().enumerate() {
            if extern_classes[index] {
                continue;
            }
            let hash = unit.identity().class_hashes[index];
            let target = reloc.0.classes[index];
            match view.class_version.get(&class.key) {
                Some((found, _, provider)) if *found != target => {
                    return Err(fail(format!(
                        "the class `{}` is provided by `{provider}` and `{path}`",
                        class.key,
                    )));
                }
                Some(_) => {}
                None => {
                    view.class_version
                        .insert(class.key.clone(), (target, hash, path.clone()));
                }
            }
        }
        for (index, interface) in module.interfaces.iter().enumerate() {
            let target = reloc.0.interfaces[index];
            match view.interface_by_key.get(&interface.key) {
                Some((found, provider)) if *found != target => {
                    return Err(fail(format!(
                        "the interface `{}` is provided by `{provider}` and `{path}`",
                        interface.key
                    )));
                }
                Some(_) => {}
                None => {
                    view.interface_by_key
                        .insert(interface.key.clone(), (target, path.clone()));
                }
            }
        }
    }
    Ok(())
}

/// Relocate one module into the merged tables.
fn relocate(
    merged: &mut Merged,
    view: &NamespaceBuild,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
    slot_scope: ArtifactId,
) -> Result<Reloc, LinkError> {
    let extern_classes = module.extern_classes();
    let strings: Vec<u32> = module.strings.iter().map(|s| merged.string(s)).collect();
    let bytes: Vec<u32> = module
        .bytes
        .iter()
        .map(|value| merged.bytes(value))
        .collect();
    let selectors: Vec<u32> = module
        .selectors
        .iter()
        .map(|s| merged.selector(s))
        .collect();
    // The class map first: a type may name a class, and an imported
    // class resolves to a definition another module provides.
    let extern_funcs = module.extern_funcs();
    let mut classes: Vec<u32> = vec![u32::MAX; module.classes.len()];
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind != ImportKind::Class {
            continue;
        }
        let target = resolve_class_import(view, import, path, idx)?;
        classes[import.def as usize] = target;
    }
    // Types reference only earlier types, so one ascending pass is
    // enough. A class reference of a local class resolves below.
    let mut types: Vec<u32> = vec![u32::MAX; module.types.len()];
    let mut apps: Vec<u32> = vec![u32::MAX; module.apps.len()];
    // The local classes take merged indices in ascending order, so a
    // parent keeps a lower index than its child. A class body needs
    // the type map, and a type may name a class. The indices are
    // therefore assigned first, and the bodies fill after the types
    // relocate.
    let mut created_classes: Vec<u32> = Vec::new();
    for idx in 0..module.classes.len() as u32 {
        if classes[idx as usize] != u32::MAX {
            continue;
        }
        let source = &module.classes[idx as usize];
        let hash = identity.class_hashes[idx as usize];
        match view.class_version.get(&source.key) {
            Some((_, seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the class `{}` arrives with two implementations, from `{provider}` and \
                     from `{path}`; rebuild both against one version",
                    source.key
                )));
            }
            _ => {}
        }
        let at = merged.classes.len() as u32;
        merged.classes.push(BcClass {
            name: source.name.clone(),
            key: source.key.clone(),
            is_final: source.is_final,
            is_frozen: source.is_frozen,
            parent: NO_PARENT,
            parent_args: Vec::new(),
            type_params: source.type_params,
            kind: source.kind,
            fields: Vec::new(),
            field_defaults: Vec::new(),
            own_start: 0,
            has_init: false,
            methods: Vec::new(),
        });
        merged.class_hashes.push(hash);
        merged.class_bounds.push(Vec::new());
        classes[idx as usize] = at;
        created_classes.push(idx);
    }
    // Interface keys are nominal. Assign every merged index before
    // type relocation because a projection names an interface.
    let mut interfaces: Vec<u32> = vec![u32::MAX; module.interfaces.len()];
    let mut created_interfaces: Vec<u32> = Vec::new();
    let mut shared_interfaces: Vec<u32> = Vec::new();
    for (idx, source) in module.interfaces.iter().enumerate() {
        if let Some((existing, _)) = view.interface_by_key.get(&source.key) {
            interfaces[idx] = *existing;
            shared_interfaces.push(idx as u32);
            continue;
        }
        if merged.interfaces.len() > lm_bytecode::MAX_INTERFACE_CALL_INDEX as usize {
            return Err(fail(format!(
                "the linked program has too many interfaces after `{path}`"
            )));
        }
        let at = merged.interfaces.len() as u32;
        merged.interfaces.push(BcInterface {
            name: source.name.clone(),
            key: source.key.clone(),
            type_params: 0,
            effect_params: 0,
            generic_is_effect: Vec::new(),
            parents: Vec::new(),
            type_bounds: Vec::new(),
            associated: Vec::new(),
            methods: Vec::new(),
        });
        merged.interface_hashes.push(identity.interface_hashes[idx]);
        interfaces[idx] = at;
        created_interfaces.push(idx as u32);
    }
    for (idx, ty) in module.types.iter().enumerate() {
        let relocated = reloc_type(ty, &types, &classes, &interfaces);
        types[idx] = merged.ty(relocated, identity.type_hashes[idx])?;
    }
    for (idx, app) in module.apps.iter().enumerate() {
        let relocated = TypeApp {
            types: app.types.iter().map(|t| types[*t as usize]).collect(),
            rows: app.rows.iter().map(|row| reloc_row(row)).collect(),
        };
        apps[idx] = merged.app(relocated);
    }
    let mut reloc = Reloc {
        strings,
        bytes,
        types,
        selectors,
        apps,
        classes,
        interfaces,
        funcs: vec![u32::MAX; module.funcs.len()],
        slots: Vec::with_capacity(module.slots.len()),
    };
    // The function map resolves each imported declaration to one
    // provider definition. Each local function gets one arena entry.
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind == ImportKind::Class {
            continue;
        }
        let target = resolve_func_import(view, merged, module, import, path, idx, &reloc)?;
        reloc.funcs[import.def as usize] = target;
    }
    let mut created_funcs: Vec<u32> = Vec::new();
    for (idx, is_extern) in extern_funcs.iter().copied().enumerate() {
        if is_extern {
            continue;
        }
        let at = merged.funcs.len() as u32;
        // The placeholder keeps the index. The body fills after all
        // function indices are known.
        merged.funcs.push(Func {
            name: module.funcs[idx].name.clone(),
            type_params: 0,
            effect_params: 0,
            params: Vec::new(),
            param_muts: Vec::new(),
            param_names: Vec::new(),
            ret: 0,
            row: Vec::new(),
            captures: Vec::new(),
            local_types: Vec::new(),
            blocks: Vec::new(),
        });
        merged.func_hashes.push(identity.func_hashes[idx]);
        merged.func_bounds.push(Vec::new());
        reloc.funcs[idx] = at;
        created_funcs.push(idx as u32);
    }
    for (idx, import) in module.imports.iter().enumerate() {
        if import.kind == ImportKind::Class {
            check_class_import_contract(merged, module, import, path, idx, &reloc)?;
        }
    }
    for (slot, source) in module.slots.iter().enumerate() {
        let contract = reloc_slot_contract(&source.contract, &reloc);
        let slot_key = (slot_scope, source.key, source.contract_hash);
        let merged_slot = match merged.slot_by_contract.get(&slot_key).copied() {
            Some(existing) => {
                let found = &merged.slots[existing as usize];
                if found.contract_hash != source.contract_hash {
                    return Err(fail(format!(
                        "the slot {slot} of `{path}` has another contract"
                    )));
                }
                existing
            }
            None => {
                let index = merged.slots.len() as u32;
                merged.slots.push(SlotSpec {
                    binding: source.binding.clone(),
                    late: source.late,
                    key: source.key,
                    contract_hash: source.contract_hash,
                    contract,
                    initial: None,
                });
                Arc::make_mut(&mut merged.slot_by_contract).insert(slot_key, index);
                index
            }
        };
        reloc.slots.push(merged_slot);
    }
    // Fill the created definitions, and prove that every shared one
    // really is the definition its hash claims.
    for idx in &created_classes {
        let source = &module.classes[*idx as usize];
        let at = reloc.classes[*idx as usize] as usize;
        let filled = reloc_class(source, &reloc);
        merged
            .classes
            .replace_recent(at, filled)
            .map_err(|_| fail("a new class left its publication chunk"))?;
        let bounds = module
            .class_bounds
            .get(*idx as usize)
            .map(|items| reloc_bounds(items, &reloc))
            .unwrap_or_default();
        merged
            .class_bounds
            .replace_recent(at, bounds)
            .map_err(|_| fail("new class bounds left their publication chunk"))?;
    }
    for idx in created_interfaces.iter().chain(shared_interfaces.iter()) {
        let source = &module.interfaces[*idx as usize];
        let at = reloc.interfaces[*idx as usize] as usize;
        let filled = reloc_interface(source, &reloc);
        if created_interfaces.contains(idx) {
            merged
                .interfaces
                .replace_recent(at, filled)
                .map_err(|_| fail("a new interface left its publication chunk"))?;
        } else if merged.interfaces[at] != filled {
            let provider = &view.interface_by_key[&source.key].1;
            return Err(fail(format!(
                "the interface `{}` arrives with two contracts, from `{provider}` and from `{path}`",
                source.key
            )));
        }
    }
    for idx in &created_funcs {
        let source = &module.funcs[*idx as usize];
        let at = reloc.funcs[*idx as usize] as usize;
        let filled = reloc_func(source, &reloc);
        let bounds = module
            .func_bounds
            .get(*idx as usize)
            .map(|items| reloc_bounds(items, &reloc))
            .unwrap_or_default();
        merged
            .funcs
            .replace_recent(at, filled)
            .map_err(|_| fail("a new function left its publication chunk"))?;
        merged
            .func_bounds
            .replace_recent(at, bounds)
            .map_err(|_| fail("new function bounds left their publication chunk"))?;
    }
    for source in &module.conformances {
        // An imported class carries its provider conformance set as
        // part of its declaration. The contract check compares that
        // set. Only the provider publishes those conformances.
        if extern_classes[source.class as usize] {
            continue;
        }
        let filled = reloc_conformance(source, &reloc);
        if !merged.conformances.contains(&filled) {
            merged.conformances.push(filled);
        }
    }
    let debug = lm_bytecode::debug::decode(&module.debug)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    lm_bytecode::debug::validate(&debug, module)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    Arc::make_mut(&mut merged.debug)
        .append_relocated(&debug, &reloc.funcs, &reloc.classes)
        .map_err(|error| fail(format!("the debug data of `{path}` is invalid: {error}")))?;
    Ok(reloc)
}

/// Merge the named function bindings of one module (specification
/// 3.6). The table is exhaustive:
///
/// | Binding key | StructuralHash | Result |
/// | --- | --- | --- |
/// | same | same | share the binding and the code |
/// | same | different | reject: conflicting providers |
/// | different | same | keep both bindings, share the code |
/// | different | different | keep both bindings and both code objects |
///
/// Row 2 is the rule the generated constructor needs. A class
/// structural hash covers no constructor, so two providers of one
/// class key with two different constructors merge into one class.
/// Their constructors carry one binding key and two structural
/// hashes, and this rule rejects them.
fn merge_bindings(
    view: &mut NamespaceBuild,
    module: &Module,
    identity: &ModuleIdentity,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_funcs = module.extern_funcs();
    for binding in &module.bindings {
        let local = binding.func as usize;
        if local >= module.funcs.len() {
            return Err(fail(format!(
                "the binding `{}` of `{path}` names a function outside the module",
                binding.key
            )));
        }
        if extern_funcs[local] {
            // An imported declaration carries no body, so the module
            // that declares it is not a provider of that name. A
            // The verifier rejects constructor bindings on imports.
            continue;
        }
        let hash = identity.func_hashes[local];
        match view.binding_version.get(&binding.key) {
            Some((seen, provider)) if *seen != hash => {
                return Err(fail(format!(
                    "the function `{}` arrives with two implementations, from \
                     `{provider}` and from `{path}`; rebuild both against one version",
                    binding.key
                )));
            }
            Some(_) => continue,
            None => {
                view.binding_version
                    .insert(binding.key.clone(), (hash, path.to_string()));
            }
        }
        view.bindings.push(lm_bytecode::FuncBinding {
            key: binding.key.clone(),
            func: reloc.funcs[local],
            class: if binding.class == lm_bytecode::NO_CLASS {
                lm_bytecode::NO_CLASS
            } else {
                reloc.classes[binding.class as usize]
            },
        });
    }
    Ok(())
}

/// Resolve one class import slot against the provided definitions.
fn resolve_class_import(
    view: &NamespaceBuild,
    import: &Import,
    path: &str,
    slot: usize,
) -> Result<u32, LinkError> {
    let key = (import.module.clone(), import.name.clone());
    check_pin(view, import, path, slot)?;
    view.class_exports.get(&key).copied().ok_or_else(|| {
        fail(format!(
            "`{path}` slot {slot} names the type `{}.{}`, which the module does \
             not export",
            import.module, import.name
        ))
    })
}

/// Compare the pinned interface hash with the provider export.
fn check_pin(
    view: &NamespaceBuild,
    import: &Import,
    path: &str,
    slot: usize,
) -> Result<(), LinkError> {
    // A method slot pins the interface hash of its class, so the
    // lookup drops the method name.
    let export_name = match import.kind {
        ImportKind::Method => import
            .name
            .rsplit_once('.')
            .map(|(class, _)| class.to_string())
            .unwrap_or_else(|| import.name.clone()),
        _ => import.name.clone(),
    };
    let key = (import.module.clone(), export_name.clone());
    let Some(found) = view.export_hash.get(&key) else {
        return Err(fail(format!(
            "`{path}` slot {slot} names `{}.{export_name}`, which the module does \
             not export",
            import.module
        )));
    };
    if *found != import.hash {
        return Err(fail(format!(
            "`{path}` slot {slot} pins an interface of `{}.{export_name}` that the \
             module no longer provides; rebuild the importing module",
            import.module
        )));
    }
    Ok(())
}

/// Resolve one function, constructor, or method import slot.
fn resolve_func_import(
    view: &NamespaceBuild,
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    reloc: &Reloc,
) -> Result<u32, LinkError> {
    check_pin(view, import, path, slot)?;
    let target = match import.kind {
        ImportKind::Func => {
            let key = (import.module.clone(), import.name.clone());
            view.func_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the function `{}.{}`, which the \
                     module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Ctor => {
            let key = (import.module.clone(), import.name.clone());
            view.ctor_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the constructor of `{}.{}`, which \
                     the module does not export",
                    import.module, import.name
                ))
            })
        }
        ImportKind::Method => {
            let Some((class_name, method)) = import.name.rsplit_once('.') else {
                return Err(fail(format!(
                    "`{path}` slot {slot} names the method `{}` without a class",
                    import.name
                )));
            };
            let key = (import.module.clone(), class_name.to_string());
            let class = view.class_exports.get(&key).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names a method of `{}.{class_name}`, \
                     which the module does not export",
                    import.module
                ))
            })?;
            if method == "init" {
                let key = lm_bytecode::qualified_key(&import.module, &format!("{class_name}.init"));
                let target = view
                    .bindings
                    .iter()
                    .find(|binding| binding.key == key)
                    .map(|binding| binding.func)
                    .ok_or_else(|| {
                        fail(format!(
                            "`{path}` slot {slot} names the initializer of \
                             `{}.{class_name}`, which the module does not provide",
                            import.module
                        ))
                    })?;
                return check_function_import_contract(
                    tables, module, import, path, slot, target, reloc,
                )
                .map(|()| target);
            }
            // The local selector table holds the method name.
            module
                .selectors
                .iter()
                .position(|s| s == method)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         the module does not call"
                    ))
                })?;
            let selector = tables.selector_index.get(method).copied().ok_or_else(|| {
                fail(format!(
                    "`{path}` slot {slot} names the unknown method `{method}`"
                ))
            })?;
            tables.classes[class as usize]
                .methods
                .iter()
                .find(|(sel, _)| *sel == selector)
                .map(|(_, func)| *func)
                .ok_or_else(|| {
                    fail(format!(
                        "`{path}` slot {slot} names the method `{method}`, which \
                         `{}.{class_name}` does not answer",
                        import.module
                    ))
                })
        }
        ImportKind::Class => unreachable!("a class slot never reaches the function map"),
    }?;
    check_function_import_contract(tables, module, import, path, slot, target, reloc)?;
    Ok(target)
}

/// Compare one sparse callable declaration with its provider.
fn check_function_import_contract(
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    target: u32,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let source = module
        .funcs
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no function declaration")))?;
    let found = tables
        .funcs
        .get(target as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no provider function")))?;
    let source_bounds = module
        .func_bounds
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no function bounds")))?;
    let params: Vec<u32> = source
        .params
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let captures: Vec<u32> = source
        .captures
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let matches = source.type_params == found.type_params
        && source.effect_params == found.effect_params
        && reloc_bounds(source_bounds, reloc) == tables.func_bounds[target as usize]
        && params == found.params
        && source.param_muts == found.param_muts
        && source.param_names == found.param_names
        && reloc.types[source.ret as usize] == found.ret
        && reloc_row(&source.row) == found.row
        && captures == found.captures;
    if !matches {
        return Err(fail(format!(
            "`{path}` slot {slot} declares another callable contract for `{}.{}`",
            import.module, import.name
        )));
    }
    Ok(())
}

/// Compare one sparse class declaration with its provider.
fn check_class_import_contract(
    tables: &Merged,
    module: &Module,
    import: &Import,
    path: &str,
    slot: usize,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let source = module
        .classes
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no class declaration")))?;
    let target_index = reloc.classes[import.def as usize];
    let found = tables
        .classes
        .get(target_index as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no provider class")))?;
    let parent = source
        .parent()
        .map(|parent| reloc.classes[parent as usize])
        .unwrap_or(NO_PARENT);
    let parent_args: Vec<u32> = source
        .parent_args
        .iter()
        .map(|ty| reloc.types[*ty as usize])
        .collect();
    let fields: Vec<(String, u32)> = source
        .fields
        .iter()
        .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
        .collect();
    let bounds = module
        .class_bounds
        .get(import.def as usize)
        .ok_or_else(|| fail(format!("`{path}` slot {slot} has no class bounds")))?;
    let layout_matches = source.name == found.name
        && source.key == found.key
        && source.is_final == found.is_final
        && source.is_frozen == found.is_frozen
        && parent == found.parent
        && parent_args == found.parent_args
        && source.type_params == found.type_params
        && source.kind == found.kind
        && fields == found.fields
        && source.field_defaults == found.field_defaults
        && source.own_start == found.own_start
        && source.has_init == found.has_init
        && reloc_bounds(bounds, reloc) == tables.class_bounds[target_index as usize];
    let methods_match = source.methods.iter().all(|(selector, function)| {
        let method = (
            reloc.selectors[*selector as usize],
            reloc.funcs[*function as usize],
        );
        found.methods.contains(&method)
    });
    let conformances_match = module
        .conformances
        .iter()
        .filter(|item| item.class == import.def)
        .map(|item| reloc_conformance(item, reloc))
        .all(|item| tables.conformances.contains(&item));
    if !(layout_matches && methods_match && conformances_match) {
        return Err(fail(format!(
            "`{path}` slot {slot} declares another class contract for `{}.{}`",
            import.module, import.name
        )));
    }
    Ok(())
}

/// Record the exports of one module for the modules that follow.
fn register_exports(
    view: &mut NamespaceBuild,
    module: &Module,
    interface: &Interface,
    path: &str,
    reloc: &Reloc,
) -> Result<(), LinkError> {
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    for export in &module.exports {
        let key = (path.to_string(), export.name.clone());
        if view.export_hash.contains_key(&key) {
            return Err(fail(format!(
                "the module `{path}` exports the name `{}` twice",
                export.name
            )));
        }
        // The decoder bounds these indices, and a hand-built module
        // reaches the linker without a decoder, so the bound is
        // checked here too.
        let limit = if export.kind.is_class() {
            reloc.classes.len()
        } else if export.kind.is_interface() {
            reloc.interfaces.len()
        } else {
            reloc.funcs.len()
        };
        if export.def as usize >= limit
            || (export.ctor != lm_bytecode::NO_CTOR && export.ctor as usize >= reloc.funcs.len())
        {
            return Err(fail(format!(
                "the export `{}` of `{path}` names a definition outside the \
                 module",
                export.name
            )));
        }
        // A module exports what it defines. A re-export of an
        // imported declaration would give one definition two
        // qualified names, and a pin would then name a module that
        // does not hold the definition.
        let imported = if export.kind.is_class() {
            extern_classes[export.def as usize]
        } else if export.kind.is_interface() {
            false
        } else {
            extern_funcs[export.def as usize]
        };
        if imported {
            return Err(fail(format!(
                "the module `{path}` exports `{}`, which it imports",
                export.name
            )));
        }
        let entry = interface.find(&export.name).ok_or_else(|| {
            fail(format!(
                "the interface of `{path}` does not describe the export `{}`",
                export.name
            ))
        })?;
        view.export_hash.insert(key.clone(), entry.iface_hash);
        if export.kind.is_class() {
            view.class_exports
                .insert(key.clone(), reloc.classes[export.def as usize]);
            if export.ctor != lm_bytecode::NO_CTOR {
                view.ctor_exports
                    .insert(key, reloc.funcs[export.ctor as usize]);
            }
        } else if export.kind.is_interface() {
            view.interface_exports
                .insert(key, reloc.interfaces[export.def as usize]);
        } else {
            view.func_exports
                .insert(key, reloc.funcs[export.def as usize]);
        }
    }
    Ok(())
}
