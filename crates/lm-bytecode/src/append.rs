//! Append-only relocation for verified linked modules.
//!
//! Runtime installation keeps every existing table index. It merges
//! equal immutable definitions and appends each new definition.

use crate::identity::module_identity;
use crate::{
    BcAssociated, BcCallableContract, BcClass, BcConformance, BcInterface, BcInterfaceMethod,
    BcInterfaceUse, BcRow, BcType, ExtendedInstr, Func, Instr, Module, SlotContract, SlotSpec,
    SlotTarget, TypeApp, NO_PARENT, NO_ROLE,
};
use std::collections::HashMap;

/// The table relocation of one appended module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendReloc {
    pub strings: Vec<u32>,
    pub types: Vec<u32>,
    pub selectors: Vec<u32>,
    pub apps: Vec<u32>,
    pub classes: Vec<u32>,
    pub interfaces: Vec<u32>,
    pub funcs: Vec<u32>,
    pub slots: Vec<u32>,
}

/// The result of one append-only relocation.
#[derive(Debug, Clone)]
pub struct AppendResult {
    pub module: Module,
    pub reloc: AppendReloc,
    /// Relocated initial targets from the appended artifact.
    pub slot_initials: Vec<Option<SlotTarget>>,
}

/// One resolved import target in the current VM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedImport {
    Class(u32),
    Function(u32),
}

fn fail(message: impl Into<String>) -> String {
    format!("append error: {}", message.into())
}

/// Append one linked module without changing any existing index.
pub fn append_linked(base: &Module, addition: &Module) -> Result<AppendResult, String> {
    if !base.imports.is_empty() || !addition.imports.is_empty() {
        return Err(fail("both modules must have resolved imports"));
    }
    append_resolved(base, addition, &[])
}

/// Append one module after its imports resolve against the base.
pub fn append_resolved(
    base: &Module,
    addition: &Module,
    imports: &[ResolvedImport],
) -> Result<AppendResult, String> {
    if !base.imports.is_empty() {
        return Err(fail("the base module must have resolved imports"));
    }
    if imports.len() != addition.imports.len() {
        return Err(fail("the import target count differs from the module"));
    }
    let base_identity = module_identity(base).map_err(|error| fail(error.to_string()))?;
    let add_identity = module_identity(addition).map_err(|error| fail(error.to_string()))?;
    let mut merged = base.clone();
    merged.exports.clear();

    let mut string_index: HashMap<String, u32> = merged
        .strings
        .iter()
        .enumerate()
        .map(|(index, value)| (value.clone(), index as u32))
        .collect();
    let mut selector_index: HashMap<String, u32> = merged
        .selectors
        .iter()
        .enumerate()
        .map(|(index, value)| (value.clone(), index as u32))
        .collect();
    let strings: Vec<u32> = addition
        .strings
        .iter()
        .map(|value| intern(&mut merged.strings, &mut string_index, value.clone()))
        .collect();
    let selectors: Vec<u32> = addition
        .selectors
        .iter()
        .map(|value| intern(&mut merged.selectors, &mut selector_index, value.clone()))
        .collect();

    let mut class_index: HashMap<(String, [u8; 32]), u32> = merged
        .classes
        .iter()
        .enumerate()
        .map(|(index, class)| {
            (
                (class.key.clone(), base_identity.class_hashes[index]),
                index as u32,
            )
        })
        .collect();
    let mut classes = vec![u32::MAX; addition.classes.len()];
    for (import, target) in addition.imports.iter().zip(imports) {
        if import.kind != crate::ImportKind::Class {
            continue;
        }
        let ResolvedImport::Class(target) = *target else {
            return Err(fail("a class import resolved to a function"));
        };
        if target as usize >= base.classes.len() {
            return Err(fail("a class import target is outside the base module"));
        }
        let slot = classes
            .get_mut(import.def as usize)
            .ok_or_else(|| fail("a class import declaration is outside the module"))?;
        if *slot != u32::MAX && *slot != target {
            return Err(fail("one class import declaration has two targets"));
        }
        *slot = target;
    }
    let mut new_classes = Vec::new();
    let mut shared_classes = Vec::new();
    for (index, class) in addition.classes.iter().enumerate() {
        if classes[index] != u32::MAX {
            continue;
        }
        let key = (class.key.clone(), add_identity.class_hashes[index]);
        if let Some(existing) = class_index.get(&key).copied() {
            classes[index] = existing;
            shared_classes.push(index);
        } else {
            let target = merged.classes.len() as u32;
            merged.classes.push(BcClass {
                name: class.name.clone(),
                key: class.key.clone(),
                is_final: class.is_final,
                parent: NO_PARENT,
                parent_args: Vec::new(),
                type_params: class.type_params,
                kind: class.kind,
                fields: Vec::new(),
                methods: Vec::new(),
            });
            merged.class_bounds.push(Vec::new());
            class_index.insert(key, target);
            classes[index] = target;
            new_classes.push(index);
        }
    }

    let mut interface_index: HashMap<String, u32> = merged
        .interfaces
        .iter()
        .enumerate()
        .map(|(index, item)| (item.key.clone(), index as u32))
        .collect();
    let mut interfaces = vec![u32::MAX; addition.interfaces.len()];
    let mut new_interfaces = Vec::new();
    let mut shared_interfaces = Vec::new();
    for (index, item) in addition.interfaces.iter().enumerate() {
        if let Some(existing) = interface_index.get(&item.key).copied() {
            interfaces[index] = existing;
            shared_interfaces.push(index);
        } else {
            let target = merged.interfaces.len() as u32;
            merged.interfaces.push(BcInterface {
                name: item.name.clone(),
                key: item.key.clone(),
                type_params: 0,
                effect_params: 0,
                generic_is_effect: Vec::new(),
                type_bounds: Vec::new(),
                associated: Vec::new(),
                methods: Vec::new(),
            });
            interface_index.insert(item.key.clone(), target);
            interfaces[index] = target;
            new_interfaces.push(index);
        }
    }

    let mut type_index: HashMap<BcType, u32> = merged
        .types
        .iter()
        .enumerate()
        .map(|(index, ty)| (ty.clone(), index as u32))
        .collect();
    let mut types = vec![u32::MAX; addition.types.len()];
    for (index, ty) in addition.types.iter().enumerate() {
        let relocated = reloc_type(ty, &types, &classes, &interfaces, &strings);
        types[index] = intern(&mut merged.types, &mut type_index, relocated);
    }
    let mut app_index: HashMap<TypeApp, u32> = merged
        .apps
        .iter()
        .enumerate()
        .map(|(index, app)| (app.clone(), index as u32))
        .collect();
    let mut apps = Vec::with_capacity(addition.apps.len());
    for app in &addition.apps {
        let relocated = TypeApp {
            types: app.types.iter().map(|ty| types[*ty as usize]).collect(),
            rows: app
                .rows
                .iter()
                .map(|row| reloc_row(row, &strings))
                .collect(),
        };
        apps.push(intern(&mut merged.apps, &mut app_index, relocated));
    }

    let mut function_index: HashMap<[u8; 32], u32> = merged
        .funcs
        .iter()
        .enumerate()
        .map(|(index, _)| (base_identity.func_hashes[index], index as u32))
        .collect();
    let mut funcs = vec![u32::MAX; addition.funcs.len()];
    for (import, target) in addition.imports.iter().zip(imports) {
        if import.kind == crate::ImportKind::Class {
            continue;
        }
        let ResolvedImport::Function(target) = *target else {
            return Err(fail("a function import resolved to a class"));
        };
        if target as usize >= base.funcs.len() {
            return Err(fail("a function import target is outside the base module"));
        }
        let slot = funcs
            .get_mut(import.def as usize)
            .ok_or_else(|| fail("a function import declaration is outside the module"))?;
        if *slot != u32::MAX && *slot != target {
            return Err(fail("one function import declaration has two targets"));
        }
        *slot = target;
    }
    let mut new_funcs = Vec::new();
    let mut shared_funcs = Vec::new();
    for (index, func) in addition.funcs.iter().enumerate() {
        if funcs[index] != u32::MAX {
            continue;
        }
        let hash = add_identity.func_hashes[index];
        if let Some(existing) = function_index.get(&hash).copied() {
            funcs[index] = existing;
            shared_funcs.push(index);
        } else {
            let target = merged.funcs.len() as u32;
            merged.funcs.push(Func {
                name: func.name.clone(),
                type_params: 0,
                effect_params: 0,
                params: Vec::new(),
                param_muts: Vec::new(),
                ret: 0,
                row: Vec::new(),
                captures: Vec::new(),
                local_types: Vec::new(),
                blocks: Vec::new(),
            });
            merged.func_bounds.push(Vec::new());
            function_index.insert(hash, target);
            funcs[index] = target;
            new_funcs.push(index);
        }
    }

    let mut slot_index: HashMap<[u8; 32], u32> = merged
        .slots
        .iter()
        .enumerate()
        .map(|(index, slot)| (slot.key, index as u32))
        .collect();
    let mut reloc = AppendReloc {
        strings,
        types,
        selectors,
        apps,
        classes,
        interfaces,
        funcs,
        slots: Vec::with_capacity(addition.slots.len()),
    };
    let mut slot_initials = Vec::with_capacity(addition.slots.len());
    for (index, slot) in addition.slots.iter().enumerate() {
        let contract = reloc_slot_contract(&slot.contract, &reloc);
        let initial = slot.initial.map(|target| reloc_slot_target(target, &reloc));
        let target = if let Some(existing) = slot_index.get(&slot.key).copied() {
            if merged.slots[existing as usize].contract != contract {
                return Err(fail(format!(
                    "slot {index} has a contract that conflicts with its key"
                )));
            }
            existing
        } else {
            let target = merged.slots.len() as u32;
            merged.slots.push(SlotSpec {
                key: slot.key,
                contract,
                initial,
            });
            slot_index.insert(slot.key, target);
            target
        };
        reloc.slots.push(target);
        slot_initials.push(initial);
    }

    fill_definitions(
        &mut merged,
        addition,
        &reloc,
        &new_classes,
        &shared_classes,
        &new_interfaces,
        &shared_interfaces,
        &new_funcs,
        &shared_funcs,
    )?;
    for conformance in &addition.conformances {
        let relocated = reloc_conformance(conformance, &reloc);
        if !merged.conformances.contains(&relocated) {
            merged.conformances.push(relocated);
        }
    }
    for (role, source) in addition.core_roles.iter().enumerate() {
        if *source == NO_ROLE {
            continue;
        }
        let target = reloc.classes[*source as usize];
        if merged.core_roles[role] == NO_ROLE {
            merged.core_roles[role] = target;
        } else if merged.core_roles[role] != target {
            return Err(fail(format!("core role {role} has another class")));
        }
    }
    let extern_funcs = addition.extern_funcs();
    for binding in &addition.bindings {
        if extern_funcs[binding.func as usize] {
            continue;
        }
        let relocated = crate::FuncBinding {
            key: binding.key.clone(),
            func: reloc.funcs[binding.func as usize],
            class: if binding.class == crate::NO_CLASS {
                crate::NO_CLASS
            } else {
                reloc.classes[binding.class as usize]
            },
        };
        if !merged.bindings.contains(&relocated) {
            merged.bindings.push(relocated);
        }
    }
    Ok(AppendResult {
        module: merged,
        reloc,
        slot_initials,
    })
}

fn intern<T: Clone + Eq + std::hash::Hash>(
    values: &mut Vec<T>,
    index: &mut HashMap<T, u32>,
    value: T,
) -> u32 {
    if let Some(found) = index.get(&value) {
        return *found;
    }
    let found = values.len() as u32;
    values.push(value.clone());
    index.insert(value, found);
    found
}

#[allow(clippy::too_many_arguments)]
fn fill_definitions(
    merged: &mut Module,
    source: &Module,
    reloc: &AppendReloc,
    new_classes: &[usize],
    shared_classes: &[usize],
    new_interfaces: &[usize],
    shared_interfaces: &[usize],
    new_funcs: &[usize],
    shared_funcs: &[usize],
) -> Result<(), String> {
    for index in new_classes.iter().chain(shared_classes) {
        let class = reloc_class(&source.classes[*index], reloc);
        let target = reloc.classes[*index] as usize;
        let bounds = reloc_bounds(&source.class_bounds[*index], reloc);
        if new_classes.contains(index) {
            merged.classes[target] = class;
            merged.class_bounds[target] = bounds;
        }
    }
    for index in new_interfaces.iter().chain(shared_interfaces) {
        let interface = reloc_interface(&source.interfaces[*index], reloc);
        let target = reloc.interfaces[*index] as usize;
        if new_interfaces.contains(index) {
            merged.interfaces[target] = interface;
        } else if merged.interfaces[target] != interface {
            return Err(fail("one nominal interface has another contract"));
        }
    }
    for index in new_funcs.iter().chain(shared_funcs) {
        let func = reloc_func(&source.funcs[*index], reloc);
        let bounds = reloc_bounds(&source.func_bounds[*index], reloc);
        let target = reloc.funcs[*index] as usize;
        if new_funcs.contains(index) {
            merged.funcs[target] = func;
            merged.func_bounds[target] = bounds;
        }
    }
    Ok(())
}

fn reloc_row(row: &[BcRow], strings: &[u32]) -> Vec<BcRow> {
    row.iter()
        .map(|item| match item {
            BcRow::Op(index) => BcRow::Op(strings[*index as usize]),
            BcRow::Var(index) => BcRow::Var(*index),
        })
        .collect()
}

fn reloc_type(
    ty: &BcType,
    types: &[u32],
    classes: &[u32],
    interfaces: &[u32],
    strings: &[u32],
) -> BcType {
    match ty {
        BcType::Class(class) => BcType::Class(classes[*class as usize]),
        BcType::Inst(class, args) => BcType::Inst(
            classes[*class as usize],
            args.iter().map(|arg| types[*arg as usize]).collect(),
        ),
        BcType::List(element) => BcType::List(types[*element as usize]),
        BcType::Map(key, value) => BcType::Map(types[*key as usize], types[*value as usize]),
        BcType::Tuple(items) => {
            BcType::Tuple(items.iter().map(|item| types[*item as usize]).collect())
        }
        BcType::Fn(params, muts, ret, row) => BcType::Fn(
            params.iter().map(|param| types[*param as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row, strings),
        ),
        BcType::Callback(params, muts, ret, row) => BcType::Callback(
            params.iter().map(|param| types[*param as usize]).collect(),
            muts.clone(),
            types[*ret as usize],
            reloc_row(row, strings),
        ),
        BcType::Projection {
            base,
            interface,
            assoc,
        } => BcType::Projection {
            base: types[*base as usize],
            interface: interfaces[*interface as usize],
            assoc: *assoc,
        },
        BcType::Run(result) => BcType::Run(types[*result as usize]),
        BcType::Wait(result) => BcType::Wait(types[*result as usize]),
        BcType::RunSnapshot(result) => BcType::RunSnapshot(types[*result as usize]),
        BcType::PendingCall(args, reply) => {
            BcType::PendingCall(types[*args as usize], types[*reply as usize])
        }
        BcType::Handle(message, result) => {
            BcType::Handle(types[*message as usize], types[*result as usize])
        }
        BcType::Op(op, function) => BcType::Op(*op, types[*function as usize]),
        other => other.clone(),
    }
}

fn reloc_interface_use(source: &BcInterfaceUse, reloc: &AppendReloc) -> BcInterfaceUse {
    BcInterfaceUse {
        interface: reloc.interfaces[source.interface as usize],
        types: source
            .types
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        rows: source
            .rows
            .iter()
            .map(|row| reloc_row(row, &reloc.strings))
            .collect(),
    }
}

fn reloc_bounds(source: &[Vec<BcInterfaceUse>], reloc: &AppendReloc) -> Vec<Vec<BcInterfaceUse>> {
    source
        .iter()
        .map(|items| {
            items
                .iter()
                .map(|item| reloc_interface_use(item, reloc))
                .collect()
        })
        .collect()
}

fn reloc_callable(source: &BcCallableContract, reloc: &AppendReloc) -> BcCallableContract {
    BcCallableContract {
        type_params: source.type_params,
        effect_params: source.effect_params,
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        params: source
            .params
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        param_muts: source.param_muts.clone(),
        ret: reloc.types[source.ret as usize],
        row: reloc_row(&source.row, &reloc.strings),
    }
}

fn reloc_slot_contract(source: &SlotContract, reloc: &AppendReloc) -> SlotContract {
    match source {
        SlotContract::Function(contract) => SlotContract::Function(reloc_callable(contract, reloc)),
        SlotContract::Method(contract) => SlotContract::Method(reloc_callable(contract, reloc)),
        SlotContract::Class {
            type_params,
            abi,
            ty,
            constructor,
        } => SlotContract::Class {
            type_params: *type_params,
            abi: *abi,
            ty: reloc.types[*ty as usize],
            constructor: reloc_callable(constructor, reloc),
        },
        SlotContract::Value { ty } => SlotContract::Value {
            ty: reloc.types[*ty as usize],
        },
        SlotContract::Process { message, result } => SlotContract::Process {
            message: reloc.types[*message as usize],
            result: reloc.types[*result as usize],
        },
    }
}

fn reloc_slot_target(source: SlotTarget, reloc: &AppendReloc) -> SlotTarget {
    match source {
        SlotTarget::Function(function) => SlotTarget::Function(reloc.funcs[function as usize]),
        SlotTarget::Class { class, constructor } => SlotTarget::Class {
            class: reloc.classes[class as usize],
            constructor: reloc.funcs[constructor as usize],
        },
    }
}

fn reloc_class(source: &BcClass, reloc: &AppendReloc) -> BcClass {
    BcClass {
        name: source.name.clone(),
        key: source.key.clone(),
        is_final: source.is_final,
        parent: source
            .parent()
            .map(|parent| reloc.classes[parent as usize])
            .unwrap_or(NO_PARENT),
        parent_args: source
            .parent_args
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        type_params: source.type_params,
        kind: source.kind,
        fields: source
            .fields
            .iter()
            .map(|(name, ty)| (name.clone(), reloc.types[*ty as usize]))
            .collect(),
        methods: source
            .methods
            .iter()
            .map(|(selector, function)| {
                (
                    reloc.selectors[*selector as usize],
                    reloc.funcs[*function as usize],
                )
            })
            .collect(),
    }
}

fn reloc_interface(source: &BcInterface, reloc: &AppendReloc) -> BcInterface {
    BcInterface {
        name: source.name.clone(),
        key: source.key.clone(),
        type_params: source.type_params,
        effect_params: source.effect_params,
        generic_is_effect: source.generic_is_effect.clone(),
        type_bounds: reloc_bounds(&source.type_bounds, reloc),
        associated: source
            .associated
            .iter()
            .map(|item| BcAssociated {
                name: item.name.clone(),
                bound: item
                    .bound
                    .as_ref()
                    .map(|bound| reloc_interface_use(bound, reloc)),
            })
            .collect(),
        methods: source
            .methods
            .iter()
            .map(|method| BcInterfaceMethod {
                selector: reloc.selectors[method.selector as usize],
                mut_self: method.mut_self,
                params: method
                    .params
                    .iter()
                    .map(|ty| reloc.types[*ty as usize])
                    .collect(),
                param_muts: method.param_muts.clone(),
                ret: reloc.types[method.ret as usize],
                row: reloc_row(&method.row, &reloc.strings),
            })
            .collect(),
    }
}

fn reloc_conformance(source: &BcConformance, reloc: &AppendReloc) -> BcConformance {
    BcConformance {
        class: reloc.classes[source.class as usize],
        application: reloc_interface_use(&source.application, reloc),
        associated: source
            .associated
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
    }
}

fn reloc_func(source: &Func, reloc: &AppendReloc) -> Func {
    Func {
        name: source.name.clone(),
        type_params: source.type_params,
        effect_params: source.effect_params,
        params: source
            .params
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        param_muts: source.param_muts.clone(),
        ret: reloc.types[source.ret as usize],
        row: reloc_row(&source.row, &reloc.strings),
        captures: source
            .captures
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        local_types: source
            .local_types
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        blocks: source
            .blocks
            .iter()
            .map(|block| {
                block
                    .iter()
                    .map(|instruction| reloc_instr(instruction, reloc))
                    .collect()
            })
            .collect(),
    }
}

fn reloc_instr(instruction: &Instr, reloc: &AppendReloc) -> Instr {
    match instruction {
        Instr::ConstStr(index) => Instr::ConstStr(reloc.strings[*index as usize]),
        Instr::Call(function) => Instr::Call(reloc.funcs[*function as usize]),
        Instr::CallG { func, app } => Instr::CallG {
            func: reloc.funcs[*func as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::CallVirtual { selector, argc } => Instr::CallVirtual {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
        },
        Instr::CallVirtualG {
            selector,
            argc,
            app,
        } => Instr::CallVirtualG {
            selector: reloc.selectors[*selector as usize],
            argc: *argc,
            app: reloc.apps[*app as usize],
        },
        Instr::MakeClosure { func, captures } => Instr::MakeClosure {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        Instr::Perform { op, argc, reply_ty } => Instr::Perform {
            op: *op,
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        Instr::PerformValue { argc, reply_ty } => Instr::PerformValue {
            argc: *argc,
            reply_ty: reloc.types[*reply_ty as usize],
        },
        Instr::New(class) => Instr::New(reloc.classes[*class as usize]),
        Instr::NewG { class, app } => Instr::NewG {
            class: reloc.classes[*class as usize],
            app: reloc.apps[*app as usize],
        },
        Instr::TupleNew { ty, count } => Instr::TupleNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::ListNew { ty, count } => Instr::ListNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::MapNew { ty, count } => Instr::MapNew {
            ty: reloc.types[*ty as usize],
            count: *count,
        },
        Instr::IsType(ty) => Instr::IsType(reloc.types[*ty as usize]),
        Instr::CastType(ty) => Instr::CastType(reloc.types[*ty as usize]),
        Instr::MapPut { ty, discard } => Instr::MapPut {
            ty: reloc.types[*ty as usize],
            discard: *discard,
        },
        Instr::Digest { ty } => Instr::Digest {
            ty: reloc.types[*ty as usize],
        },
        Instr::AsCall { op, ty } => Instr::AsCall {
            op: *op,
            ty: reloc.types[*ty as usize],
        },
        Instr::CallInterface {
            interface,
            method,
            recv_ty,
        } => Instr::CallInterface {
            interface: reloc.interfaces[*interface as usize],
            method: *method,
            recv_ty: reloc.types[*recv_ty as usize],
        },
        Instr::Extended(instruction) => Instr::Extended(reloc_extended(instruction, reloc)),
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::LoadLocal(_)
        | Instr::StoreLocal(_)
        | Instr::Pop
        | Instr::Add
        | Instr::Sub
        | Instr::Mul
        | Instr::Div
        | Instr::Rem
        | Instr::Neg
        | Instr::Not
        | Instr::LtInt
        | Instr::LeInt
        | Instr::GtInt
        | Instr::GeInt
        | Instr::EqInt
        | Instr::NeInt
        | Instr::EqBool
        | Instr::NeBool
        | Instr::EqRef
        | Instr::EqValue
        | Instr::NeValue
        | Instr::NeRef
        | Instr::CallValue { .. }
        | Instr::LoadCapture(_)
        | Instr::LoadField(_)
        | Instr::StoreField(_)
        | Instr::TupleGet(_)
        | Instr::ListLen
        | Instr::ListAt
        | Instr::ListPush
        | Instr::MapLen
        | Instr::MapHas
        | Instr::MapAt
        | Instr::Freeze
        | Instr::EqDigest
        | Instr::NeDigest
        | Instr::Jump(_)
        | Instr::JumpIfFalse(_)
        | Instr::JumpIfTrue(_)
        | Instr::Return
        | Instr::OpConst(_)
        | Instr::TableEdit { .. }
        | Instr::CallArgs
        | Instr::FaultCode
        | Instr::FaultDenied
        | Instr::RequestOp
        | Instr::Unreachable
        | Instr::Native(_) => *instruction,
    }
}

fn reloc_extended(instruction: &ExtendedInstr, reloc: &AppendReloc) -> ExtendedInstr {
    match instruction {
        ExtendedInstr::MakeCallback { func, captures } => ExtendedInstr::MakeCallback {
            func: reloc.funcs[*func as usize],
            captures: *captures,
        },
        ExtendedInstr::FunctionCode { func } => ExtendedInstr::FunctionCode {
            func: reloc.funcs[*func as usize],
        },
        ExtendedInstr::ClassCode { class } => ExtendedInstr::ClassCode {
            class: reloc.classes[*class as usize],
        },
        ExtendedInstr::OptionSome { ty } => ExtendedInstr::OptionSome {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionNone { ty } => ExtendedInstr::OptionNone {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::OptionPayload { ty } => ExtendedInstr::OptionPayload {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::ListGet { ty } => ExtendedInstr::ListGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapGet { ty } => ExtendedInstr::MapGet {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::ListPop { ty } => ExtendedInstr::ListPop {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::MapRemove { ty } => ExtendedInstr::MapRemove {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::DynPack { ty } => ExtendedInstr::DynPack {
            ty: reloc.types[*ty as usize],
        },
        ExtendedInstr::CallSlot { slot, app } => ExtendedInstr::CallSlot {
            slot: reloc.slots[*slot as usize],
            app: reloc_app(*app, reloc),
        },
        ExtendedInstr::NewSlot { slot, app } => ExtendedInstr::NewSlot {
            slot: reloc.slots[*slot as usize],
            app: reloc_app(*app, reloc),
        },
        ExtendedInstr::LoadSlot { slot } => ExtendedInstr::LoadSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::SendSlot { slot } => ExtendedInstr::SendSlot {
            slot: reloc.slots[*slot as usize],
        },
        ExtendedInstr::AsCallback
        | ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::MapEpoch
        | ExtendedInstr::MapIterLen
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::ListCapacity
        | ExtendedInstr::ListSet
        | ExtendedInstr::ListInsert
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListReserve
        | ExtendedInstr::ListTruncate
        | ExtendedInstr::ListContains
        | ExtendedInstr::ListReorder
        | ExtendedInstr::MapClear
        | ExtendedInstr::MapReserve
        | ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::DynRender
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::SyntaxToTree => *instruction,
    }
}

fn reloc_app(app: u32, reloc: &AppendReloc) -> u32 {
    if app == crate::NO_APP {
        crate::NO_APP
    } else {
        reloc.apps[app as usize]
    }
}
