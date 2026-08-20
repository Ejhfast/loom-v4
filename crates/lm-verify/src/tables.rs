//! Structural validation of every module table.
//!
//! One part of the bytecode verifier. `lib.rs` holds the shared
//! context, the error type, and the entry points.

use super::*;

/// Validate the type, selector, application, class, and function
/// tables.
pub(crate) fn verify_tables(module: &Module, core: CoreLayout) -> Result<Ctx<'_>, VerifyError> {
    let terr = |message: String| VerifyError {
        func: u32::MAX,
        message,
    };
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
            BcType::Unit | BcType::Bool | BcType::Int | BcType::Str => {}
            BcType::Digest | BcType::Bytes | BcType::FileHandle | BcType::ResourceHandle => {}
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
                let text_key = match module.types[*k as usize] {
                    BcType::Class(class) => {
                        Some(class) == core.text || Some(class) == core.substring
                    }
                    _ => false,
                };
                if !text_key
                    && !matches!(
                        module.types[*k as usize],
                        BcType::Bool | BcType::Int | BcType::Str | BcType::Bytes | BcType::Var(_)
                    )
                {
                    return Err(terr(format!(
                        "type {idx} has a map key type outside Bool, Int, Text, String, Substring, or Bytes"
                    )));
                }
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
                        if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
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
            | BcType::EmptyVm
            | BcType::SnapshotImage => {}
            BcType::Vm(t) | BcType::Wait(t) | BcType::Snapshot(t) => check_ref(*t)?,
            BcType::PendingCall(a, r) | BcType::Handle(a, r) => {
                check_ref(*a)?;
                check_ref(*r)?;
            }
            BcType::Op(op, f) => {
                if *op >= lm_abi::OP_COUNT || lm_abi::op(*op).kind != lm_abi::OpKind::Fixed {
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
        class_ty,
        uni: RefCell::new(Universe {
            types: module.types.clone(),
            index,
            facts,
        }),
        core,
    };
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
                    lm_abi::op_name(*op)
                )));
            }
        }
    }
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
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
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
    // Validate nominal interface contracts before any bound uses them.
    let mut interface_keys: HashMap<&str, usize> = HashMap::new();
    for (iidx, contract) in module.interfaces.iter().enumerate() {
        let ierr = |message: String| terr(format!("interface {iidx}: {message}"));
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
        let mut interface_scope = vec![vec![self_application]];
        interface_scope.extend(contract.type_bounds.clone());
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
            if let Some(bound) = &associated.bound {
                ctx.check_interface_use(bound, contract.type_params + 1, contract.effect_params)
                    .map_err(&ierr)?;
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
                    if !lm_abi::row_name_valid(name) {
                        return Err(ierr("a method row names an unknown effect".to_string()));
                    }
                }
            }
            if !ctx.row_canonical(&method.row) {
                return Err(ierr("a method row is not canonical".to_string()));
            }
        }
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
        let cerr = |message: String| terr(format!("class {cidx}: {message}"));
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
    // Validate the import slots. Each slot names one definition of its
    // own kind, and no definition takes two slots. An imported
    // definition carries a signature and no body: the linker replaces
    // it with the provider definition, and the loader admits a module
    // only when the import table is empty.
    let extern_classes = module.extern_classes();
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
    // Validate classes.
    for (cidx, class) in module.classes.iter().enumerate() {
        let cerr = |message: String| terr(format!("class {cidx}: {message}"));
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
    // Validate all direct conformance references first. One conformance
    // can resolve another conformance during its semantic checks.
    for (index, conformance) in module.conformances.iter().enumerate() {
        let cerr = |message: String| terr(format!("conformance {index}: {message}"));
        let Some(class) = module.classes.get(conformance.class as usize) else {
            return Err(cerr("the class index is out of range".to_string()));
        };
        ctx.check_interface_use(&conformance.application, class.type_params, 0)
            .map_err(&cerr)?;
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
        let cerr = |message: String| terr(format!("conformance {index}: {message}"));
        let class = &module.classes[conformance.class as usize];
        if !conformance_keys.insert((conformance.class, conformance.application.interface)) {
            return Err(cerr(
                "the class repeats one interface conformance".to_string(),
            ));
        }
        let contract = &module.interfaces[conformance.application.interface as usize];
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
            if !ctx.projections_proven(*ty, &module.class_bounds[conformance.class as usize]) {
                return Err(cerr(
                    "an associated binding uses an unproven associated type".to_string(),
                ));
            }
        }
        let self_ty = ctx
            .class_self_type(conformance.class)
            .ok_or_else(|| cerr("the class has no canonical self type".to_string()))?;
        if !ctx.interface_arguments_meet_bounds(
            self_ty,
            &conformance.application,
            &module.class_bounds[conformance.class as usize],
        ) {
            return Err(cerr(
                "the interface arguments do not meet their bounds".to_string(),
            ));
        }
        let mut contract_types = Vec::with_capacity(conformance.application.types.len() + 1);
        contract_types.push(self_ty);
        contract_types.extend_from_slice(&conformance.application.types);
        for (associated, actual) in contract.associated.iter().zip(&conformance.associated) {
            let Some(bound) = &associated.bound else {
                continue;
            };
            let required =
                ctx.subst_interface_use(bound, &contract_types, &conformance.application.rows);
            let found = ctx.interface_application_with_bounds(
                *actual,
                required.interface,
                &module.class_bounds[conformance.class as usize],
                0,
            );
            if found.as_ref() != Some(&required) {
                return Err(cerr(format!(
                    "the associated binding `{}` does not meet its bound",
                    associated.name
                )));
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
                .projected_type(iter, iterator as u32, iterator_item)
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
            let target = ctx
                .find_method(conformance.class, requirement.selector)
                .ok_or_else(|| cerr("a required method is missing".to_string()))?;
            let owner = ctx
                .method_owner(conformance.class, requirement.selector)
                .ok_or_else(|| cerr("a required method has no owner".to_string()))?;
            let owner_args = ctx
                .ancestor_args(conformance.class, &class_args, owner)
                .ok_or_else(|| cerr("a required method owner is not an ancestor".to_string()))?;
            let method = &module.funcs[target as usize];
            if method.type_params != module.classes[owner as usize].type_params
                || method.effect_params != 0
            {
                return Err(cerr(
                    "an interface method implementation cannot add generics".to_string(),
                ));
            }
            if method.params.len() != requirement.params.len() + 1
                || method.param_muts.len() != method.params.len()
                || method.param_muts.first().copied() != Some(requirement.mut_self)
                || method.param_muts[1..] != requirement.param_muts[..]
            {
                return Err(cerr(
                    "an interface method implementation has a different parameter shape"
                        .to_string(),
                ));
            }
            let actual_params: Vec<u32> = method.params[1..]
                .iter()
                .map(|item| ctx.subst(*item, &owner_args, &[]))
                .collect();
            let required_params: Vec<u32> = requirement
                .params
                .iter()
                .map(|item| ctx.subst(*item, &contract_types, &conformance.application.rows))
                .collect();
            if actual_params != required_params {
                return Err(cerr(
                    "an interface method implementation changes parameter types".to_string(),
                ));
            }
            let actual_ret = ctx.subst(method.ret, &owner_args, &[]);
            let required_ret = ctx.subst(
                requirement.ret,
                &contract_types,
                &conformance.application.rows,
            );
            if !ctx.is_subtype(actual_ret, required_ret) {
                return Err(cerr(
                    "an interface method implementation widens the result type".to_string(),
                ));
            }
            let required_row = ctx.row_subst(&requirement.row, &conformance.application.rows);
            if !ctx.row_included(&method.row, &required_row) {
                return Err(cerr(
                    "an interface method implementation widens the effect row".to_string(),
                ));
            }
        }
    }
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
                    if !lm_abi::row_name_valid(&module.strings[*s as usize]) {
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
    Ok(ctx)
}
