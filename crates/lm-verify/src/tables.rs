//! Structural validation of every module table.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

/// Validate the type, selector, application, class, and function
/// tables.
/// One shared table error. No table names a function.
fn terr(message: String) -> VerifyError {
    VerifyError {
        func: None,
        message,
    }
}

/// Build a named subject for one table entry.
fn named_table_subject(kind: &str, name: &str, index: usize) -> String {
    if name.is_empty() {
        format!("{kind} table {index}")
    } else {
        format!("{kind} `{name}` (table {index})")
    }
}

/// Build a named subject for one conformance entry.
fn conformance_subject(
    module: &Module,
    index: usize,
    conformance: &lm_bytecode::BcConformance,
) -> String {
    let class = module
        .classes
        .get(conformance.class as usize)
        .map(|item| item.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("<invalid class>");
    let interface = module
        .interfaces
        .get(conformance.application.interface as usize)
        .map(|item| item.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("<invalid interface>");
    format!("conformance `{class}: {interface}` (table {index})")
}

pub(crate) fn verify_tables(
    module: &Module,
    core: CoreLayout,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
) -> Result<Ctx<'_>, VerifyError> {
    verify_selectors(module)?;
    // The type table must start with the canonical primitive prefix.
    let prefix = [BcType::Unit, BcType::Bool, BcType::Int, BcType::Str];
    if module.types.len() < prefix.len() || module.types[..prefix.len()] != prefix[..] {
        return Err(terr(
            "the type table does not start with Unit, Bool, Int, String".to_string(),
        ));
    }
    let mut index: HashMap<BcType, u32> = HashMap::new();
    for (idx, ty) in module.types.iter().enumerate() {
        if index.insert(ty.clone(), idx as u32).is_some() {
            return Err(terr(format!("type {idx} duplicates an earlier type entry")));
        }
        let check_ref = |child: u32| -> Result<(), VerifyError> {
            if child as usize >= idx {
                return Err(terr(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )));
            }
            Ok(())
        };
        match ty {
            BcType::Unit | BcType::Bool | BcType::Int | BcType::Float | BcType::Str => {}
            BcType::Digest
            | BcType::Bytes
            | BcType::FileHandle
            | BcType::ResourceHandle
            | BcType::HostResource => {}
            BcType::Var(_) => {}
            BcType::Projection {
                base,
                interface,
                assoc,
            } => {
                check_ref(*base)?;
                let Some(contract) = module.interfaces.get(*interface as usize) else {
                    return Err(terr(format!(
                        "type {idx} names interface {interface}, which does not exist"
                    )));
                };
                if *assoc as usize >= contract.associated.len() {
                    return Err(terr(format!(
                        "type {idx} names associated type {assoc}, which does not exist"
                    )));
                }
            }
            BcType::Class(c) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                if module.classes[*c as usize].type_params != 0 {
                    return Err(terr(format!(
                        "type {idx} names the generic class {c} without arguments"
                    )));
                }
            }
            BcType::Inst(c, args) => {
                if *c as usize >= module.classes.len() {
                    return Err(terr(format!(
                        "type {idx} names class {c}, which does not exist"
                    )));
                }
                let arity = module.classes[*c as usize].type_params;
                if arity == 0 || args.len() != arity as usize {
                    return Err(terr(format!(
                        "type {idx} applies class {c} with the wrong argument count"
                    )));
                }
                for a in args {
                    check_ref(*a)?;
                }
            }
            BcType::List(e) => check_ref(*e)?,
            BcType::Map(k, v) => {
                check_ref(*k)?;
                check_ref(*v)?;
            }
            BcType::Tuple(elems) => {
                if elems.is_empty() || elems.len() > MAX_TUPLE_ARITY {
                    return Err(terr(format!(
                        "type {idx} is a tuple with an invalid arity {}",
                        elems.len()
                    )));
                }
                for e in elems {
                    check_ref(*e)?;
                }
            }
            BcType::Fn(params, muts, ret, row) | BcType::Callback(params, muts, ret, row) => {
                if muts.len() != params.len() {
                    return Err(terr(format!(
                        "type {idx} has a function type whose mut markers do \
                         not align with the parameters"
                    )));
                }
                for p in params {
                    check_ref(*p)?;
                }
                check_ref(*ret)?;
                for elem in row {
                    if let BcRow::Op(s) = elem {
                        if *s as usize >= module.strings.len() {
                            return Err(terr(format!(
                                "type {idx} has a row with an invalid string index"
                            )));
                        }
                        if !bundle.row_name_valid(&module.strings[*s as usize]) {
                            return Err(terr(format!(
                                "type {idx} has a row that names `{}`, which is \
                                 not in the operation manifest",
                                module.strings[*s as usize]
                            )));
                        }
                    }
                }
            }
            BcType::Fault
            | BcType::Request
            | BcType::PolicyTable
            | BcType::Vm
            | BcType::VmSnapshot => {}
            BcType::Run(t) | BcType::Wait(t) | BcType::RunSnapshot(t) => check_ref(*t)?,
            BcType::PendingCall(a, r) | BcType::Handle(a, r) => {
                check_ref(*a)?;
                check_ref(*r)?;
            }
            BcType::Op(op, f) => {
                if bundle
                    .op(*op)
                    .is_none_or(|operation| operation.kind != lm_abi::OpKind::Fixed)
                {
                    return Err(terr(format!(
                        "type {idx} names an invalid first-class operation slot {op}"
                    )));
                }
                check_ref(*f)?;
                // The function type must equal the manifest
                // signature; the check runs after the context exists.
            }
        }
    }
    // Class types by class index.
    let mut class_ty = vec![None; module.classes.len()];
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Class(c) = ty {
            class_ty[*c as usize] = Some(idx as u32);
        }
    }
    let mut conformance_index = HashMap::with_capacity(module.conformances.len());
    for (index, conformance) in module.conformances.iter().enumerate() {
        if conformance.class as usize >= module.classes.len()
            || conformance.application.interface as usize >= module.interfaces.len()
        {
            continue;
        }
        conformance_index
            .entry((conformance.class, conformance.application.interface))
            .or_insert(index);
    }
    let mut constructor_classes = vec![None; module.funcs.len()];
    let mut class_constructors = vec![Vec::new(); module.classes.len()];
    for binding in &module.bindings {
        if binding.class == lm_bytecode::NO_CLASS || binding.class as usize >= module.classes.len()
        {
            continue;
        }
        class_constructors[binding.class as usize].push(binding.func);
        if let Some(class) = constructor_classes.get_mut(binding.func as usize) {
            class.get_or_insert(binding.class);
        }
    }
    // Bound the nesting depth of the type table. A type child names an
    // earlier entry, so one forward pass gives the depth of each entry.
    let mut depth: Vec<u32> = Vec::with_capacity(module.types.len());
    let mut kids: Vec<u32> = Vec::new();
    for (idx, ty) in module.types.iter().enumerate() {
        kids.clear();
        lm_bytecode::closed::bc_children(ty, &mut kids);
        let mut own = 1u32;
        for child in &kids {
            let Some(child_depth) = depth.get(*child as usize) else {
                return Err(terr(format!(
                    "type {idx} references type {child}, which is not an earlier entry"
                )));
            };
            own = own.max(child_depth + 1);
        }
        if own > MAX_TYPE_DEPTH {
            return Err(terr(format!(
                "type {idx} nests {own} deep, and the limit is {MAX_TYPE_DEPTH}"
            )));
        }
        depth.push(own);
    }
    let mut facts = Vec::with_capacity(module.types.len());
    for ty in &module.types {
        facts.push(type_facts(ty, &facts));
    }
    let ctx = Ctx {
        module,
        bundle,
        class_ty,
        conformance_index,
        constructor_classes,
        class_constructors,
        uni: RefCell::new(Universe {
            types: module.types.clone(),
            index,
            facts,
        }),
        core,
    };
    verify_type_placement(&ctx)?;
    verify_applications(&ctx)?;
    let interface_self = verify_interfaces(&ctx)?;
    verify_imports(&ctx)?;
    verify_classes(&ctx)?;
    verify_conformances(&ctx, &interface_self)?;
    verify_map_key_types(&ctx)?;
    verify_signatures(&ctx)?;
    verify_slots(&ctx)?;
    Ok(ctx)
}

/// Reject one concrete map key that cannot implement `Hashable`.
///
/// A type variable or projection needs its use-site bounds. The
/// instruction verifier checks those bounds before any map operation.
fn verify_map_key_types(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let hashable = ctx.hashable_interface();
    for (index, ty) in ctx.module.types.iter().enumerate() {
        let BcType::Map(key, _) = ty else {
            continue;
        };
        if ctx.native_map_key(*key)
            || matches!(ctx.ty(*key), BcType::Var(_) | BcType::Projection { .. })
            || hashable.is_some_and(|interface| ctx.concrete_conformance(*key, interface).is_some())
        {
            continue;
        }
        return Err(terr(format!(
            "type {index} has a map key type that does not implement Hashable"
        )));
    }
    Ok(())
}

/// The selector table holds no duplicate name.
fn verify_selectors(module: &Module) -> Result<(), VerifyError> {
    // The selector table must hold no duplicate name. The canonical
    // identity encoding replaces a selector index with its name, so a
    // duplicate name lets two different dispatch keys hash alike. The
    // verified-code cache keys on that hash, so this rule keeps the
    // index-to-name map injective and belongs in the structural pass.
    let mut selector_names: HashMap<&str, u32> = HashMap::new();
    for (idx, name) in module.selectors.iter().enumerate() {
        if let Some(first) = selector_names.insert(name.as_str(), idx as u32) {
            return Err(terr(format!(
                "selector {idx} duplicates the name of selector {first}"
            )));
        }
    }
    Ok(())
}

/// Callback placement, row canonicality, and operation types.
fn verify_type_placement(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    // A callback can occur only as one direct callable parameter.
    // A safe higher-order function type can occur in any value position.
    let mut callback_children = Vec::new();
    for idx in 0..module.types.len() as u32 {
        callback_children.clear();
        let invalid = match ctx.ty(idx) {
            BcType::Fn(params, _, ret, _) | BcType::Callback(params, _, ret, _) => {
                params.iter().any(|param| {
                    ctx.stores_callback(*param) && !matches!(ctx.ty(*param), BcType::Callback(..))
                }) || ctx.stores_callback(ret)
            }
            _ => {
                ctx.type_children(idx, &mut callback_children);
                callback_children
                    .iter()
                    .any(|child| ctx.stores_callback(*child))
            }
        };
        if invalid {
            return Err(terr(format!(
                "type {idx} stores a nonescaping callback inside another type"
            )));
        }
    }
    // Row canonicality inside function types.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Fn(_, _, _, row) | BcType::Callback(_, _, _, row) = ty {
            if !ctx.row_canonical(row) {
                return Err(terr(format!("type {idx} has a non-canonical row")));
            }
        }
    }
    // First-class operation types must carry the exact manifest
    // signature.
    for (idx, ty) in module.types.iter().enumerate() {
        if let BcType::Op(op, f) = ty {
            let sig = ctx.fixed_sig_type(*op).map_err(&terr)?;
            if *f != sig {
                return Err(terr(format!(
                    "type {idx} claims a wrong signature for operation {}",
                    ctx.bundle.op_name(*op).unwrap_or("<invalid operation>")
                )));
            }
        }
    }
    Ok(())
}

/// The type applications.
fn verify_applications(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    // Validate the type applications.
    for (aidx, app) in module.apps.iter().enumerate() {
        for t in &app.types {
            if *t as usize >= module.types.len() {
                return Err(terr(format!(
                    "application {aidx} references an invalid type index"
                )));
            }
            if ctx.stores_callback(*t) {
                return Err(terr(format!(
                    "application {aidx} contains a nonescaping callback"
                )));
            }
        }
        for row in &app.rows {
            for elem in row {
                if let BcRow::Op(s) = elem {
                    if *s as usize >= module.strings.len() {
                        return Err(terr(format!(
                            "application {aidx} has a row with an invalid string index"
                        )));
                    }
                    if !ctx.bundle.row_name_valid(&module.strings[*s as usize]) {
                        return Err(terr(format!(
                            "application {aidx} has a row that names `{}`, which \
                             is not in the operation manifest",
                            module.strings[*s as usize]
                        )));
                    }
                }
            }
            if !ctx.row_canonical(row) {
                return Err(terr(format!("application {aidx} has a non-canonical row")));
            }
        }
    }
    Ok(())
}

/// The nominal interface contracts.
fn verify_interfaces(ctx: &Ctx<'_>) -> Result<Vec<bool>, VerifyError> {
    let module = ctx.module;
    // Validate nominal interface contracts before any bound uses them.
    let mut interface_keys: HashMap<&str, usize> = HashMap::new();
    for (iidx, contract) in module.interfaces.iter().enumerate() {
        let ierr = |message: String| {
            terr(format!(
                "{}: {message}",
                named_table_subject("interface", &contract.name, iidx)
            ))
        };
        if contract.name.is_empty() || contract.key.is_empty() {
            return Err(ierr("the name and key must not be empty".to_string()));
        }
        if let Some(first) = interface_keys.insert(contract.key.as_str(), iidx) {
            return Err(ierr(format!("the key duplicates interface {first}")));
        }
        if contract.generic_is_effect.len()
            != (contract.type_params + contract.effect_params) as usize
            || contract
                .generic_is_effect
                .iter()
                .filter(|item| !**item)
                .count()
                != contract.type_params as usize
            || contract
                .generic_is_effect
                .iter()
                .filter(|item| **item)
                .count()
                != contract.effect_params as usize
        {
            return Err(ierr(
                "the generic kind markers do not match the arities".to_string(),
            ));
        }
        if contract.type_bounds.len() != contract.type_params as usize {
            return Err(ierr(
                "the type-bound table does not match the type arity".to_string(),
            ));
        }
        let self_application = BcInterfaceUse {
            interface: iidx as u32,
            types: (0..contract.type_params)
                .map(|parameter| ctx.intern(BcType::Var(parameter + 1)))
                .collect(),
            rows: (0..contract.effect_params)
                .map(|parameter| vec![BcRow::Var(parameter)])
                .collect(),
        };
        let mut self_bounds = vec![self_application];
        let mut parent_ids = HashSet::new();
        for parent in &contract.parents {
            ctx.check_interface_use(parent, contract.type_params + 1, contract.effect_params)
                .map_err(&ierr)?;
            if parent.interface == iidx as u32 {
                return Err(ierr("an interface cannot extend itself".to_string()));
            }
            if !parent_ids.insert(parent.interface) {
                return Err(ierr("a parent interface appears twice".to_string()));
            }
            self_bounds.push(parent.clone());
        }
        let mut interface_scope = vec![self_bounds];
        interface_scope.extend(contract.type_bounds.clone());
        let self_ty = ctx.intern(BcType::Var(0));
        for parent in &contract.parents {
            if !ctx.interface_arguments_meet_bounds(self_ty, parent, &interface_scope) {
                return Err(ierr(
                    "a parent interface has arguments outside its bounds".to_string(),
                ));
            }
        }
        for (parameter, bounds) in contract.type_bounds.iter().enumerate() {
            let mut seen = HashSet::new();
            for bound in bounds {
                ctx.check_interface_use(bound, contract.type_params + 1, contract.effect_params)
                    .map_err(&ierr)?;
                if !seen.insert(bound.interface) {
                    return Err(ierr(
                        "one type parameter repeats an interface bound".to_string(),
                    ));
                }
                let receiver = ctx.intern(BcType::Var(parameter as u32 + 1));
                if !ctx.interface_arguments_meet_bounds(receiver, bound, &interface_scope) {
                    return Err(ierr(
                        "an interface bound has arguments outside their bounds".to_string(),
                    ));
                }
            }
        }
        let mut associated_names = HashSet::new();
        for (associated_index, associated) in contract.associated.iter().enumerate() {
            if associated.name.is_empty() || !associated_names.insert(associated.name.as_str()) {
                return Err(ierr(
                    "associated type names must be nonempty and unique".to_string(),
                ));
            }
            let mut seen = HashSet::new();
            for bound in &associated.bounds {
                ctx.check_interface_use(bound, contract.type_params + 1, contract.effect_params)
                    .map_err(&ierr)?;
                if !seen.insert(bound.interface) {
                    return Err(ierr(
                        "one associated type repeats an interface bound".to_string(),
                    ));
                }
                let base = ctx.intern(BcType::Var(0));
                let receiver = ctx.intern(BcType::Projection {
                    base,
                    interface: iidx as u32,
                    assoc: associated_index as u32,
                });
                if !ctx.interface_arguments_meet_bounds(receiver, bound, &interface_scope) {
                    return Err(ierr(
                        "an associated bound has arguments outside their bounds".to_string(),
                    ));
                }
            }
        }
        let mut method_selectors = HashSet::new();
        for method in &contract.methods {
            if method.selector as usize >= module.selectors.len() {
                return Err(ierr("a method selector is out of range".to_string()));
            }
            if !method_selectors.insert(method.selector) {
                return Err(ierr("a method selector appears twice".to_string()));
            }
            if method.param_muts.len() != method.params.len() {
                return Err(ierr(
                    "method mut markers do not match the parameters".to_string(),
                ));
            }
            for ty in method.params.iter().chain([&method.ret]) {
                if *ty as usize >= module.types.len() {
                    return Err(ierr("a method type is out of range".to_string()));
                }
                if !ctx.vars_bounded(*ty, contract.type_params + 1, contract.effect_params) {
                    return Err(ierr("a method type uses an unbound variable".to_string()));
                }
                if !ctx.projections_proven(*ty, &interface_scope) {
                    return Err(ierr(
                        "a method type uses an associated type without its interface bound"
                            .to_string(),
                    ));
                }
            }
            for ty in &method.params {
                if ctx.stores_callback(*ty) && !matches!(ctx.ty(*ty), BcType::Callback(..)) {
                    return Err(ierr(
                        "a callback must be a direct method parameter".to_string(),
                    ));
                }
            }
            if ctx.stores_callback(method.ret) {
                return Err(ierr("a method cannot return a callback".to_string()));
            }
            if !ctx.row_vars_bounded(&method.row, contract.effect_params) {
                return Err(ierr("a method row uses an unbound variable".to_string()));
            }
            for element in &method.row {
                if let BcRow::Op(string) = element {
                    let Some(name) = module.strings.get(*string as usize) else {
                        return Err(ierr("a method row string is out of range".to_string()));
                    };
                    if !ctx.bundle.row_name_valid(name) {
                        return Err(ierr("a method row names an unknown effect".to_string()));
                    }
                }
            }
            if !ctx.row_canonical(&method.row) {
                return Err(ierr("a method row is not canonical".to_string()));
            }
        }
    }
    let mut pending: Vec<usize> = module
        .interfaces
        .iter()
        .map(|interface| interface.parents.len())
        .collect();
    let mut children = vec![Vec::new(); module.interfaces.len()];
    for (child, interface) in module.interfaces.iter().enumerate() {
        for parent in &interface.parents {
            children[parent.interface as usize].push(child);
        }
    }
    let mut queue: std::collections::VecDeque<usize> = pending
        .iter()
        .enumerate()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut depth = vec![0usize; module.interfaces.len()];
    let mut uses_self: Vec<bool> = module
        .interfaces
        .iter()
        .map(|contract| {
            contract.methods.iter().any(|method| {
                method
                    .params
                    .iter()
                    .any(|ty| interface_type_uses_self(ctx, *ty))
                    || interface_type_uses_self(ctx, method.ret)
            })
        })
        .collect();
    let mut finished = 0usize;
    while let Some(parent) = queue.pop_front() {
        finished += 1;
        for child in &children[parent] {
            depth[*child] = depth[*child].max(depth[parent] + 1);
            uses_self[*child] |= uses_self[parent];
            if depth[*child] > 128 {
                return Err(terr("interface inheritance exceeds 128 levels".to_string()));
            }
            pending[*child] -= 1;
            if pending[*child] == 0 {
                queue.push_back(*child);
            }
        }
    }
    if finished != module.interfaces.len() {
        return Err(terr("interface inheritance contains a cycle".to_string()));
    }
    if module.func_bounds.len() != module.funcs.len() {
        return Err(terr(
            "the function-bound table does not match the function table".to_string(),
        ));
    }
    for (fidx, bounds) in module.func_bounds.iter().enumerate() {
        let func = &module.funcs[fidx];
        if bounds.len() != func.type_params as usize {
            return Err(err(
                fidx as u32,
                format!(
                    "the type-bound table has {} entries for `{}`, which has {} type parameters",
                    bounds.len(),
                    func.name,
                    func.type_params
                ),
            ));
        }
        for (parameter, items) in bounds.iter().enumerate() {
            let mut seen = HashSet::new();
            for bound in items {
                ctx.check_interface_use(bound, func.type_params, func.effect_params)
                    .map_err(|message| err(fidx as u32, message))?;
                if !seen.insert(bound.interface) {
                    return Err(err(
                        fidx as u32,
                        "one type parameter repeats an interface bound",
                    ));
                }
                let receiver = ctx.intern(BcType::Var(parameter as u32));
                if !ctx.interface_arguments_meet_bounds(receiver, bound, bounds) {
                    return Err(err(
                        fidx as u32,
                        "an interface bound has arguments outside their bounds",
                    ));
                }
            }
        }
    }
    if module.class_bounds.len() != module.classes.len() {
        return Err(terr(
            "the class-bound table does not match the class table".to_string(),
        ));
    }
    for (cidx, bounds) in module.class_bounds.iter().enumerate() {
        let class = &module.classes[cidx];
        let cerr = |message: String| {
            terr(format!(
                "{}: {message}",
                named_table_subject("class", &class.name, cidx)
            ))
        };
        if bounds.len() != class.type_params as usize {
            return Err(cerr(
                "the type-bound table does not match the type arity".to_string(),
            ));
        }
        for (parameter, items) in bounds.iter().enumerate() {
            let mut seen = HashSet::new();
            for bound in items {
                ctx.check_interface_use(bound, class.type_params, 0)
                    .map_err(&cerr)?;
                if !seen.insert(bound.interface) {
                    return Err(cerr(
                        "one type parameter repeats an interface bound".to_string(),
                    ));
                }
                let receiver = ctx.intern(BcType::Var(parameter as u32));
                if !ctx.interface_arguments_meet_bounds(receiver, bound, bounds) {
                    return Err(cerr(
                        "an interface bound has arguments outside their bounds".to_string(),
                    ));
                }
            }
        }
    }
    Ok(uses_self)
}

/// The import slots and the signatures the class checks read.
fn verify_imports(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    // Validate the import slots. Each slot names one definition of its
    // own kind, and no definition takes two slots. An imported
    // definition carries a signature and no body: the linker replaces
    // it with the provider definition, and the loader admits a module
    // only when the import table is empty.
    let extern_funcs = module.extern_funcs();
    {
        let mut claimed_classes = vec![false; module.classes.len()];
        let mut claimed_funcs = vec![false; module.funcs.len()];
        for (idx, import) in module.imports.iter().enumerate() {
            let ierr = |message: String| terr(format!("import {idx}: {message}"));
            if import.module.is_empty() || import.name.is_empty() {
                return Err(ierr("the slot needs a module path and a name".to_string()));
            }
            let claimed = if import.kind.is_func() {
                &mut claimed_funcs
            } else {
                &mut claimed_classes
            };
            let at = import.def as usize;
            if at >= claimed.len() {
                return Err(ierr("the definition index is out of range".to_string()));
            }
            if claimed[at] {
                return Err(ierr(
                    "the definition already has an import slot".to_string(),
                ));
            }
            claimed[at] = true;
        }
    }
    if extern_funcs.get(module.entry as usize) == Some(&true) {
        return Err(terr("the entry function cannot be imported".to_string()));
    }
    // Class and interface checks inspect function signatures. Reject
    // invalid indices before those checks use the type universe.
    for (fidx, func) in module.funcs.iter().enumerate() {
        for ty in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain(func.local_types.iter())
            .chain([&func.ret])
        {
            if *ty as usize >= module.types.len() {
                return Err(err(
                    fidx as u32,
                    "the signature references an invalid type index",
                ));
            }
        }
    }
    Ok(())
}

/// The class table.
fn verify_classes(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    let core = ctx.core;
    let extern_classes = module.extern_classes();
    let extern_funcs = module.extern_funcs();
    // Validate classes.
    for (cidx, class) in module.classes.iter().enumerate() {
        let cerr = |message: String| {
            terr(format!(
                "{}: {message}",
                named_table_subject("class", &class.name, cidx)
            ))
        };
        if extern_classes[cidx] {
            if let Some(p) = class.parent() {
                if !extern_classes[p as usize] {
                    return Err(cerr(
                        "an imported class cannot inherit a local class".to_string(),
                    ));
                }
            }
        }
        match class.kind {
            BcClassKind::Abstract => {
                if class.is_final {
                    return Err(cerr("an abstract class cannot be final".to_string()));
                }
                if class.parent().is_some() {
                    return Err(cerr("an abstract class cannot inherit".to_string()));
                }
            }
            BcClassKind::Case => {
                if class.is_final {
                    return Err(cerr("a case class cannot use the final flag".to_string()));
                }
                let Some(p) = class.parent() else {
                    return Err(cerr("a case class needs its enum parent".to_string()));
                };
                if p as usize >= cidx {
                    return Err(cerr(format!("parent {p} is not an earlier class entry")));
                }
                if module.classes[p as usize].kind != BcClassKind::Abstract {
                    return Err(cerr(
                        "a case class parent must be an abstract enum parent".to_string(),
                    ));
                }
                if module.classes[p as usize].type_params != class.type_params {
                    return Err(cerr(
                        "a case class must keep the family type arity".to_string(),
                    ));
                }
            }
            BcClassKind::Normal => {}
        }
        if class.is_frozen {
            if !class.is_final {
                return Err(cerr("a frozen class must also be final".to_string()));
            }
            if class.kind != BcClassKind::Normal {
                return Err(cerr("only a normal class can be frozen".to_string()));
            }
            if class.parent().is_some() {
                return Err(cerr("a frozen class cannot declare a parent".to_string()));
            }
            if !extern_classes[cidx] {
                let constructors = &ctx.class_constructors[cidx];
                if constructors.len() != 1 {
                    return Err(cerr(
                        "a frozen class needs one named constructor".to_string(),
                    ));
                }
                if constructors[0] as usize >= module.funcs.len() {
                    return Err(cerr(
                        "the frozen class constructor does not exist".to_string(),
                    ));
                }
                let constructor = &module.funcs[constructors[0] as usize];
                if !constructor.blocks.iter().flatten().any(|instruction| {
                    matches!(instruction, Instr::Extended(ExtendedInstr::SealInstance))
                }) {
                    return Err(cerr(
                        "the frozen class constructor does not seal its instance".to_string(),
                    ));
                }
            }
        }
        if let Some(p) = class.parent() {
            if p as usize >= cidx {
                return Err(cerr(format!("parent {p} is not an earlier class entry")));
            }
            let parent = &module.classes[p as usize];
            if parent.is_final {
                return Err(cerr("a class cannot inherit a final class".to_string()));
            }
            if class.kind != BcClassKind::Case {
                let native_text_child =
                    Some(cidx as u32) == core.string || Some(cidx as u32) == core.substring;
                if Some(p) == core.text && !native_text_child {
                    return Err(cerr(
                        "only String and Substring can inherit Text".to_string(),
                    ));
                }
                if parent.kind != BcClassKind::Normal && Some(p) != core.text {
                    return Err(cerr(
                        "only a case class may inherit a sealed enum class".to_string(),
                    ));
                }
                // A generic parent carries one closed type argument per
                // parameter. A generic class still declares no parent,
                // so no parent argument holds a type variable.
                if class.type_params != 0 {
                    return Err(cerr("a generic class cannot declare a parent".to_string()));
                }
                if class.parent_args.len() != parent.type_params as usize {
                    return Err(cerr(
                        "the parent type argument count does not match the parent arity"
                            .to_string(),
                    ));
                }
                for arg in &class.parent_args {
                    if *arg as usize >= module.types.len() {
                        return Err(cerr("a parent type argument is out of range".to_string()));
                    }
                    if !ctx.vars_bounded(*arg, 0, 0) {
                        return Err(cerr(
                            "a parent type argument holds a type variable".to_string(),
                        ));
                    }
                }
            } else if !class.parent_args.is_empty() {
                return Err(cerr(
                    "a case class carries no parent type argument".to_string(),
                ));
            }
            // The field layout must extend the parent layout exactly.
            // A generic parent contributes its fields with the declared
            // arguments applied.
            let inherited: Vec<(String, u32)> = parent
                .fields
                .iter()
                .map(|(name, ty)| (name.clone(), ctx.subst(*ty, &class.parent_args, &[])))
                .collect();
            if class.fields.len() < inherited.len()
                || class.fields[..inherited.len()] != inherited[..]
            {
                return Err(cerr(
                    "the field layout does not extend the parent layout".to_string(),
                ));
            }
        } else if !class.parent_args.is_empty() {
            return Err(cerr(
                "a class without a parent carries no parent type argument".to_string(),
            ));
        }
        for (fname, fty) in &class.fields {
            if *fty as usize >= module.types.len() {
                return Err(cerr(format!("field `{fname}` has an invalid type index")));
            }
            if !ctx.vars_bounded(*fty, class.type_params, 0) {
                return Err(cerr(format!(
                    "field `{fname}` uses a type variable outside the class arity"
                )));
            }
            if !ctx.projections_proven(*fty, &module.class_bounds[cidx]) {
                return Err(cerr(format!(
                    "field `{fname}` uses an associated type without its interface bound"
                )));
            }
            if ctx.stores_callback(*fty) {
                return Err(cerr(format!(
                    "field `{fname}` cannot store a nonescaping callback"
                )));
            }
            if class.is_frozen && !ctx.type_always_frozen(*fty, true) {
                return Err(cerr(format!(
                    "frozen class field `{fname}` has a type that is not always frozen"
                )));
            }
        }
        // The canonical self type of the class.
        let own_ty = ctx.class_self_type(cidx as u32);
        let mut seen = Vec::new();
        for (sel, func) in &class.methods {
            if *sel as usize >= module.selectors.len() {
                return Err(cerr(format!("selector {sel} does not exist")));
            }
            if seen.contains(sel) {
                return Err(cerr(format!("selector {sel} appears twice")));
            }
            seen.push(*sel);
            if *func as usize >= module.funcs.len() {
                return Err(cerr(format!("method function {func} does not exist")));
            }
            if extern_funcs[*func as usize] != extern_classes[cidx] {
                return Err(cerr(format!(
                    "method function {func} does not follow the import state of \
                     its class"
                )));
            }
            let f = &module.funcs[*func as usize];
            if class.is_frozen && f.param_muts.first().copied() == Some(true) {
                return Err(cerr(format!(
                    "frozen class method function {func} has a mutable receiver"
                )));
            }
            if !f.captures.is_empty() {
                return Err(cerr("a method function must not have captures".to_string()));
            }
            if f.type_params < class.type_params {
                return Err(cerr(format!(
                    "method function {func} does not carry the class type arity"
                )));
            }
            let self_ok = match (f.params.first(), own_ty) {
                (Some(p0), Some(t)) => *p0 == t,
                _ => false,
            };
            if !self_ok {
                return Err(cerr(format!(
                    "method function {func} does not receive this class as `self`"
                )));
            }
            // Override compatibility with the nearest ancestor method.
            // The base signature is read in the subclass view, so a
            // generic ancestor compares with its arguments applied.
            if let Some(parent) = class.parent() {
                if let Some(base_func) = ctx.find_method(parent, *sel) {
                    let base = &module.funcs[base_func as usize];
                    let owner = ctx
                        .method_owner(parent, *sel)
                        .expect("the base method has a declaring class");
                    let owner_arity = module.classes[owner as usize].type_params;
                    let start_args = ctx.declared_parent_args(cidx as u32);
                    let owner_args = ctx
                        .ancestor_args(parent, &start_args, owner)
                        .expect("the declaring class is an ancestor");
                    let Some(own_count) = base.type_params.checked_sub(owner_arity) else {
                        return Err(cerr(format!(
                            "the base of selector {sel} does not carry its class type arity"
                        )));
                    };
                    if f.type_params != class.type_params + own_count
                        || base.effect_params != f.effect_params
                    {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the generic arity"
                        )));
                    }
                    let mut targs = owner_args;
                    for i in 0..own_count {
                        targs.push(ctx.intern(BcType::Var(class.type_params + i)));
                    }
                    let base_params: Vec<u32> = base
                        .params
                        .iter()
                        .map(|p| ctx.subst(*p, &targs, &[]))
                        .collect();
                    if base_params.len() != f.params.len() || base_params[1..] != f.params[1..] {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter types"
                        )));
                    }
                    // `get` keeps a malformed marker vector from
                    // panicking before the signature validation runs.
                    if base.param_muts.get(1..) != f.param_muts.get(1..) {
                        return Err(cerr(format!(
                            "override of selector {sel} changes the parameter mut markers"
                        )));
                    }
                    let base_ret = ctx.subst(base.ret, &targs, &[]);
                    if !ctx.is_subtype(f.ret, base_ret) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the result type"
                        )));
                    }
                    if !ctx.row_included(&f.row, &base.row) {
                        return Err(cerr(format!(
                            "override of selector {sel} widens the effect row"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

fn interface_type_uses_self(ctx: &Ctx<'_>, root: u32) -> bool {
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(ty) = stack.pop() {
        if !seen.insert(ty) {
            continue;
        }
        match ctx.ty(ty) {
            BcType::Var(0) => return true,
            BcType::Inst(_, args) | BcType::Tuple(args) => stack.extend(args),
            BcType::List(item)
            | BcType::Run(item)
            | BcType::Wait(item)
            | BcType::RunSnapshot(item) => stack.push(item),
            BcType::Map(key, value)
            | BcType::PendingCall(key, value)
            | BcType::Handle(key, value) => {
                stack.push(key);
                stack.push(value);
            }
            BcType::Fn(params, _, ret, _) | BcType::Callback(params, _, ret, _) => {
                stack.extend(params);
                stack.push(ret);
            }
            BcType::Op(_, callable) => stack.push(callable),
            BcType::Projection { .. } => {}
            _ => {}
        }
    }
    false
}

/// Test whether one conformance premise set provides another set.
fn conformance_premises_imply(
    available: &[lm_bytecode::BcConformancePremise],
    required: &[lm_bytecode::BcConformancePremise],
) -> bool {
    required.iter().all(|premise| {
        available
            .iter()
            .find(|candidate| candidate.param == premise.param)
            .is_some_and(|candidate| {
                premise
                    .bounds
                    .iter()
                    .all(|bound| candidate.bounds.contains(bound))
            })
    })
}

/// Test whether one bound table provides another table.
fn conformance_bounds_imply(
    available: &[Vec<BcInterfaceUse>],
    required: &[Vec<BcInterfaceUse>],
) -> bool {
    required.iter().enumerate().all(|(index, bounds)| {
        available
            .get(index)
            .is_some_and(|actual| bounds.iter().all(|bound| actual.contains(bound)))
    })
}

/// Conformance references and their method witnesses.
fn verify_conformances(ctx: &Ctx<'_>, interface_self: &[bool]) -> Result<(), VerifyError> {
    let module = ctx.module;
    // Validate all direct conformance references first. One conformance
    // can resolve another conformance during its semantic checks.
    for (index, conformance) in module.conformances.iter().enumerate() {
        let cerr = |message: String| {
            terr(format!(
                "{}: {message}",
                conformance_subject(module, index, conformance)
            ))
        };
        let Some(class) = module.classes.get(conformance.class as usize) else {
            return Err(cerr("the class index is out of range".to_string()));
        };
        ctx.check_interface_use(&conformance.application, class.type_params, 0)
            .map_err(&cerr)?;
        let mut seen_params = HashSet::new();
        for premise in &conformance.premises {
            if premise.param >= class.type_params {
                return Err(cerr("a premise parameter is out of range".to_string()));
            }
            if !seen_params.insert(premise.param) {
                return Err(cerr("a premise parameter appears twice".to_string()));
            }
            if premise.bounds.is_empty() {
                return Err(cerr("a premise has no interface bound".to_string()));
            }
            let mut seen_interfaces = HashSet::new();
            for bound in &premise.bounds {
                ctx.check_interface_use(bound, class.type_params, 0)
                    .map_err(&cerr)?;
                if !seen_interfaces.insert(bound.interface) {
                    return Err(cerr("a premise repeats one interface bound".to_string()));
                }
            }
        }
        let contract = &module.interfaces[conformance.application.interface as usize];
        if conformance.associated.len() != contract.associated.len() {
            return Err(cerr(
                "the associated bindings do not match the contract".to_string(),
            ));
        }
        if conformance
            .associated
            .iter()
            .any(|ty| *ty as usize >= module.types.len())
        {
            return Err(cerr("an associated binding is out of range".to_string()));
        }
    }
    // Validate each explicit conformance and its method witnesses.
    let iterable_interface = module
        .interfaces
        .iter()
        .position(|interface| interface.key == "core.Iterable");
    let iterator_interface = module
        .interfaces
        .iter()
        .position(|interface| interface.key == "core.Iterator");
    let mut conformance_keys = HashSet::new();
    for (index, conformance) in module.conformances.iter().enumerate() {
        let cerr = |message: String| {
            terr(format!(
                "{}: {message}",
                conformance_subject(module, index, conformance)
            ))
        };
        let class = &module.classes[conformance.class as usize];
        if !conformance_keys.insert((conformance.class, conformance.application.interface)) {
            return Err(cerr(
                "the class repeats one interface conformance".to_string(),
            ));
        }
        let contract = &module.interfaces[conformance.application.interface as usize];
        let mut conformance_bounds = module.class_bounds[conformance.class as usize].clone();
        for premise in &conformance.premises {
            let Some(bounds) = conformance_bounds.get_mut(premise.param as usize) else {
                return Err(cerr("a premise parameter is out of range".to_string()));
            };
            for bound in &premise.bounds {
                if !bounds.contains(bound) {
                    bounds.push(bound.clone());
                }
            }
        }
        if class.kind == BcClassKind::Normal
            && !class.is_final
            && interface_self[conformance.application.interface as usize]
        {
            return Err(cerr(
                "a non-final class conforms to a Self-dependent interface".to_string(),
            ));
        }
        let mut contract_types = Vec::with_capacity(conformance.application.types.len() + 1);
        let self_ty = ctx
            .class_self_type(conformance.class)
            .ok_or_else(|| cerr("the class has no canonical self type".to_string()))?;
        contract_types.push(self_ty);
        contract_types.extend_from_slice(&conformance.application.types);
        for parent in &contract.parents {
            let required = ctx.subst_interface_use_with_bounds(
                parent,
                &contract_types,
                &conformance.application.rows,
                &conformance_bounds,
            );
            let found = ctx.direct_conformance(conformance.class, required.interface);
            let valid = found.is_some_and(|candidate| {
                candidate.application == required
                    && conformance_premises_imply(&conformance.premises, &candidate.premises)
            });
            if !valid {
                return Err(cerr(
                    "the conformance omits one parent interface".to_string(),
                ));
            }
        }
        for ty in &conformance.associated {
            if !ctx.vars_bounded(*ty, class.type_params, 0) {
                return Err(cerr(
                    "an associated binding uses an unbound variable".to_string(),
                ));
            }
            if ctx.stores_callback(*ty) {
                return Err(cerr(
                    "an associated binding cannot contain a callback".to_string(),
                ));
            }
            if !ctx.projections_proven(*ty, &conformance_bounds) {
                return Err(cerr(
                    "an associated binding uses an unproven associated type".to_string(),
                ));
            }
        }
        if !ctx.interface_arguments_meet_bounds(
            self_ty,
            &conformance.application,
            &conformance_bounds,
        ) {
            return Err(cerr(
                "the interface arguments do not meet their bounds".to_string(),
            ));
        }
        for (associated, actual) in contract.associated.iter().zip(&conformance.associated) {
            for bound in &associated.bounds {
                let required = ctx.subst_interface_use_with_bounds(
                    bound,
                    &contract_types,
                    &conformance.application.rows,
                    &conformance_bounds,
                );
                let found = ctx.interface_application_with_bounds(
                    *actual,
                    required.interface,
                    &conformance_bounds,
                    0,
                );
                if found.as_ref() != Some(&required) {
                    return Err(cerr(format!(
                        "the associated binding `{}` does not meet one bound",
                        associated.name
                    )));
                }
            }
        }
        if iterable_interface == Some(conformance.application.interface as usize) {
            let iterator = iterator_interface.ok_or_else(|| {
                cerr("the core Iterable contract needs core Iterator".to_string())
            })?;
            let item_index = contract
                .associated
                .iter()
                .position(|item| item.name == "Item")
                .ok_or_else(|| cerr("the core Iterable contract needs Item".to_string()))?;
            let iter_index = contract
                .associated
                .iter()
                .position(|item| item.name == "Iter")
                .ok_or_else(|| cerr("the core Iterable contract needs Iter".to_string()))?;
            let iterator_item = module.interfaces[iterator]
                .associated
                .iter()
                .position(|item| item.name == "Item")
                .ok_or_else(|| cerr("the core Iterator contract needs Item".to_string()))?
                as u32;
            let item = conformance.associated[item_index];
            let iter = conformance.associated[iter_index];
            let actual = ctx
                .projected_type_with_bounds(
                    iter,
                    iterator as u32,
                    iterator_item,
                    &conformance_bounds,
                )
                .unwrap_or_else(|| {
                    ctx.intern(BcType::Projection {
                        base: iter,
                        interface: iterator as u32,
                        assoc: iterator_item,
                    })
                });
            if actual != item {
                return Err(cerr(
                    "Iterable.Item must equal Iterable.Iter.Item".to_string(),
                ));
            }
        }
        let class_args: Vec<u32> = (0..class.type_params)
            .map(|item| ctx.intern(BcType::Var(item)))
            .collect();
        for requirement in &contract.methods {
            let merr = |message: &str| {
                let method_name = module
                    .selectors
                    .get(requirement.selector as usize)
                    .map(String::as_str)
                    .unwrap_or("<invalid selector>");
                terr(format!(
                    "the method `{method_name}` of `{}` does not satisfy `{}`: \
                     {message} (conformance table {index})",
                    class.name, contract.name
                ))
            };
            let (owner, target) = ctx
                .method_resolution(conformance.class, requirement.selector)
                .ok_or_else(|| merr("the implementation is missing"))?;
            let owner_args = ctx
                .ancestor_args(conformance.class, &class_args, owner)
                .ok_or_else(|| merr("the implementation owner is not an ancestor"))?;
            let method = &module.funcs[target as usize];
            if method.type_params != module.classes[owner as usize].type_params
                || method.effect_params != 0
            {
                return Err(merr("the implementation adds generic parameters"));
            }
            let class_bound_count = module.classes[owner as usize].type_params as usize;
            let Some(method_bounds) = module.func_bounds.get(target as usize) else {
                return Err(merr("the implementation has no generic bound table"));
            };
            let Some(class_method_bounds) = method_bounds.get(..class_bound_count) else {
                return Err(merr("the implementation has incomplete class bounds"));
            };
            let bounds_hold = if owner == conformance.class {
                conformance_bounds_imply(&conformance_bounds, class_method_bounds)
            } else {
                ctx.type_arguments_meet_bounds(
                    &owner_args,
                    &[],
                    class_method_bounds,
                    &conformance_bounds,
                )
            };
            if !bounds_hold {
                return Err(merr("the implementation needs an undeclared premise"));
            }
            if method.params.len() != requirement.params.len() + 1 {
                return Err(merr("the parameter count differs"));
            }
            if method.param_muts.len() != method.params.len() {
                return Err(merr("the implementation has invalid parameter mutability"));
            }
            if method.param_muts.first().copied() != Some(requirement.mut_self) {
                let receiver = if requirement.mut_self {
                    "`mut self`"
                } else {
                    "`self`"
                };
                return Err(merr(&format!("the contract requires {receiver}")));
            }
            if method.param_muts[1..] != requirement.param_muts[..] {
                return Err(merr("parameter mutability differs"));
            }
            let params_match = method.params[1..]
                .iter()
                .zip(&requirement.params)
                .zip(&requirement.param_muts)
                .all(|((implementation, required), mutable)| {
                    let implementation = ctx.subst(*implementation, &owner_args, &[]);
                    let required = ctx.subst_with_bounds(
                        *required,
                        &contract_types,
                        &conformance.application.rows,
                        &conformance_bounds,
                    );
                    if *mutable {
                        implementation == required
                    } else {
                        ctx.is_subtype(required, implementation)
                    }
                });
            if !params_match {
                return Err(merr("parameter types differ"));
            }
            let actual_ret = ctx.subst(method.ret, &owner_args, &[]);
            let required_ret = ctx.subst_with_bounds(
                requirement.ret,
                &contract_types,
                &conformance.application.rows,
                &conformance_bounds,
            );
            if !ctx.is_subtype(actual_ret, required_ret) {
                return Err(merr("the result type is too wide"));
            }
            let required_row = ctx.row_subst(&requirement.row, &conformance.application.rows);
            if !ctx.row_included(&method.row, &required_row) {
                return Err(merr("the effect row is too wide"));
            }
        }
    }
    Ok(())
}

/// The core role slots, function signatures, and local types.
fn verify_signatures(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    let extern_funcs = module.extern_funcs();
    // Validate the declared core role slots. The class table is
    // validated above, so every field type index is inside the type
    // table by now.
    verify_core_roles(module)?;
    // Validate function signatures and the declared local-type
    // tables. The verifier validates the table instead of trusting
    // it: entries must be valid types, the parameter prefix must
    // equal the signature, and every variable must be in scope.
    for (fidx, func) in module.funcs.iter().enumerate() {
        if extern_funcs[fidx] {
            // An imported function is a declaration: a signature with
            // no body, no captures, and no extra local slots.
            if !func.blocks.is_empty() {
                return Err(err(fidx as u32, "an imported function must have no body"));
            }
            if !func.captures.is_empty() {
                return Err(err(
                    fidx as u32,
                    "an imported function must have no captures",
                ));
            }
            if func.local_types.len() != func.params.len() {
                return Err(err(
                    fidx as u32,
                    "an imported function must declare only its parameter slots",
                ));
            }
        }
        if func.param_muts.len() != func.params.len() {
            return Err(err(
                fidx as u32,
                "the parameter mut markers do not align with the parameters",
            ));
        }
        if func.local_types.len() < func.params.len() {
            return Err(err(fidx as u32, "more parameters than local slots"));
        }
        if func.local_types[..func.params.len()] != func.params[..] {
            return Err(err(
                fidx as u32,
                "the local-type table prefix does not equal the parameter types",
            ));
        }
        for t in func
            .params
            .iter()
            .chain(func.captures.iter())
            .chain(func.local_types.iter())
            .chain([&func.ret])
        {
            if *t as usize >= module.types.len() {
                return Err(err(
                    fidx as u32,
                    "the signature references an invalid type index",
                ));
            }
            if !ctx.vars_bounded(*t, func.type_params, func.effect_params) {
                return Err(err(
                    fidx as u32,
                    "the signature uses a variable outside the declared generic arity",
                ));
            }
            if !ctx.projections_proven(*t, &module.func_bounds[fidx]) {
                return Err(err(
                    fidx as u32,
                    "the signature uses an associated type without its interface bound",
                ));
            }
        }
        for ty in &func.params {
            if ctx.stores_callback(*ty) && !matches!(ctx.ty(*ty), BcType::Callback(..)) {
                return Err(err(
                    fidx as u32,
                    "a callback must be a direct function parameter",
                ));
            }
        }
        if func.captures.iter().any(|ty| ctx.stores_callback(*ty)) {
            return Err(err(
                fidx as u32,
                "a closure cannot capture a nonescaping callback",
            ));
        }
        if func.local_types[func.params.len()..]
            .iter()
            .any(|ty| ctx.stores_callback(*ty))
        {
            return Err(err(
                fidx as u32,
                "a local cannot store a nonescaping callback",
            ));
        }
        if ctx.stores_callback(func.ret) {
            return Err(err(
                fidx as u32,
                "a function cannot return a nonescaping callback",
            ));
        }
        for elem in &func.row {
            match elem {
                BcRow::Op(s) => {
                    if *s as usize >= module.strings.len() {
                        return Err(err(
                            fidx as u32,
                            "the declared row references an invalid string index",
                        ));
                    }
                    if !ctx.bundle.row_name_valid(&module.strings[*s as usize]) {
                        return Err(err(
                            fidx as u32,
                            format!(
                                "the declared row names `{}`, which is not in the \
                                 operation manifest",
                                module.strings[*s as usize]
                            ),
                        ));
                    }
                }
                BcRow::Var(v) => {
                    if *v >= func.effect_params {
                        return Err(err(
                            fidx as u32,
                            "the declared row uses an effect variable outside the arity",
                        ));
                    }
                }
            }
        }
        if !ctx.row_canonical(&func.row) {
            return Err(err(fidx as u32, "the declared row is not canonical"));
        }
    }
    Ok(())
}

/// The immutable contracts and initial targets of late-bound slots.
fn verify_slots(ctx: &Ctx<'_>) -> Result<(), VerifyError> {
    let module = ctx.module;
    let mut keys = HashSet::new();
    for (slot, spec) in module.slots.iter().enumerate() {
        let serr = |message: String| terr(format!("slot {slot}: {message}"));
        if !keys.insert(spec.key) {
            return Err(serr("the slot key appears twice".to_string()));
        }
        match &spec.contract {
            SlotContract::Function(contract) | SlotContract::Method(contract) => {
                verify_callable_contract(ctx, slot, contract)?;
                if let Some(SlotTarget::Function(target)) = spec.initial {
                    let func = module
                        .funcs
                        .get(target as usize)
                        .ok_or_else(|| serr("the function target does not exist".to_string()))?;
                    if !func.captures.is_empty() {
                        return Err(serr("the function target has captures".to_string()));
                    }
                    if !callable_matches(module, target, contract) {
                        return Err(serr(
                            "the function target does not match the slot contract".to_string(),
                        ));
                    }
                    if matches!(&spec.contract, SlotContract::Method(_))
                        && !module
                            .classes
                            .iter()
                            .any(|class| class.methods.iter().any(|(_, func)| *func == target))
                    {
                        return Err(serr("the method target is not a class method".to_string()));
                    }
                } else if spec.initial.is_some() {
                    return Err(serr("the initial target has the wrong kind".to_string()));
                }
            }
            SlotContract::Class {
                type_params,
                abi,
                ty,
                constructor,
            } => {
                if spec.contract_hash != *abi {
                    return Err(serr(
                        "the class contract identity differs from its ABI".to_string(),
                    ));
                }
                verify_slot_type(ctx, slot, *ty, *type_params, 0, &[], false)?;
                verify_callable_contract(ctx, slot, constructor)?;
                if constructor.type_params != *type_params
                    || constructor.effect_params != 0
                    || constructor.ret != *ty
                {
                    return Err(serr(
                        "the class constructor contract does not match the class type".to_string(),
                    ));
                }
                let Some((contract_class, args)) = ctx.as_instance(*ty) else {
                    return Err(serr("the class contract type is not a class".to_string()));
                };
                if args.len() != *type_params as usize
                    || !args.iter().enumerate().all(|(index, arg)| {
                        matches!(ctx.ty(*arg), BcType::Var(found) if found == index as u32)
                    })
                {
                    return Err(serr(
                        "the class contract type does not bind its parameters".to_string(),
                    ));
                }
                if let Some(SlotTarget::Class {
                    class: target,
                    constructor: target_constructor,
                }) = spec.initial
                {
                    let class = module
                        .classes
                        .get(target as usize)
                        .ok_or_else(|| serr("the class target does not exist".to_string()))?;
                    if class.type_params != *type_params || target != contract_class {
                        return Err(serr(
                            "the class target does not match the slot type".to_string(),
                        ));
                    }
                    let function =
                        module
                            .funcs
                            .get(target_constructor as usize)
                            .ok_or_else(|| {
                                serr("the class constructor target does not exist".to_string())
                            })?;
                    if !function.captures.is_empty()
                        || !callable_matches(module, target_constructor, constructor)
                    {
                        return Err(serr(
                            "the class constructor target does not match the slot contract"
                                .to_string(),
                        ));
                    }
                } else if spec.initial.is_some() {
                    return Err(serr("the initial target has the wrong kind".to_string()));
                }
            }
            SlotContract::Value { ty } => {
                verify_slot_type(ctx, slot, *ty, 0, 0, &[], false)?;
                if spec.initial.is_some() {
                    return Err(serr(
                        "a value slot cannot have a portable initial value".to_string(),
                    ));
                }
            }
            SlotContract::Process { message, result } => {
                verify_slot_type(ctx, slot, *message, 0, 0, &[], false)?;
                verify_slot_type(ctx, slot, *result, 0, 0, &[], false)?;
                if spec.initial.is_some() {
                    return Err(serr(
                        "a process slot cannot have a portable initial process".to_string(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn verify_callable_contract(
    ctx: &Ctx<'_>,
    slot: usize,
    contract: &BcCallableContract,
) -> Result<(), VerifyError> {
    let serr = |message: String| terr(format!("slot {slot}: {message}"));
    if contract.param_muts.len() != contract.params.len() {
        return Err(serr(
            "the parameter mut markers do not align with the parameters".to_string(),
        ));
    }
    if contract.type_bounds.len() != contract.type_params as usize {
        return Err(serr(
            "the type-bound table does not match the type arity".to_string(),
        ));
    }
    for parameter in &contract.type_bounds {
        let mut seen = HashSet::new();
        for bound in parameter {
            ctx.check_interface_use(bound, contract.type_params, contract.effect_params)
                .map_err(&serr)?;
            if !seen.insert(bound.interface) {
                return Err(serr(
                    "one type parameter repeats an interface bound".to_string(),
                ));
            }
        }
    }
    for ty in &contract.params {
        verify_slot_type(
            ctx,
            slot,
            *ty,
            contract.type_params,
            contract.effect_params,
            &contract.type_bounds,
            true,
        )?;
    }
    verify_slot_type(
        ctx,
        slot,
        contract.ret,
        contract.type_params,
        contract.effect_params,
        &contract.type_bounds,
        false,
    )?;
    for elem in &contract.row {
        match elem {
            BcRow::Op(name) => {
                let Some(name) = ctx.module.strings.get(*name as usize) else {
                    return Err(serr("the effect row has an invalid name".to_string()));
                };
                if !ctx.bundle.row_name_valid(name) {
                    return Err(serr(
                        "the effect row names an operation outside the manifest".to_string(),
                    ));
                }
            }
            BcRow::Var(variable) if *variable >= contract.effect_params => {
                return Err(serr(
                    "the effect row uses a variable outside the declared arity".to_string(),
                ));
            }
            BcRow::Var(_) => {}
        }
    }
    if !ctx.row_canonical(&contract.row) {
        return Err(serr("the effect row is not canonical".to_string()));
    }
    Ok(())
}

fn verify_slot_type(
    ctx: &Ctx<'_>,
    slot: usize,
    ty: u32,
    type_params: u32,
    effect_params: u32,
    bounds: &[Vec<BcInterfaceUse>],
    allow_direct_callback: bool,
) -> Result<(), VerifyError> {
    let serr = |message: String| terr(format!("slot {slot}: {message}"));
    if ty as usize >= ctx.module.types.len() {
        return Err(serr("the contract has an invalid type index".to_string()));
    }
    if !ctx.vars_bounded(ty, type_params, effect_params) {
        return Err(serr(
            "the contract uses a variable outside the declared arity".to_string(),
        ));
    }
    if !ctx.projections_proven(ty, bounds) {
        return Err(serr(
            "the contract uses an associated type without its interface bound".to_string(),
        ));
    }
    if ctx.stores_callback(ty)
        && !(allow_direct_callback && matches!(ctx.ty(ty), BcType::Callback(..)))
    {
        return Err(serr(
            "the contract stores a nonescaping callback".to_string(),
        ));
    }
    Ok(())
}

fn callable_matches(module: &Module, target: u32, contract: &BcCallableContract) -> bool {
    let func = &module.funcs[target as usize];
    func.type_params == contract.type_params
        && func.effect_params == contract.effect_params
        && module.func_bounds[target as usize] == contract.type_bounds
        && func.params == contract.params
        && func.param_muts == contract.param_muts
        && func.ret == contract.ret
        && func.row == contract.row
}
