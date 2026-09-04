//! Dependency collection for one artifact unit.
//!
//! The collector starts from an entry or an exported surface.
//! It keeps unresolved imports for the artifact linker.
//! It compacts each table in its original order.

use crate::reloc_tables::{
    reloc_bounds, reloc_class, reloc_conformance, reloc_func, reloc_interface, reloc_reflection,
    reloc_row, reloc_slot, reloc_type, Reloc,
};
use lm_bytecode::debug::{DebugCodeOrigin, DebugDefinition, DebugFunction, DebugInfo};
use lm_bytecode::{
    BcCallableContract, BcClass, BcConformance, BcInterface, BcInterfaceUse, BcRow, BcType, Export,
    ExtendedInstr, Func, Import, ImportKind, Instr, Module, SlotContract, SlotSpec, SlotTarget,
    TypeApp, NO_APP, NO_CLASS, NO_CTOR, NO_FUNC, NO_REFLECTION_DEF, NO_ROLE,
};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy)]
struct Offsets {
    strings: usize,
    bytes: usize,
    types: usize,
    selectors: usize,
    apps: usize,
    interfaces: usize,
    classes: usize,
    funcs: usize,
    slots: usize,
    reflections: usize,
    total: usize,
}

impl Offsets {
    fn new(module: &Module) -> Offsets {
        let strings = 0;
        let bytes = strings + module.strings.len();
        let types = bytes + module.bytes.len();
        let selectors = types + module.types.len();
        let apps = selectors + module.selectors.len();
        let interfaces = apps + module.apps.len();
        let classes = interfaces + module.interfaces.len();
        let funcs = classes + module.classes.len();
        let slots = funcs + module.funcs.len();
        let reflections = slots + module.slots.len();
        let total = reflections + module.reflections.len();
        Offsets {
            strings,
            bytes,
            types,
            selectors,
            apps,
            interfaces,
            classes,
            funcs,
            slots,
            reflections,
            total,
        }
    }

    fn node(base: usize, index: u32) -> u32 {
        (base + index as usize) as u32
    }

    fn string(self, index: u32) -> u32 {
        Self::node(self.strings, index)
    }

    fn bytes(self, index: u32) -> u32 {
        Self::node(self.bytes, index)
    }

    fn ty(self, index: u32) -> u32 {
        Self::node(self.types, index)
    }

    fn selector(self, index: u32) -> u32 {
        Self::node(self.selectors, index)
    }

    fn app(self, index: u32) -> u32 {
        Self::node(self.apps, index)
    }

    fn interface(self, index: u32) -> u32 {
        Self::node(self.interfaces, index)
    }

    fn class(self, index: u32) -> u32 {
        Self::node(self.classes, index)
    }

    fn func(self, index: u32) -> u32 {
        Self::node(self.funcs, index)
    }

    fn slot(self, index: u32) -> u32 {
        Self::node(self.slots, index)
    }

    fn reflection(self, index: u32) -> u32 {
        Self::node(self.reflections, index)
    }
}

/// Collect one verified program with no unresolved imports.
#[cfg(test)]
pub(crate) fn collect_program(module: &Module) -> Result<Module, String> {
    if !module.imports.is_empty() {
        return Err("dependency collection needs a program with resolved imports".to_string());
    }
    if module.entry as usize >= module.funcs.len() {
        return Err("dependency collection received an invalid entry".to_string());
    }
    let offsets = Offsets::new(module);
    if offsets.total > u32::MAX as usize {
        return Err("dependency collection has too many table entries".to_string());
    }
    let roots = [offsets.func(module.entry)];
    collect_from_roots(module, offsets, &roots, false, &[])
}

/// Collect one root module before the linker resolves its imports.
pub(crate) fn collect_link_root(module: &Module) -> Result<Module, String> {
    if module.entry as usize >= module.funcs.len() {
        return Err("dependency collection received an invalid entry".to_string());
    }
    let offsets = Offsets::new(module);
    if offsets.total > u32::MAX as usize {
        return Err("dependency collection has too many table entries".to_string());
    }
    let roots = [offsets.func(module.entry)];
    collect_from_roots(module, offsets, &roots, true, &[])
}

/// Collect one compiled module and keep its complete local surface.
pub(crate) fn collect_compiled_unit(module: &Module) -> Result<Module, String> {
    if module.entry as usize >= module.funcs.len() {
        return Err("dependency collection received an invalid entry".to_string());
    }
    let offsets = Offsets::new(module);
    if offsets.total > u32::MAX as usize {
        return Err("dependency collection has too many table entries".to_string());
    }
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    let mut roots = vec![offsets.func(module.entry)];
    for export in &module.exports {
        if export.kind.is_class() {
            checked_class_export(module, &extern_classes, export)?;
            roots.push(offsets.class(export.def));
            if export.ctor != NO_CTOR {
                roots.push(offsets.func(export.ctor));
            }
        } else if export.kind == lm_bytecode::ExportKind::Function {
            if export.def as usize >= module.funcs.len() {
                return Err(format!(
                    "the export `{}` names a function outside the module",
                    export.name
                ));
            }
            if extern_funcs[export.def as usize] {
                return Err(format!(
                    "the module exports `{}`, which it imports",
                    export.name
                ));
            }
            roots.push(offsets.func(export.def));
        } else if export.kind.is_constant() {
            let constant = export
                .constant
                .as_ref()
                .ok_or_else(|| format!("the constant export `{}` has no value", export.name))?;
            if constant.ty as usize >= module.types.len() {
                return Err(format!(
                    "the constant export `{}` names a type outside the module",
                    export.name
                ));
            }
            roots.push(offsets.ty(constant.ty));
        }
    }
    roots.sort_unstable();
    roots.dedup();
    collect_from_roots(module, offsets, &roots, true, &module.exports)
}

/// One definition selected as an artifact root.
#[derive(Debug, Clone, Copy)]
pub(crate) enum DefinitionRoot {
    Function(u32),
    Class(u32),
}

/// Collect one definition and the small artifact entry.
pub(crate) fn collect_link_definition(
    module: &Module,
    root: DefinitionRoot,
    export: Export,
) -> Result<Module, String> {
    if module.entry as usize >= module.funcs.len() {
        return Err("dependency collection received an invalid entry".to_string());
    }
    let offsets = Offsets::new(module);
    if offsets.total > u32::MAX as usize {
        return Err("dependency collection has too many table entries".to_string());
    }
    let root = match root {
        DefinitionRoot::Function(function) => {
            if function as usize >= module.funcs.len() {
                return Err("the portable function is outside its module".to_string());
            }
            offsets.func(function)
        }
        DefinitionRoot::Class(class) => {
            if class as usize >= module.classes.len() {
                return Err("the portable class is outside its module".to_string());
            }
            offsets.class(class)
        }
    };
    collect_from_roots(module, offsets, &[root], true, &[export])
}

/// Collect the exports requested from one provider module.
pub(crate) fn collect_link_exports(
    module: &Module,
    requests: &[(String, ImportKind)],
) -> Result<Module, String> {
    let offsets = Offsets::new(module);
    if offsets.total > u32::MAX as usize {
        return Err("dependency collection has too many table entries".to_string());
    }
    let mut roots = Vec::with_capacity(requests.len());
    let mut exports = HashSet::with_capacity(requests.len());
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    for (name, kind) in requests {
        let export_name = provider_export_name(name, *kind);
        let mut matches = module
            .exports
            .iter()
            .filter(|export| export.name == export_name);
        let export = matches
            .next()
            .ok_or_else(|| format!("the module does not export `{export_name}`"))?;
        if matches.next().is_some() {
            return Err(format!("the module exports `{export_name}` twice"));
        }
        exports.insert(export_name.to_string());
        match kind {
            ImportKind::Class => {
                if !export.kind.is_class() {
                    return Err(format!("the export `{export_name}` is not a class"));
                }
                checked_class_export(module, &extern_classes, export)?;
                roots.push(offsets.class(export.def));
                if export.ctor != NO_CTOR {
                    roots.push(offsets.func(export.ctor));
                }
            }
            ImportKind::Ctor => {
                if !export.kind.is_class() || export.ctor == NO_CTOR {
                    return Err(format!("the export `{export_name}` has no constructor"));
                }
                checked_class_export(module, &extern_classes, export)?;
                roots.push(offsets.func(export.ctor));
            }
            ImportKind::Method => {
                if !export.kind.is_class() {
                    return Err(format!("the export `{export_name}` is not a class"));
                }
                checked_class_export(module, &extern_classes, export)?;
                roots.push(offsets.class(export.def));
                if export.ctor != NO_CTOR {
                    roots.push(offsets.func(export.ctor));
                }
            }
            ImportKind::Func => {
                if export.kind != lm_bytecode::ExportKind::Function {
                    return Err(format!("the export `{export_name}` is not a function"));
                }
                if export.def as usize >= module.funcs.len() {
                    return Err(format!(
                        "the export `{export_name}` names a function outside the module"
                    ));
                }
                if extern_funcs[export.def as usize] {
                    return Err(format!(
                        "the module exports `{export_name}`, which it imports"
                    ));
                }
                roots.push(offsets.func(export.def));
            }
            ImportKind::Constant => {
                if !export.kind.is_constant() {
                    return Err(format!("the export `{export_name}` is not a constant"));
                }
                let constant = export
                    .constant
                    .as_ref()
                    .ok_or_else(|| format!("the constant export `{export_name}` has no value"))?;
                if constant.ty as usize >= module.types.len() {
                    return Err(format!(
                        "the constant export `{export_name}` names a type outside the module"
                    ));
                }
                roots.push(offsets.ty(constant.ty));
            }
        }
    }
    roots.sort_unstable();
    roots.dedup();
    let exports: Vec<Export> = module
        .exports
        .iter()
        .filter(|export| exports.contains(&export.name))
        .cloned()
        .collect();
    collect_from_roots(module, offsets, &roots, true, &exports)
}

fn checked_class_export(
    module: &Module,
    extern_classes: &[bool],
    export: &Export,
) -> Result<(), String> {
    if export.def as usize >= module.classes.len()
        || (export.ctor != NO_CTOR && export.ctor as usize >= module.funcs.len())
    {
        return Err(format!(
            "the export `{}` names a definition outside the module",
            export.name
        ));
    }
    if extern_classes[export.def as usize] {
        return Err(format!(
            "the module exports `{}`, which it imports",
            export.name
        ));
    }
    Ok(())
}

fn provider_export_name(name: &str, kind: ImportKind) -> &str {
    if kind == ImportKind::Method {
        name.rsplit_once('.')
            .map(|(class, _)| class)
            .unwrap_or(name)
    } else {
        name
    }
}

fn collect_from_roots(
    module: &Module,
    offsets: Offsets,
    roots: &[u32],
    keep_imports: bool,
    exports: &[Export],
) -> Result<Module, String> {
    if roots.is_empty() {
        return Err("dependency collection received no roots".to_string());
    }
    let graph = dependency_graph(module, offsets);
    let mut canonical_roots = Vec::with_capacity(roots.len() + 5);
    canonical_roots.extend_from_slice(roots);
    canonical_roots.push(offsets.func(module.entry));
    canonical_roots.extend((0..module.types.len().min(4) as u32).map(|index| offsets.ty(index)));
    let extern_classes = module.extern_classes();
    let live = loop {
        let (_, component_of) =
            lm_scc::components_from_roots(offsets.total, &graph, &canonical_roots);
        let mut added = Vec::new();
        for (index, class) in module.classes.iter().enumerate() {
            if !extern_classes[index]
                || component_of[offsets.class(index as u32) as usize] == u32::MAX
            {
                continue;
            }
            for (selector, function) in &class.methods {
                if component_of[offsets.selector(*selector) as usize] != u32::MAX
                    && component_of[offsets.func(*function) as usize] == u32::MAX
                {
                    added.push(offsets.func(*function));
                }
            }
        }
        if added.is_empty() {
            break component_of
                .iter()
                .map(|component| *component != u32::MAX)
                .collect::<Vec<_>>();
        }
        canonical_roots.extend(added);
        canonical_roots.sort_unstable();
        canonical_roots.dedup();
    };
    let reloc = Reloc::from_live(module, offsets, &live);
    relocate_module(module, &reloc, keep_imports, exports)
}

fn dependency_graph(module: &Module, offsets: Offsets) -> Vec<Vec<u32>> {
    let mut graph = vec![Vec::new(); offsets.total];
    let extern_classes = module.extern_classes();
    let type_index: HashMap<BcType, u32> = module
        .types
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, ty)| (ty, index as u32))
        .collect();
    for (index, ty) in module.types.iter().enumerate() {
        let edges = &mut graph[offsets.ty(index as u32) as usize];
        type_edges(module, offsets, ty, edges);
        if let BcType::Fn(params, muts, ret, row) = ty {
            let callback = BcType::Callback(params.clone(), muts.clone(), *ret, row.clone());
            if let Some(callback) = type_index.get(&callback) {
                edges.push(offsets.ty(*callback));
            }
        }
    }
    for (index, app) in module.apps.iter().enumerate() {
        let edges = &mut graph[offsets.app(index as u32) as usize];
        type_list_edges(offsets, &app.types, edges);
        nominal_role_edges(module, offsets, &app.types, edges);
        rows_edges(offsets, &app.rows, edges);
    }
    for (index, interface) in module.interfaces.iter().enumerate() {
        let edges = &mut graph[offsets.interface(index as u32) as usize];
        interface_edges(module, offsets, interface, edges);
    }
    for (index, class) in module.classes.iter().enumerate() {
        let edges = &mut graph[offsets.class(index as u32) as usize];
        class_edges(module, offsets, index, class, !extern_classes[index], edges);
    }
    for conformance in &module.conformances {
        let edges = &mut graph[offsets.class(conformance.class) as usize];
        conformance_edges(module, offsets, conformance, edges);
    }
    for binding in &module.bindings {
        if binding.class != NO_CLASS {
            graph[offsets.class(binding.class) as usize].push(offsets.func(binding.func));
        }
    }
    for import in &module.imports {
        if import.kind == ImportKind::Constant {
            continue;
        }
        if matches!(import.kind, ImportKind::Ctor | ImportKind::Method) {
            let class_name = if import.kind == ImportKind::Method {
                import
                    .name
                    .rsplit_once('.')
                    .map_or(import.name.as_str(), |(class, _)| class)
            } else {
                import.name.as_str()
            };
            if let Some(class) = module.imports.iter().find(|candidate| {
                candidate.kind == ImportKind::Class
                    && candidate.module == import.module
                    && candidate.name == class_name
            }) {
                graph[offsets.func(import.def) as usize].push(offsets.class(class.def));
            }
        }
        if import.kind == ImportKind::Class
            && module
                .classes
                .get(import.def as usize)
                .is_some_and(|class| class.has_init)
        {
            let ctor = module.imports.iter().find(|candidate| {
                candidate.kind == ImportKind::Ctor
                    && candidate.module == import.module
                    && candidate.name == import.name
            });
            if let Some(ctor) = ctor {
                graph[offsets.class(import.def) as usize].push(offsets.func(ctor.def));
            }
        }
        if import.kind != ImportKind::Method {
            continue;
        }
        let method = import
            .name
            .rsplit_once('.')
            .map(|(_, method)| method)
            .unwrap_or(&import.name);
        if let Some(selector) = module.selectors.iter().position(|name| name == method) {
            graph[offsets.func(import.def) as usize].push(offsets.selector(selector as u32));
        }
    }
    for (child, class) in module.classes.iter().enumerate() {
        let Some(parent) = class.parent() else {
            continue;
        };
        if class.kind == lm_bytecode::BcClassKind::Case
            && module.classes[parent as usize].kind == lm_bytecode::BcClassKind::Abstract
        {
            graph[offsets.class(parent) as usize].push(offsets.class(child as u32));
        }
    }
    for (index, func) in module.funcs.iter().enumerate() {
        let edges = &mut graph[offsets.func(index as u32) as usize];
        func_edges(module, offsets, &type_index, index as u32, func, edges);
        if func.name.starts_with("<late new ") {
            for instruction in func.blocks.iter().flatten() {
                if let Instr::Extended(ExtendedInstr::NewSlot { slot, .. }) = instruction {
                    // Runtime class installation deduplicates this dispatcher.
                    // Keep it with the class slot that gives it meaning.
                    graph[offsets.slot(*slot) as usize].push(offsets.func(index as u32));
                }
            }
        }
    }
    for (index, slot) in module.slots.iter().enumerate() {
        let edges = &mut graph[offsets.slot(index as u32) as usize];
        slot_edges(offsets, slot, edges);
        match slot.initial {
            Some(SlotTarget::Function(function)) => {
                graph[offsets.func(function) as usize].push(offsets.slot(index as u32));
            }
            Some(SlotTarget::Class { class, constructor }) => {
                graph[offsets.class(class) as usize].push(offsets.slot(index as u32));
                graph[offsets.func(constructor) as usize].push(offsets.slot(index as u32));
            }
            None => {}
        }
    }
    for (index, reflection) in module.reflections.iter().enumerate() {
        let edges = &mut graph[offsets.reflection(index as u32) as usize];
        for declaration in &reflection.declarations {
            match declaration.kind {
                lm_bytecode::ExportKind::Function => {
                    edges.push(offsets.func(declaration.def));
                }
                lm_bytecode::ExportKind::Class | lm_bytecode::ExportKind::Enum => {
                    edges.push(offsets.class(declaration.def));
                }
                lm_bytecode::ExportKind::Interface => {
                    edges.push(offsets.interface(declaration.def));
                }
                lm_bytecode::ExportKind::Constant => {
                    let constant = declaration
                        .constant
                        .as_ref()
                        .expect("the verifier checks reflected constants");
                    edges.push(offsets.ty(constant.ty));
                }
                lm_bytecode::ExportKind::EnumCase => unreachable!("the verifier rejects cases"),
            }
            if declaration.callable != NO_REFLECTION_DEF {
                edges.push(offsets.func(declaration.callable));
            }
        }
    }
    for roles in lm_bytecode::corepin::ROLE_FAMILIES {
        core_role_family_edges(module, offsets, roles, &mut graph);
    }
    graph
}

fn core_role_family_edges(
    module: &Module,
    offsets: Offsets,
    roles: &[usize],
    graph: &mut [Vec<u32>],
) {
    let members: Vec<u32> = roles
        .iter()
        .map(|role| module.core_roles[*role])
        .filter(|class| *class != NO_ROLE)
        .map(|class| offsets.class(class))
        .collect();
    for member in &members {
        graph[*member as usize].extend(members.iter().copied());
    }
}

fn type_edges(_module: &Module, offsets: Offsets, ty: &BcType, edges: &mut Vec<u32>) {
    match ty {
        BcType::Unit
        | BcType::Never
        | BcType::Bool
        | BcType::Int
        | BcType::Float
        | BcType::Str
        | BcType::Bytes
        | BcType::FileHandle => {}
        BcType::Class(class) => edges.push(offsets.class(*class)),
        BcType::Inst(class, args) => {
            edges.push(offsets.class(*class));
            type_list_edges(offsets, args, edges);
        }
        BcType::List(element) => {
            edges.push(offsets.ty(*element));
        }
        BcType::Map(key, value) => {
            edges.push(offsets.ty(*key));
            edges.push(offsets.ty(*value));
        }
        BcType::Tuple(items) => {
            type_list_edges(offsets, items, edges);
        }
        BcType::Fn(params, _, ret, row) | BcType::Callback(params, _, ret, row) => {
            type_list_edges(offsets, params, edges);
            edges.push(offsets.ty(*ret));
            row_edges(offsets, row, edges);
        }
        BcType::Projection {
            base, interface, ..
        } => {
            edges.push(offsets.ty(*base));
            edges.push(offsets.interface(*interface));
        }
        BcType::Run(result) | BcType::Wait(result) | BcType::RunSnapshot(result) => {
            edges.push(offsets.ty(*result));
        }
        BcType::PendingCall(args, reply) | BcType::Handle(args, reply) => {
            edges.push(offsets.ty(*args));
            edges.push(offsets.ty(*reply));
        }
        BcType::Op(_, function) => edges.push(offsets.ty(*function)),
        BcType::Var(_)
        | BcType::Fault
        | BcType::Request
        | BcType::PolicyTable
        | BcType::Vm
        | BcType::Digest
        | BcType::VmSnapshot
        | BcType::ResourceHandle
        | BcType::HostResource => {}
    }
}

/// Keep nominal views needed for generic conformance checks.
fn nominal_role_edges(module: &Module, offsets: Offsets, roots: &[u32], edges: &mut Vec<u32>) {
    let mut work = roots.to_vec();
    let mut seen = HashSet::new();
    while let Some(index) = work.pop() {
        if !seen.insert(index) {
            continue;
        }
        match &module.types[index as usize] {
            BcType::Unit => core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_UNIT, edges),
            BcType::Bool => core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_BOOL, edges),
            BcType::Int => core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_INT, edges),
            BcType::Float => {
                core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_FLOAT, edges)
            }
            BcType::Str => {
                core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_STRING, edges)
            }
            BcType::Bytes => {
                core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_BYTES, edges)
            }
            BcType::FileHandle => core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_FILE_HANDLE,
                edges,
            ),
            BcType::List(element) => {
                core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_LIST, edges);
                work.push(*element);
            }
            BcType::Map(key, value) => {
                core_role_edge(module, offsets, lm_bytecode::corepin::ROLE_MAP, edges);
                work.push(*key);
                work.push(*value);
            }
            BcType::Tuple(items) => {
                if let Some(role) = lm_bytecode::corepin::tuple_role(items.len()) {
                    core_role_edge(module, offsets, role, edges);
                }
                work.extend(items);
            }
            BcType::Inst(_, args) => work.extend(args),
            _ => {}
        }
    }
}

fn core_role_edge(module: &Module, offsets: Offsets, role: usize, edges: &mut Vec<u32>) {
    let class = module.core_roles[role];
    if class != NO_ROLE {
        edges.push(offsets.class(class));
    }
}

fn type_list_edges(offsets: Offsets, types: &[u32], edges: &mut Vec<u32>) {
    edges.extend(types.iter().map(|ty| offsets.ty(*ty)));
}

fn row_edges(_offsets: Offsets, _row: &[BcRow], _edges: &mut Vec<u32>) {}

fn rows_edges(offsets: Offsets, rows: &[Vec<BcRow>], edges: &mut Vec<u32>) {
    for row in rows {
        row_edges(offsets, row, edges);
    }
}

fn interface_use_edges(offsets: Offsets, item: &BcInterfaceUse, edges: &mut Vec<u32>) {
    edges.push(offsets.interface(item.interface));
    type_list_edges(offsets, &item.types, edges);
    rows_edges(offsets, &item.rows, edges);
}

fn bounds_edges(offsets: Offsets, bounds: &[Vec<BcInterfaceUse>], edges: &mut Vec<u32>) {
    for group in bounds {
        for item in group {
            interface_use_edges(offsets, item, edges);
        }
    }
}

fn callable_edges(offsets: Offsets, callable: &BcCallableContract, edges: &mut Vec<u32>) {
    bounds_edges(offsets, &callable.type_bounds, edges);
    type_list_edges(offsets, &callable.params, edges);
    edges.push(offsets.ty(callable.ret));
    row_edges(offsets, &callable.row, edges);
}

fn interface_edges(
    module: &Module,
    offsets: Offsets,
    interface: &BcInterface,
    edges: &mut Vec<u32>,
) {
    for parent in &interface.parents {
        interface_use_edges(offsets, parent, edges);
    }
    bounds_edges(offsets, &interface.type_bounds, edges);
    for associated in &interface.associated {
        for bound in &associated.bounds {
            interface_use_edges(offsets, bound, edges);
        }
    }
    for method in &interface.methods {
        edges.push(offsets.selector(method.selector));
        bounds_edges(offsets, &method.type_bounds, edges);
        for premise in &method.premises {
            edges.push(offsets.ty(premise.subject));
            nominal_role_edges(module, offsets, &[premise.subject], edges);
            for bound in &premise.bounds {
                interface_use_edges(offsets, bound, edges);
            }
        }
        type_list_edges(offsets, &method.params, edges);
        edges.push(offsets.ty(method.ret));
        row_edges(offsets, &method.row, edges);
        if method.default != NO_FUNC {
            edges.push(offsets.func(method.default));
        }
    }
}

fn class_edges(
    module: &Module,
    offsets: Offsets,
    index: usize,
    class: &BcClass,
    keep_all_methods: bool,
    edges: &mut Vec<u32>,
) {
    if let Some(parent) = class.parent() {
        edges.push(offsets.class(parent));
    }
    type_list_edges(offsets, &class.parent_args, edges);
    for (_, ty) in &class.fields {
        edges.push(offsets.ty(*ty));
    }
    if keep_all_methods {
        for (selector, func) in &class.methods {
            edges.push(offsets.selector(*selector));
            edges.push(offsets.func(*func));
        }
    }
    if let Some(bounds) = module.class_bounds.get(index) {
        bounds_edges(offsets, bounds, edges);
    }
}

fn conformance_edges(
    module: &Module,
    offsets: Offsets,
    item: &BcConformance,
    edges: &mut Vec<u32>,
) {
    interface_use_edges(offsets, &item.application, edges);
    for premise in &item.premises {
        for bound in &premise.bounds {
            interface_use_edges(offsets, bound, edges);
        }
    }
    type_list_edges(offsets, &item.associated, edges);
    nominal_role_edges(module, offsets, &item.associated, edges);
}

fn func_edges(
    module: &Module,
    offsets: Offsets,
    type_index: &HashMap<BcType, u32>,
    index: u32,
    func: &Func,
    edges: &mut Vec<u32>,
) {
    type_list_edges(offsets, &func.params, edges);
    edges.push(offsets.ty(func.ret));
    row_edges(offsets, &func.row, edges);
    type_list_edges(offsets, &func.captures, edges);
    type_list_edges(offsets, &func.local_types, edges);
    if let Some(bounds) = module.func_bounds.get(index as usize) {
        bounds_edges(offsets, bounds, edges);
    }
    for instruction in func.blocks.iter().flatten() {
        instruction_edges(module, offsets, type_index, instruction, edges);
    }
}

fn instruction_edges(
    module: &Module,
    offsets: Offsets,
    type_index: &HashMap<BcType, u32>,
    instruction: &Instr,
    edges: &mut Vec<u32>,
) {
    match instruction {
        Instr::ConstStr(index) => edges.push(offsets.string(*index)),
        Instr::ConstBytes(index) => edges.push(offsets.bytes(*index)),
        Instr::ConstRegex(index) => {
            edges.push(offsets.string(*index));
            role_edges(module, offsets, &["Regex"], edges);
        }
        Instr::ConstChar(_) => role_edges(module, offsets, &["Char"], edges),
        Instr::Call(function) => edges.push(offsets.func(*function)),
        Instr::CallG { func, app } => {
            edges.push(offsets.func(*func));
            edges.push(offsets.app(*app));
        }
        Instr::CallVirtual { selector, .. } => {
            edges.push(offsets.selector(*selector));
            virtual_receiver_edges(module, offsets, *selector, edges);
        }
        Instr::CallVirtualG { selector, app, .. } => {
            edges.push(offsets.selector(*selector));
            edges.push(offsets.app(*app));
            virtual_receiver_edges(module, offsets, *selector, edges);
        }
        Instr::MakeClosure { func, .. } => {
            edges.push(offsets.func(*func));
            callable_type_edge(module, offsets, type_index, *func, false, edges);
        }
        Instr::Perform { reply_ty, .. } | Instr::PerformValue { reply_ty, .. } => {
            edges.push(offsets.ty(*reply_ty));
        }
        Instr::New(class) => edges.push(offsets.class(*class)),
        Instr::NewG { class, app } => {
            edges.push(offsets.class(*class));
            edges.push(offsets.app(*app));
        }
        Instr::MapNew { ty, .. } => {
            edges.push(offsets.ty(*ty));
            nominal_role_edges(module, offsets, &[*ty], edges);
        }
        Instr::TupleNew { ty, .. }
        | Instr::ListNew { ty, .. }
        | Instr::IsType(ty)
        | Instr::CastType(ty)
        | Instr::MapPut { ty, .. }
        | Instr::AsCall { ty, .. } => edges.push(offsets.ty(*ty)),
        Instr::Digest { ty } => {
            edges.push(offsets.ty(*ty));
            if let Some(digest) = type_index.get(&BcType::Digest) {
                edges.push(offsets.ty(*digest));
            }
        }
        Instr::CallInterface { site, recv_ty, app } => {
            let (interface, _) = lm_bytecode::unpack_interface_call_site(*site);
            edges.push(offsets.interface(interface));
            edges.push(offsets.ty(*recv_ty));
            nominal_role_edges(module, offsets, &[*recv_ty], edges);
            if *app != NO_APP {
                edges.push(offsets.app(*app));
            }
        }
        Instr::Native(instruction) => {
            native_edges(module, offsets, instruction, edges);
            if matches!(instruction, lm_bytecode::NativeInstr::BytesNew) {
                if let Some(bytes) = type_index.get(&BcType::Bytes) {
                    edges.push(offsets.ty(*bytes));
                }
            }
        }
        Instr::Numeric(instruction) => numeric_edges(module, offsets, instruction, edges),
        Instr::Extended(instruction) => {
            extended_edges(module, offsets, type_index, instruction, edges)
        }
        Instr::ConstUnit
        | Instr::ConstBool(_)
        | Instr::ConstInt(_)
        | Instr::ConstFloat(_)
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
        | Instr::RequestOp
        | Instr::FaultDenied
        | Instr::RaiseUserPanic
        | Instr::RaiseAssertionFailed
        | Instr::RaiseFault
        | Instr::Unreachable
        | Instr::EqValue
        | Instr::NeValue => {}
    }
}

fn virtual_receiver_edges(module: &Module, offsets: Offsets, selector: u32, edges: &mut Vec<u32>) {
    for role in [
        lm_bytecode::corepin::ROLE_INT,
        lm_bytecode::corepin::ROLE_FLOAT,
        lm_bytecode::corepin::ROLE_BOOL,
        lm_bytecode::corepin::ROLE_STRING,
        lm_bytecode::corepin::ROLE_BYTES,
        lm_bytecode::corepin::ROLE_FILE_HANDLE,
    ] {
        let class = module.core_roles[role];
        if class == NO_ROLE {
            continue;
        }
        let answers = module.classes[class as usize]
            .methods
            .iter()
            .any(|(method, _)| *method == selector);
        if answers {
            edges.push(offsets.class(class));
        }
    }
}

fn role_edges(module: &Module, offsets: Offsets, labels: &[&str], edges: &mut Vec<u32>) {
    for label in labels {
        if let Some(role) = lm_bytecode::corepin::role_index(label) {
            core_role_edge(module, offsets, role, edges);
        }
    }
}

fn native_edges(
    module: &Module,
    offsets: Offsets,
    instruction: &lm_bytecode::NativeInstr,
    edges: &mut Vec<u32>,
) {
    use lm_bytecode::NativeInstr;
    match instruction {
        NativeInstr::EqStr
        | NativeInstr::NeStr
        | NativeInstr::StrByteLen
        | NativeInstr::StrCharCount
        | NativeInstr::StrConcat
        | NativeInstr::StrStartsWith
        | NativeInstr::StrEndsWith
        | NativeInstr::StrContains
        | NativeInstr::StrFindIndex
        | NativeInstr::TextFindByteIndex
        | NativeInstr::TextTrim
        | NativeInstr::TextTrimStart
        | NativeInstr::TextTrimEnd
        | NativeInstr::TextToLowerAscii
        | NativeInstr::TextToUpperAscii
        | NativeInstr::TextReplace
        | NativeInstr::TextParseIntStatus
        | NativeInstr::TextParseIntValue
        | NativeInstr::TextPadStart
        | NativeInstr::TextPadEnd
        | NativeInstr::TextSplit
        | NativeInstr::TextLines
        | NativeInstr::TextSlice
        | NativeInstr::TextIsBoundary
        | NativeInstr::TextSliceBytes
        | NativeInstr::TextBytes
        | NativeInstr::TextLt
        | NativeInstr::TextLe
        | NativeInstr::TextGt
        | NativeInstr::TextGe
        | NativeInstr::TextHash => role_edges(module, offsets, &["Text"], edges),
        NativeInstr::TextAt | NativeInstr::TextAtByte => {
            role_edges(module, offsets, &["Text", "Char"], edges)
        }
        NativeInstr::TextToString => role_edges(module, offsets, &["Text"], edges),
        NativeInstr::RegexCompileStatus => role_edges(module, offsets, &["Text"], edges),
        NativeInstr::RegexCompileValue => role_edges(module, offsets, &["Text", "Regex"], edges),
        NativeInstr::RegexSource
        | NativeInstr::RegexIsMatch
        | NativeInstr::RegexCount
        | NativeInstr::RegexReplaceAll => role_edges(module, offsets, &["Regex", "Text"], edges),
        NativeInstr::RegexSplit => {
            role_edges(module, offsets, &["Regex", "Text", "Substring"], edges)
        }
        NativeInstr::RegexMatchStart
        | NativeInstr::RegexMatchEnd
        | NativeInstr::RegexMatchText
        | NativeInstr::RegexMatchGroupCount => role_edges(module, offsets, &["RegexMatch"], edges),
        NativeInstr::BytesTextView => role_edges(module, offsets, &["Substring"], edges),
        NativeInstr::CharCodepoint
        | NativeInstr::CharUtf8Len
        | NativeInstr::EqChar
        | NativeInstr::NeChar
        | NativeInstr::LtChar
        | NativeInstr::LeChar
        | NativeInstr::GtChar
        | NativeInstr::GeChar => role_edges(module, offsets, &["Char"], edges),
        NativeInstr::SbAppendStr => role_edges(module, offsets, &["StringBuilder", "Text"], edges),
        NativeInstr::SbAppendChar => role_edges(module, offsets, &["StringBuilder", "Char"], edges),
        NativeInstr::SbNew
        | NativeInstr::SbAppendInt
        | NativeInstr::SbAppendBool
        | NativeInstr::SbBuild
        | NativeInstr::SbByteLen
        | NativeInstr::SbFinish
        | NativeInstr::SbLen
        | NativeInstr::SbClear => role_edges(module, offsets, &["StringBuilder"], edges),
        NativeInstr::BbNew
        | NativeInstr::BbAppend
        | NativeInstr::BbLen
        | NativeInstr::BbBuild
        | NativeInstr::BbFinish
        | NativeInstr::BbExtend
        | NativeInstr::BbReserve
        | NativeInstr::BbClear
        | NativeInstr::BbAt
        | NativeInstr::BbFindFrom
        | NativeInstr::BbSet
        | NativeInstr::BbCapacity
        | NativeInstr::BbTruncate => role_edges(module, offsets, &["ByteBuffer"], edges),
        NativeInstr::BytesEndsWith
        | NativeInstr::BytesContains
        | NativeInstr::BytesNew
        | NativeInstr::BytesLen
        | NativeInstr::BytesText
        | NativeInstr::BytesTextRange
        | NativeInstr::BytesAt
        | NativeInstr::BytesGet
        | NativeInstr::BytesReadU32Be
        | NativeInstr::BytesReadU32Le
        | NativeInstr::BytesSlice
        | NativeInstr::BytesConcat
        | NativeInstr::BytesStartsWith
        | NativeInstr::BytesFindIndex
        | NativeInstr::BytesHex
        | NativeInstr::BytesIsUtf8
        | NativeInstr::EqBytes
        | NativeInstr::NeBytes
        | NativeInstr::LtBytes
        | NativeInstr::LeBytes
        | NativeInstr::GtBytes
        | NativeInstr::GeBytes
        | NativeInstr::BytesCompact
        | NativeInstr::BytesHash
        | NativeInstr::HashCombine
        | NativeInstr::HashUnorderedCombine => {}
    }
}

fn numeric_edges(
    module: &Module,
    offsets: Offsets,
    instruction: &lm_bytecode::NumericInstr,
    edges: &mut Vec<u32>,
) {
    use lm_bytecode::NumericInstr;
    match instruction {
        NumericInstr::TextParseFloatStatus | NumericInstr::TextParseFloatValue => {
            role_edges(module, offsets, &["Text"], edges)
        }
        NumericInstr::SbAppendFloat => role_edges(module, offsets, &["StringBuilder"], edges),
        NumericInstr::IntBitAnd
        | NumericInstr::IntBitOr
        | NumericInstr::IntBitXor
        | NumericInstr::IntBitNot
        | NumericInstr::IntCountOnes
        | NumericInstr::IntLeadingZeros
        | NumericInstr::IntTrailingZeros
        | NumericInstr::IntSignum
        | NumericInstr::IntShl
        | NumericInstr::IntShr
        | NumericInstr::IntUshr
        | NumericInstr::IntWrappingAdd
        | NumericInstr::IntWrappingSub
        | NumericInstr::IntWrappingMul
        | NumericInstr::IntRotateLeft
        | NumericInstr::IntRotateRight
        | NumericInstr::IntRotateLeft32
        | NumericInstr::IntRotateRight32
        | NumericInstr::IntToFloat
        | NumericInstr::FloatNeg
        | NumericInstr::FloatAbs
        | NumericInstr::FloatMin
        | NumericInstr::FloatMax
        | NumericInstr::FloatSqrt
        | NumericInstr::FloatFloor
        | NumericInstr::FloatCeil
        | NumericInstr::FloatRound
        | NumericInstr::FloatTrunc
        | NumericInstr::FloatAdd
        | NumericInstr::FloatSub
        | NumericInstr::FloatMul
        | NumericInstr::FloatDiv
        | NumericInstr::FloatEq
        | NumericInstr::FloatNe
        | NumericInstr::FloatLt
        | NumericInstr::FloatLe
        | NumericInstr::FloatGt
        | NumericInstr::FloatGe
        | NumericInstr::FloatIsNan
        | NumericInstr::FloatIsFinite
        | NumericInstr::FloatIsInfinite
        | NumericInstr::FloatRem
        | NumericInstr::FloatCopySign
        | NumericInstr::FloatMulAdd
        | NumericInstr::FloatPow
        | NumericInstr::FloatExp
        | NumericInstr::FloatExp2
        | NumericInstr::FloatExpM1
        | NumericInstr::FloatLn
        | NumericInstr::FloatLog2
        | NumericInstr::FloatLog10
        | NumericInstr::FloatLn1P
        | NumericInstr::FloatCbrt
        | NumericInstr::FloatHypot
        | NumericInstr::FloatSin
        | NumericInstr::FloatCos
        | NumericInstr::FloatTan
        | NumericInstr::FloatAsin
        | NumericInstr::FloatAcos
        | NumericInstr::FloatAtan
        | NumericInstr::FloatAtan2
        | NumericInstr::FloatSinh
        | NumericInstr::FloatCosh
        | NumericInstr::FloatTanh
        | NumericInstr::FloatAsinh
        | NumericInstr::FloatAcosh
        | NumericInstr::FloatAtanh
        | NumericInstr::FloatHash
        | NumericInstr::FloatBits
        | NumericInstr::FloatFromBits
        | NumericInstr::FloatToIntStatus
        | NumericInstr::FloatToIntValue
        | NumericInstr::BytesBitAnd
        | NumericInstr::BytesBitOr
        | NumericInstr::BytesBitXor
        | NumericInstr::BytesBitNot
        | NumericInstr::FloatFixed => {}
    }
}

fn extended_edges(
    module: &Module,
    offsets: Offsets,
    type_index: &HashMap<BcType, u32>,
    instruction: &ExtendedInstr,
    edges: &mut Vec<u32>,
) {
    match instruction {
        ExtendedInstr::MakeCallback { func, .. } => {
            edges.push(offsets.func(*func));
            callable_type_edge(module, offsets, type_index, *func, true, edges);
        }
        ExtendedInstr::FunctionCode { func } => {
            edges.push(offsets.func(*func));
            core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_FUNCTION_CODE,
                edges,
            );
        }
        ExtendedInstr::ClassCode { class } => {
            edges.push(offsets.class(*class));
            core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_CLASS_CODE,
                edges,
            );
        }
        ExtendedInstr::ModuleCode { module: reflection } => {
            edges.push(offsets.reflection(*reflection));
            core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_MODULE_CODE,
                edges,
            );
        }
        ExtendedInstr::ReflectionRefine { pattern, .. } => {
            edges.push(offsets.func(pattern.function()));
        }
        ExtendedInstr::ReflectionEnd { pattern, .. } => {
            edges.push(offsets.func(*pattern));
        }
        ExtendedInstr::ReflectionDeclarations => role_edges(
            module,
            offsets,
            &["ModuleCode", "VerifiedModule", "DeclarationCode", "List"],
            edges,
        ),
        ExtendedInstr::ReflectionOpen => role_edges(
            module,
            offsets,
            &["ModuleCode", "DeclarationCode", "MemberCode", "OpenCode"],
            edges,
        ),
        ExtendedInstr::ReflectionMembers => role_edges(
            module,
            offsets,
            &["DeclarationCode", "MemberCode", "List"],
            edges,
        ),
        ExtendedInstr::ReflectionName => role_edges(
            module,
            offsets,
            &["ModuleCode", "DeclarationCode", "MemberCode"],
            edges,
        ),
        ExtendedInstr::ReflectionDeclarationKind => role_edges(
            module,
            offsets,
            &[
                "DeclarationCode",
                "CodeKind",
                "CodeKind.Class",
                "CodeKind.Enum",
                "CodeKind.Interface",
                "CodeKind.Function",
                "CodeKind.Constant",
            ],
            edges,
        ),
        ExtendedInstr::ReflectionMemberKind => role_edges(
            module,
            offsets,
            &["MemberCode", "CodeKind", "CodeKind.Method"],
            edges,
        ),
        ExtendedInstr::ReflectionTypeParameterCount => {
            role_edges(module, offsets, &["DeclarationCode"], edges)
        }
        ExtendedInstr::ReflectionInterfaceNames => role_edges(
            module,
            offsets,
            &["DeclarationCode", "List", "String"],
            edges,
        ),
        ExtendedInstr::OptionSome { ty }
        | ExtendedInstr::OptionNone { ty }
        | ExtendedInstr::OptionPayload { ty }
        | ExtendedInstr::ListGet { ty }
        | ExtendedInstr::MapGet { ty }
        | ExtendedInstr::MapPutText { ty, .. }
        | ExtendedInstr::ListPop { ty }
        | ExtendedInstr::MapRemove { ty }
        | ExtendedInstr::PrepareWait { reply_ty: ty, .. } => edges.push(offsets.ty(*ty)),
        ExtendedInstr::RegexCaptures { ty } => {
            edges.push(offsets.ty(*ty));
            role_edges(module, offsets, &["Regex", "RegexMatch", "Text"], edges);
        }
        ExtendedInstr::RegexMatchGroup { ty } | ExtendedInstr::RegexMatchNamed { ty } => {
            edges.push(offsets.ty(*ty));
            role_edges(module, offsets, &["RegexMatch", "Text", "Substring"], edges);
        }
        ExtendedInstr::CodeSource { ty } => {
            edges.push(offsets.ty(*ty));
            core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_DEFINITION_SOURCE,
                edges,
            );
        }
        ExtendedInstr::CodeDefinition => core_role_edge(
            module,
            offsets,
            lm_bytecode::corepin::ROLE_DEFINITION_SPEC,
            edges,
        ),
        ExtendedInstr::FaultSite { ty } | ExtendedInstr::FaultTrace { ty } => {
            edges.push(offsets.ty(*ty));
            core_role_edge(
                module,
                offsets,
                lm_bytecode::corepin::ROLE_CODE_LOCATION,
                edges,
            );
        }
        ExtendedInstr::CallSlot { slot, app } | ExtendedInstr::NewSlot { slot, app } => {
            edges.push(offsets.slot(*slot));
            if *app != NO_APP {
                edges.push(offsets.app(*app));
            }
        }
        ExtendedInstr::LoadSlot { slot } | ExtendedInstr::SendSlot { slot } => {
            edges.push(offsets.slot(*slot));
            if matches!(instruction, ExtendedInstr::SendSlot { .. }) {
                role_edges(module, offsets, &["SendResult"], edges);
            }
        }
        ExtendedInstr::SyntaxTreeRoot
        | ExtendedInstr::SyntaxKind
        | ExtendedInstr::SyntaxCategory
        | ExtendedInstr::SyntaxRangeStart
        | ExtendedInstr::SyntaxRangeEnd
        | ExtendedInstr::SyntaxText
        | ExtendedInstr::SyntaxChildren
        | ExtendedInstr::SyntaxDetach
        | ExtendedInstr::SyntaxBuildToken
        | ExtendedInstr::SyntaxBuildTrivia
        | ExtendedInstr::SyntaxBuildNode
        | ExtendedInstr::SyntaxToTree => role_edges(module, offsets, &["SyntaxElement"], edges),
        ExtendedInstr::DynPack { ty } => {
            edges.push(offsets.ty(*ty));
            role_edges(module, offsets, &["DynValue"], edges);
        }
        ExtendedInstr::DynRender => role_edges(module, offsets, &["DynValue"], edges),
        ExtendedInstr::AsCallback
        | ExtendedInstr::MapInternTextRange
        | ExtendedInstr::ListEpoch
        | ExtendedInstr::ListIterLen
        | ExtendedInstr::MapEpoch
        | ExtendedInstr::MapIterLen
        | ExtendedInstr::MapNextIndex
        | ExtendedInstr::SealInstance
        | ExtendedInstr::MapKeyAt
        | ExtendedInstr::MapValueAt
        | ExtendedInstr::ListCapacity
        | ExtendedInstr::ListSet
        | ExtendedInstr::ListInsert
        | ExtendedInstr::ListRemove
        | ExtendedInstr::ListSwapRemove
        | ExtendedInstr::ListSwap
        | ExtendedInstr::ListReserve
        | ExtendedInstr::ListTruncate
        | ExtendedInstr::ListContains
        | ExtendedInstr::ListReorder
        | ExtendedInstr::MapClear
        | ExtendedInstr::MapReserve
        | ExtendedInstr::MapProbe
        | ExtendedInstr::MapProbeFound
        | ExtendedInstr::MapProbeKey
        | ExtendedInstr::MapProbeValue
        | ExtendedInstr::MapProbeSetValue
        | ExtendedInstr::MapProbeRemove
        | ExtendedInstr::MapInsertHashed
        | ExtendedInstr::MapWriteGuard => {}
    }
}

fn callable_type_edge(
    module: &Module,
    offsets: Offsets,
    type_index: &HashMap<BcType, u32>,
    function: u32,
    callback: bool,
    edges: &mut Vec<u32>,
) {
    let function = &module.funcs[function as usize];
    let ty = if callback {
        BcType::Callback(
            function.params.clone(),
            function.param_muts.clone(),
            function.ret,
            function.row.clone(),
        )
    } else {
        BcType::Fn(
            function.params.clone(),
            function.param_muts.clone(),
            function.ret,
            function.row.clone(),
        )
    };
    if let Some(index) = type_index.get(&ty) {
        edges.push(offsets.ty(*index));
    }
}

fn slot_edges(offsets: Offsets, slot: &SlotSpec, edges: &mut Vec<u32>) {
    match &slot.contract {
        SlotContract::Function(contract) | SlotContract::Method(contract) => {
            callable_edges(offsets, contract, edges);
        }
        SlotContract::Class {
            ty, constructor, ..
        } => {
            edges.push(offsets.ty(*ty));
            callable_edges(offsets, constructor, edges);
        }
        SlotContract::Value { ty } => edges.push(offsets.ty(*ty)),
        SlotContract::Process { message, result } => {
            edges.push(offsets.ty(*message));
            edges.push(offsets.ty(*result));
        }
    }
    match slot.initial {
        Some(SlotTarget::Function(function)) => edges.push(offsets.func(function)),
        Some(SlotTarget::Class { class, constructor }) => {
            edges.push(offsets.class(class));
            edges.push(offsets.func(constructor));
        }
        None => {}
    }
}

const DEAD: u32 = u32::MAX;

impl Reloc {
    fn from_live(module: &Module, offsets: Offsets, live: &[bool]) -> Reloc {
        Reloc {
            strings: table_reloc(module.strings.len(), offsets.strings, live),
            bytes: table_reloc(module.bytes.len(), offsets.bytes, live),
            types: table_reloc(module.types.len(), offsets.types, live),
            selectors: table_reloc(module.selectors.len(), offsets.selectors, live),
            apps: table_reloc(module.apps.len(), offsets.apps, live),
            interfaces: table_reloc(module.interfaces.len(), offsets.interfaces, live),
            classes: table_reloc(module.classes.len(), offsets.classes, live),
            funcs: table_reloc(module.funcs.len(), offsets.funcs, live),
            slots: table_reloc(module.slots.len(), offsets.slots, live),
            reflections: table_reloc(module.reflections.len(), offsets.reflections, live),
        }
    }
}

fn table_reloc(count: usize, base: usize, live: &[bool]) -> Vec<u32> {
    let mut next = 0u32;
    (0..count)
        .map(|index| {
            if live[base + index] {
                let target = next;
                next += 1;
                target
            } else {
                DEAD
            }
        })
        .collect()
}

fn relocate_module(
    module: &Module,
    reloc: &Reloc,
    keep_imports: bool,
    exports: &[Export],
) -> Result<Module, String> {
    let strings = retain_table(&module.strings, &reloc.strings, Clone::clone);
    let bytes = retain_table(&module.bytes, &reloc.bytes, Clone::clone);
    let types = retain_table(&module.types, &reloc.types, |ty| {
        reloc_type(ty, &reloc.types, &reloc.classes, &reloc.interfaces)
    });
    let selectors = retain_table(&module.selectors, &reloc.selectors, Clone::clone);
    let apps = retain_table(&module.apps, &reloc.apps, |app| TypeApp {
        types: app
            .types
            .iter()
            .map(|ty| reloc.types[*ty as usize])
            .collect(),
        rows: app.rows.iter().map(|row| reloc_row(row)).collect(),
    });
    let interfaces = retain_table(&module.interfaces, &reloc.interfaces, |interface| {
        reloc_interface(interface, reloc)
    });
    let classes = retain_table(&module.classes, &reloc.classes, |class| {
        reloc_class(class, reloc)
    });
    let class_bounds = module
        .class_bounds
        .iter()
        .enumerate()
        .filter(|(index, _)| reloc.classes[*index] != DEAD)
        .map(|(_, bounds)| reloc_bounds(bounds, reloc))
        .collect();
    let funcs = retain_table(&module.funcs, &reloc.funcs, |func| reloc_func(func, reloc));
    let func_bounds = module
        .func_bounds
        .iter()
        .enumerate()
        .filter(|(index, _)| reloc.funcs[*index] != DEAD)
        .map(|(_, bounds)| reloc_bounds(bounds, reloc))
        .collect();
    let conformances = module
        .conformances
        .iter()
        .filter(|item| reloc.classes[item.class as usize] != DEAD)
        .map(|item| reloc_conformance(item, reloc))
        .collect();
    let slots = retain_table(&module.slots, &reloc.slots, |slot| reloc_slot(slot, reloc));
    let reflections = retain_table(&module.reflections, &reloc.reflections, |reflection| {
        reloc_reflection(reflection, reloc)
    });
    let imports = if keep_imports {
        module
            .imports
            .iter()
            .filter_map(|import| reloc_import(import, reloc))
            .collect()
    } else {
        Vec::new()
    };
    let exports = exports
        .iter()
        .map(|export| reloc_export(export, reloc))
        .collect::<Result<Vec<_>, _>>()?;
    let mut core_roles = [NO_ROLE; lm_bytecode::CORE_ROLE_COUNT];
    for (role, class) in module.core_roles.iter().enumerate() {
        if *class != NO_ROLE {
            let target = reloc.classes[*class as usize];
            if target != DEAD {
                core_roles[role] = target;
            }
        }
    }
    let bindings = module
        .bindings
        .iter()
        .filter_map(|binding| {
            let func = reloc.funcs[binding.func as usize];
            let class = if binding.class == NO_CLASS {
                NO_CLASS
            } else {
                reloc.classes[binding.class as usize]
            };
            if func == DEAD || (binding.class != NO_CLASS && class == DEAD) {
                None
            } else {
                Some(lm_bytecode::FuncBinding {
                    key: binding.key.clone(),
                    func,
                    class,
                })
            }
        })
        .collect();
    let entry = reloc.funcs[module.entry as usize];
    let entry = if entry == DEAD {
        reloc
            .funcs
            .iter()
            .copied()
            .find(|function| *function != DEAD)
            .ok_or_else(|| "dependency collection retained no entry function".to_string())?
    } else {
        entry
    };
    let mut collected = Module {
        strings,
        bytes,
        types,
        selectors,
        apps,
        interfaces,
        conformances,
        class_bounds,
        func_bounds,
        imports,
        slots,
        core_roles,
        reflections,
        classes,
        funcs,
        entry,
        exports,
        bindings,
        debug: Vec::new(),
    };
    collected.debug = relocate_debug(module, reloc, &collected)?;
    Ok(collected)
}

fn reloc_import(source: &Import, reloc: &Reloc) -> Option<Import> {
    if source.kind == ImportKind::Constant {
        return Some(Import {
            module: source.module.clone(),
            name: source.name.clone(),
            kind: source.kind,
            def: lm_bytecode::NO_IMPORT_DEF,
            hash: source.hash,
        });
    }
    let def = if source.kind == ImportKind::Class {
        reloc.classes[source.def as usize]
    } else {
        reloc.funcs[source.def as usize]
    };
    (def != DEAD).then(|| Import {
        module: source.module.clone(),
        name: source.name.clone(),
        kind: source.kind,
        def,
        hash: source.hash,
    })
}

fn reloc_export(source: &Export, reloc: &Reloc) -> Result<Export, String> {
    if source.kind.is_constant() {
        let constant = source
            .constant
            .as_ref()
            .ok_or_else(|| format!("the constant export `{}` has no value", source.name))?;
        if source.def != NO_CTOR || source.ctor != NO_CTOR {
            return Err(format!(
                "the constant export `{}` has invalid runtime fields",
                source.name
            ));
        }
        let ty = reloc
            .types
            .get(constant.ty as usize)
            .copied()
            .ok_or_else(|| {
                format!(
                    "the constant export `{}` names a type outside the module",
                    source.name
                )
            })?;
        if ty == DEAD {
            return Err(format!(
                "dependency collection removed the type of `{}`",
                source.name
            ));
        }
        return Ok(Export {
            kind: source.kind,
            name: source.name.clone(),
            source: source.source,
            def: source.def,
            ctor: source.ctor,
            constant: Some(lm_bytecode::Constant {
                ty,
                value: constant.value.clone(),
            }),
        });
    }
    let def = if source.kind.is_class() {
        reloc.classes[source.def as usize]
    } else if source.kind.is_interface() {
        reloc.interfaces[source.def as usize]
    } else {
        reloc.funcs[source.def as usize]
    };
    if def == DEAD {
        return Err(format!(
            "dependency collection removed the requested export `{}`",
            source.name
        ));
    }
    let ctor = match source.ctor {
        NO_CTOR => NO_CTOR,
        source_ctor => {
            let ctor = reloc.funcs[source_ctor as usize];
            if ctor == DEAD {
                return Err(format!(
                    "dependency collection removed the constructor of `{}`",
                    source.name
                ));
            }
            ctor
        }
    };
    Ok(Export {
        kind: source.kind,
        name: source.name.clone(),
        source: source.source,
        def,
        ctor,
        constant: source.constant.clone(),
    })
}

fn retain_table<T, U>(source: &[T], reloc: &[u32], mut transform: impl FnMut(&T) -> U) -> Vec<U> {
    source
        .iter()
        .enumerate()
        .filter(|(index, _)| reloc[*index] != DEAD)
        .map(|(_, item)| transform(item))
        .collect()
}

fn relocate_debug(module: &Module, reloc: &Reloc, collected: &Module) -> Result<Vec<u8>, String> {
    let debug = lm_bytecode::debug::decode(&module.debug)
        .map_err(|error| format!("dependency collection found invalid debug data: {error}"))?;
    lm_bytecode::debug::validate(&debug, module)
        .map_err(|error| format!("dependency collection found invalid debug data: {error}"))?;
    let definitions: Vec<DebugDefinition> = debug
        .definitions
        .iter()
        .filter_map(|definition| {
            let target = match definition.kind {
                lm_bytecode::debug::DefinitionKind::Function => {
                    reloc.funcs[definition.target as usize]
                }
                lm_bytecode::debug::DefinitionKind::Class => {
                    reloc.classes[definition.target as usize]
                }
            };
            (target != DEAD).then(|| DebugDefinition {
                target,
                ..definition.clone()
            })
        })
        .collect();
    let functions: Vec<DebugFunction> = debug
        .functions
        .iter()
        .filter_map(|function| {
            let target = reloc.funcs[function.function as usize];
            (target != DEAD).then(|| DebugFunction {
                function: target,
                ..function.clone()
            })
        })
        .collect();
    let code_origins: Vec<DebugCodeOrigin> = debug
        .code_origins
        .iter()
        .filter_map(|origin| {
            let function = reloc.funcs[origin.function as usize];
            (function != DEAD).then(|| DebugCodeOrigin {
                function,
                ..origin.clone()
            })
        })
        .collect();
    let mut source_live = vec![false; debug.sources.len()];
    for definition in &definitions {
        source_live[definition.source as usize] = true;
    }
    for function in &functions {
        source_live[function.source as usize] = true;
    }
    let mut source_reloc = vec![DEAD; debug.sources.len()];
    let mut sources = Vec::new();
    for (index, source) in debug.sources.iter().enumerate() {
        if source_live[index] {
            source_reloc[index] = sources.len() as u32;
            sources.push(source.clone());
        }
    }
    let mut relocated = DebugInfo {
        sources,
        definitions,
        functions,
        code_origins,
    };
    for definition in &mut relocated.definitions {
        definition.source = source_reloc[definition.source as usize];
    }
    for function in &mut relocated.functions {
        function.source = source_reloc[function.source as usize];
    }
    lm_bytecode::debug::validate(&relocated, collected)
        .map_err(|error| format!("dependency collection produced invalid debug data: {error}"))?;
    Ok(lm_bytecode::debug::encode(&relocated))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_function(name: &str, blocks: Vec<Vec<Instr>>) -> Func {
        Func {
            name: name.to_string(),
            param_names: Vec::new(),
            type_params: 0,
            effect_params: 0,
            params: Vec::new(),
            param_muts: Vec::new(),
            ret: 0,
            row: Vec::new(),
            captures: Vec::new(),
            local_types: Vec::new(),
            blocks,
        }
    }

    fn function_module(funcs: Vec<Func>, entry: u32) -> Module {
        Module {
            strings: Vec::new(),
            bytes: Vec::new(),
            types: vec![BcType::Unit],
            selectors: Vec::new(),
            apps: Vec::new(),
            interfaces: Vec::new(),
            conformances: Vec::new(),
            class_bounds: Vec::new(),
            func_bounds: vec![Vec::new(); funcs.len()],
            imports: Vec::new(),
            slots: Vec::new(),
            core_roles: [NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            reflections: Vec::new(),
            classes: Vec::new(),
            funcs,
            entry,
            exports: Vec::new(),
            bindings: Vec::new(),
            debug: lm_bytecode::debug::encode(&DebugInfo::default()),
        }
    }

    #[test]
    fn an_unreached_mutual_cycle_is_removed() {
        let module = function_module(
            vec![
                unit_function("entry", vec![vec![Instr::ConstUnit, Instr::Return]]),
                unit_function("left", vec![vec![Instr::Call(2), Instr::Return]]),
                unit_function("right", vec![vec![Instr::Call(1), Instr::Return]]),
            ],
            0,
        );
        let collected = collect_program(&module).expect("the program collects");
        assert_eq!(module.funcs.len(), 3);
        assert_eq!(collected.funcs.len(), 1);
        assert_eq!(collected.funcs[0].name, "entry");
    }

    #[test]
    fn a_deep_dependency_chain_does_not_use_the_host_stack() {
        let live_count = 20_000usize;
        let mut funcs = Vec::with_capacity(live_count + 1);
        for index in 0..live_count {
            let blocks = if index == 0 {
                vec![vec![Instr::ConstUnit, Instr::Return]]
            } else {
                vec![vec![Instr::Call(index as u32 - 1), Instr::Return]]
            };
            funcs.push(unit_function(&format!("live_{index}"), blocks));
        }
        funcs.push(unit_function(
            "unused",
            vec![vec![Instr::ConstUnit, Instr::Return]],
        ));
        let module = function_module(funcs, live_count as u32 - 1);
        let collected = collect_program(&module).expect("the deep program collects");
        assert_eq!(collected.funcs.len(), live_count);
        assert_eq!(collected.funcs.last().unwrap().name, "live_19999");
    }
}
