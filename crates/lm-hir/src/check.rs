//! Module-level checking: predeclaration, signature resolution, the
//! pinned core image, and the prelude.
//!
//! The expression checker lives in `checkfn`. This file resolves the
//! module shape: classes, enums, generic signatures, effect rows, and
//! the order of definition indices. The core sources are ordinary
//! Loom code compiled by the same pipeline into every module, after
//! the user definitions, so user definition indices stay stable.

use crate::checkfn::{FnChecker, RetKind};
use crate::hir::*;
use lm_source::ast;
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{ClassId, ClassKind, Row, RowElem, Type, TypeId, TypeStore, NEVER, UNIT};
use std::collections::HashMap;
use std::sync::OnceLock;

/// The concatenated pinned core sources, in canonical file order.
pub const CORE_SOURCE: &str = concat!(
    include_str!("../../../core/option.lm"),
    "\n",
    include_str!("../../../core/result.lm"),
    "\n",
    include_str!("../../../core/ordering.lm"),
    "\n",
    include_str!("../../../core/pair.lm"),
    "\n",
    include_str!("../../../core/range.lm"),
    "\n",
    include_str!("../../../core/errors.lm"),
    "\n",
    include_str!("../../../core/vm.lm"),
    "\n",
);

/// The type names the prelude places into unqualified scope.
pub const PRELUDE_TYPES: [&str; 5] = ["Option", "Result", "Ordering", "Pair", "Range"];

/// The constructor names the prelude places into unqualified scope.
pub const PRELUDE_CTORS: [&str; 4] = ["Some", "None", "Ok", "Err"];

/// Checker options. `prelude` controls only unqualified name
/// resolution; the core image itself never depends on it.
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions {
    pub prelude: bool,
}

impl Default for CheckOptions {
    fn default() -> CheckOptions {
        CheckOptions { prelude: true }
    }
}

fn core_ast() -> &'static ast::Module {
    static CORE: OnceLock<ast::Module> = OnceLock::new();
    CORE.get_or_init(|| {
        let module = lm_source::parse::parse(CORE_SOURCE).expect("the core sources parse");
        assert!(module.entry.is_empty(), "the core has no entry expression");
        assert!(module.funcs.is_empty(), "the core has no free functions");
        module
    })
}

/// One callable signature. Methods include `self` as parameter zero.
/// For a method, the type parameters hold the class parameters first
/// and the method's own parameters after them.
#[derive(Clone)]
pub(crate) struct FnSig {
    pub(crate) type_params: Vec<String>,
    pub(crate) effect_params: Vec<String>,
    pub(crate) params: Vec<TypeId>,
    pub(crate) param_muts: Vec<bool>,
    /// Declared parameter names for labeled arguments. A method holds
    /// `self` at position zero.
    pub(crate) param_names: Vec<String>,
    pub(crate) ret: TypeId,
    pub(crate) row: Row,
}

/// One declared method, without `self` in the parameter lists.
#[derive(Clone)]
pub(crate) struct MethodSig {
    pub(crate) name: String,
    pub(crate) func: u32,
    pub(crate) mut_self: bool,
    pub(crate) params: Vec<TypeId>,
    pub(crate) param_muts: Vec<bool>,
    /// Declared parameter names, without `self`.
    pub(crate) param_names: Vec<String>,
    pub(crate) ret: TypeId,
    pub(crate) row: Row,
    pub(crate) own_type_params: Vec<String>,
    pub(crate) own_effect_params: Vec<String>,
}

/// Checker-side class information with the full field layout.
pub(crate) struct ClassInfo {
    pub(crate) name: String,
    pub(crate) parent: Option<u32>,
    pub(crate) type_params: Vec<String>,
    pub(crate) kind: ClassKind,
    /// The instance type seen by method bodies: `Class(c)` or
    /// `Inst(c, [Var 0..])`.
    pub(crate) self_ty: TypeId,
    pub(crate) field_names: Vec<String>,
    pub(crate) field_tys: Vec<TypeId>,
    pub(crate) has_default: Vec<bool>,
    /// The layout index where own fields start.
    pub(crate) own_start: usize,
    pub(crate) methods: Vec<MethodSig>,
    pub(crate) init: Option<MethodSig>,
    /// For an enum case: the family parent class index.
    pub(crate) family: Option<u32>,
    /// For an enum parent: the case class indices in arm order.
    pub(crate) arms: Vec<u32>,
    /// For an enum case: the short arm name, for example `Some`.
    pub(crate) arm_short: String,
}

/// The lexical resolution scope of the code being checked.
#[derive(Clone, Default)]
pub(crate) struct TyEnv {
    pub(crate) type_names: Vec<String>,
    pub(crate) effect_names: Vec<String>,
    /// True while checking core sources: names resolve against the
    /// core definitions only.
    pub(crate) core_scope: bool,
}

/// Shared module state for all function checkers.
pub(crate) struct Ctx {
    pub(crate) store: TypeStore,
    pub(crate) classes: Vec<ClassInfo>,
    pub(crate) user_types: HashMap<String, u32>,
    pub(crate) core_types: HashMap<String, u32>,
    pub(crate) prelude: bool,
    pub(crate) func_index: HashMap<String, u32>,
    pub(crate) sigs: Vec<FnSig>,
    pub(crate) funcs: Vec<Option<HirFunc>>,
    pub(crate) core: CoreIds,
}

impl Ctx {
    /// Look up a class or enum type name in the given scope.
    pub(crate) fn lookup_type(&self, name: &str, env: &TyEnv) -> Option<u32> {
        if env.core_scope {
            return self.core_types.get(name).copied();
        }
        if let Some(idx) = self.user_types.get(name) {
            return Some(*idx);
        }
        if self.prelude && PRELUDE_TYPES.contains(&name) {
            return self.core_types.get(name).copied();
        }
        None
    }

    /// Find a method by name, walking the ancestor chain.
    pub(crate) fn find_method(&self, mut class: u32, name: &str) -> Option<MethodSig> {
        loop {
            let info = &self.classes[class as usize];
            if let Some(m) = info.methods.iter().find(|m| m.name == name) {
                return Some(m.clone());
            }
            match info.parent {
                Some(p) => class = p,
                None => return None,
            }
        }
    }

    /// Find a field layout index by name.
    pub(crate) fn find_field(&self, class: u32, name: &str) -> Option<usize> {
        self.classes[class as usize]
            .field_names
            .iter()
            .position(|n| n == name)
    }

    /// The enum family index of a class, when it belongs to one.
    pub(crate) fn family_of(&self, class: u32) -> Option<u32> {
        let info = &self.classes[class as usize];
        match info.kind {
            ClassKind::EnumParent => Some(class),
            ClassKind::EnumCase => info.family,
            ClassKind::Normal => None,
        }
    }

    /// Find an arm of a family by its short name.
    pub(crate) fn find_arm(&self, family: u32, short: &str) -> Option<u32> {
        self.classes[family as usize]
            .arms
            .iter()
            .copied()
            .find(|arm| self.classes[*arm as usize].arm_short == short)
    }

    /// The families whose unqualified constructor names are in scope.
    pub(crate) fn ctor_families(&self, env: &TyEnv, ctor: &str) -> Vec<u32> {
        let mut families = Vec::new();
        if env.core_scope {
            for idx in self.core_types.values() {
                if self.classes[*idx as usize].kind == ClassKind::EnumParent {
                    families.push(*idx);
                }
            }
        } else {
            for idx in self.user_types.values() {
                if self.classes[*idx as usize].kind == ClassKind::EnumParent {
                    families.push(*idx);
                }
            }
            if self.prelude && PRELUDE_CTORS.contains(&ctor) {
                for name in ["Option", "Result"] {
                    if let Some(idx) = self.core_types.get(name) {
                        families.push(*idx);
                    }
                }
            }
        }
        families.sort_unstable();
        families
            .into_iter()
            .filter(|f| self.find_arm(*f, ctor).is_some())
            .collect()
    }

    /// Register one more checked function and return its index.
    pub(crate) fn push_func(&mut self, func: HirFunc, sig: FnSig) -> u32 {
        let idx = self.funcs.len() as u32;
        self.funcs.push(Some(func));
        self.sigs.push(sig);
        idx
    }

    /// The `Option[arg]` instance type for the pinned core `Option`.
    pub(crate) fn option_of(&mut self, arg: TypeId) -> TypeId {
        let class = ClassId(self.core.option_class);
        self.store.intern(Type::Inst(class, vec![arg]))
    }
}

pub(crate) fn resolve_type(
    ctx: &mut Ctx,
    env: &TyEnv,
    ty: &ast::TypeExpr,
) -> Result<TypeId, Diagnostic> {
    match &ty.kind {
        ast::TypeExprKind::Unit => Ok(UNIT),
        ast::TypeExprKind::Name(name) => {
            if let Some(pos) = env.type_names.iter().position(|n| n == name) {
                return Ok(ctx.store.intern(Type::Var(pos as u32)));
            }
            if let Some(id) = ctx.store.by_name(name) {
                return Ok(id);
            }
            if let Some(class) = ctx.lookup_type(name, env) {
                // The store metadata is complete after predeclaration,
                // before the `ClassInfo` entries exist, so forward and
                // recursive type references resolve here.
                let arity = ctx.store.class_meta(ClassId(class)).type_params;
                if arity != 0 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("the generic type `{name}` needs {arity} type argument(s)"),
                        ty.span,
                    ));
                }
                return Ok(ctx.store.intern(Type::Class(ClassId(class))));
            }
            if name == "List" || name == "Map" {
                return Err(Diagnostic::new(
                    "E1024",
                    format!("the generic type `{name}` needs type arguments"),
                    ty.span,
                ));
            }
            Err(Diagnostic::new(
                "E1013",
                format!("unknown type name `{name}`"),
                ty.span,
            ))
        }
        ast::TypeExprKind::Apply(name, args) => match name.as_str() {
            "Vm" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Vm` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let result = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::Vm(result)))
            }
            "List" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`List` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let elem = resolve_type(ctx, env, &args[0])?;
                Ok(ctx.store.intern(Type::List(elem)))
            }
            "Map" => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`Map` takes 2 type arguments, found {}", args.len()),
                        ty.span,
                    ));
                }
                let key = resolve_type(ctx, env, &args[0])?;
                check_key_type(ctx, key, args[0].span)?;
                let value = resolve_type(ctx, env, &args[1])?;
                Ok(ctx.store.intern(Type::Map(key, value)))
            }
            other => {
                if let Some(class) = ctx.lookup_type(other, env) {
                    let arity = ctx.store.class_meta(ClassId(class)).type_params as usize;
                    if arity == 0 {
                        return Err(Diagnostic::new(
                            "E1024",
                            format!("the type `{other}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    if args.len() != arity {
                        return Err(Diagnostic::new(
                            "E1024",
                            format!(
                                "`{other}` takes {arity} type argument(s), found {}",
                                args.len()
                            ),
                            ty.span,
                        ));
                    }
                    let mut resolved = Vec::with_capacity(args.len());
                    for arg in args {
                        resolved.push(resolve_type(ctx, env, arg)?);
                    }
                    return Ok(ctx.store.intern(Type::Inst(ClassId(class), resolved)));
                }
                Err(Diagnostic::new(
                    "E1013",
                    format!("unknown type name `{other}`"),
                    ty.span,
                ))
            }
        },
        ast::TypeExprKind::ListShort(elem) => {
            let elem = resolve_type(ctx, env, elem)?;
            Ok(ctx.store.intern(Type::List(elem)))
        }
        ast::TypeExprKind::MapShort(key, value) => {
            let key_ty = resolve_type(ctx, env, key)?;
            check_key_type(ctx, key_ty, key.span)?;
            let value = resolve_type(ctx, env, value)?;
            Ok(ctx.store.intern(Type::Map(key_ty, value)))
        }
        ast::TypeExprKind::Tuple(elems) => {
            let mut resolved = Vec::with_capacity(elems.len());
            for elem in elems {
                resolved.push(resolve_type(ctx, env, elem)?);
            }
            Ok(ctx.store.intern(Type::Tuple(resolved)))
        }
        ast::TypeExprKind::Fn(params, muts, ret, row) => {
            let mut ptys = Vec::new();
            for p in params {
                ptys.push(resolve_type(ctx, env, p)?);
            }
            let ret = resolve_type(ctx, env, ret)?;
            let row = resolve_row(ctx, env, row)?;
            Ok(ctx.store.intern_fn(ptys, muts.clone(), ret, row))
        }
    }
}

/// Resolve declared row items to canonical row elements.
pub(crate) fn resolve_row(
    ctx: &mut Ctx,
    env: &TyEnv,
    items: &[ast::RowItem],
) -> Result<Row, Diagnostic> {
    let mut row = Vec::with_capacity(items.len());
    for item in items {
        if let Some(pos) = env.effect_names.iter().position(|n| n == &item.name) {
            row.push(RowElem::Var(pos as u32));
            continue;
        }
        if lm_abi::row_name_valid(&item.name) {
            let idx = ctx.store.intern_row_name(&item.name);
            row.push(RowElem::Op(idx));
            continue;
        }
        let starts_upper = item
            .name
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
        if item.name.contains('.') || starts_upper {
            return Err(Diagnostic::new(
                "E1050",
                format!(
                    "`{}` is not an operation or group in the operation manifest",
                    item.name
                ),
                item.span,
            ));
        }
        return Err(Diagnostic::new(
            "E1046",
            format!(
                "unknown effect name `{}`; declare it with `effect {}` in the \
                 generic parameter list",
                item.name, item.name
            ),
            item.span,
        ));
    }
    Ok(ctx.store.canonical_row(row))
}

pub(crate) fn check_key_type(ctx: &Ctx, key: TypeId, span: Span) -> Result<(), Diagnostic> {
    if matches!(key, lm_types::BOOL | lm_types::INT | lm_types::STRING) {
        Ok(())
    } else {
        Err(Diagnostic::new(
            "E1033",
            format!(
                "a map key must be Bool, Int, or String, found {}",
                ctx.store.display(key)
            ),
            span,
        ))
    }
}

/// Split generic parameters into type names and effect names.
fn split_generics(generics: &[ast::GenericParam]) -> (Vec<String>, Vec<String>) {
    let mut type_names = Vec::new();
    let mut effect_names = Vec::new();
    for g in generics {
        if g.is_effect {
            effect_names.push(g.name.clone());
        } else {
            type_names.push(g.name.clone());
        }
    }
    (type_names, effect_names)
}

/// Check a parsed module and produce typed HIR with the default
/// options.
pub fn check_module(module: &ast::Module) -> Result<HirModule, Diagnostic> {
    check_module_with(module, CheckOptions::default())
}

/// Check a parsed module and produce typed HIR.
pub fn check_module_with(
    module: &ast::Module,
    options: CheckOptions,
) -> Result<HirModule, Diagnostic> {
    let core = core_ast();
    let mut ctx = Ctx {
        store: TypeStore::new(),
        classes: Vec::new(),
        user_types: HashMap::new(),
        core_types: HashMap::new(),
        prelude: options.prelude,
        func_index: HashMap::new(),
        sigs: Vec::new(),
        funcs: Vec::new(),
        core: CoreIds {
            option_class: 0,
            some_class: 0,
            none_class: 0,
        },
    };
    // Pass 1: predeclare all type names. User definitions come first,
    // so their class indices do not depend on the core.
    register_type_names(&mut ctx, module, false)?;
    register_type_names(&mut ctx, core, true).expect("the core type names register");
    link_class_parents(&mut ctx, module, false)?;
    // The core has no user-style inheritance, only enum families,
    // which were linked during registration.
    let option_class = ctx.core_types["Option"];
    ctx.core = CoreIds {
        option_class,
        some_class: option_class + 1,
        none_class: option_class + 2,
    };
    // Pass 2a: predeclare top-level function signatures.
    for (idx, func) in module.funcs.iter().enumerate() {
        if ctx.func_index.contains_key(&func.name) || ctx.user_types.contains_key(&func.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", func.name),
                func.name_span,
            ));
        }
        let (type_names, effect_names) = split_generics(&func.generics);
        let env = TyEnv {
            type_names: type_names.clone(),
            effect_names: effect_names.clone(),
            core_scope: false,
        };
        let sig = resolve_sig(
            &mut ctx,
            &env,
            type_names,
            effect_names,
            &func.params,
            &func.ret,
            &func.row,
            None,
        )?;
        ctx.func_index.insert(func.name.clone(), idx as u32);
        ctx.sigs.push(sig);
        ctx.funcs.push(None);
    }
    // Pass 2b: resolve user classes and enums in class-index order.
    resolve_all_classes(&mut ctx, module, false)?;
    // Reserve the entry function index.
    let entry_idx = ctx.funcs.len();
    ctx.funcs.push(None);
    ctx.sigs.push(FnSig {
        type_params: vec![],
        effect_params: vec![],
        params: vec![],
        param_muts: vec![],
        param_names: vec![],
        ret: UNIT,
        row: vec![],
    });
    // Pass 2c: resolve core classes and enums. Their method function
    // indices follow the entry.
    resolve_all_classes(&mut ctx, core, true).map_err(core_defect)?;
    // Pass 3: check field defaults.
    let mut own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>> = Vec::new();
    check_defaults(&mut ctx, module, false, &mut own_defaults)?;
    check_defaults(&mut ctx, core, true, &mut own_defaults).map_err(core_defect)?;
    // Pass 4: check top-level function bodies.
    for (idx, func) in module.funcs.iter().enumerate() {
        let sig = ctx.sigs[idx].clone();
        let env = TyEnv {
            type_names: sig.type_params.clone(),
            effect_names: sig.effect_params.clone(),
            core_scope: false,
        };
        let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
        for (slot, param) in func.params.iter().enumerate() {
            checker
                .locals
                .push((sig.params[slot], sig.param_muts[slot]));
            if checker.scopes[0]
                .insert(param.name.clone(), slot as u32)
                .is_some()
            {
                return Err(Diagnostic::new(
                    "E1014",
                    format!("duplicate parameter name `{}`", param.name),
                    param.span,
                ));
            }
        }
        let checked = checker.check_callable(&mut ctx, &func.body, sig.ret, func.span)?;
        ctx.funcs[idx] = Some(HirFunc {
            name: func.name.clone(),
            type_params: sig.type_params.len() as u32,
            effect_params: sig.effect_params.len() as u32,
            params: sig.params.clone(),
            param_muts: sig.param_muts.clone(),
            ret: sig.ret,
            row: sig.row.clone(),
            captures: vec![],
            locals: checked.locals,
            body: checked.body,
        });
    }
    // Pass 5: check user method bodies, then core method bodies.
    check_all_methods(&mut ctx, module, false)?;
    check_all_methods(&mut ctx, core, true).map_err(core_defect)?;
    // Pass 6: check the entry statements.
    let entry_span = module
        .entry
        .last()
        .map(|s| s.span)
        .unwrap_or(Span::new(0, 0));
    let checker = FnChecker::entry_collect(TyEnv::default());
    let (body, entry_ty, _mutable, locals, entry_row) =
        checker.check_entry(&mut ctx, &module.entry, entry_span)?;
    let entry_ty = if entry_ty == NEVER { UNIT } else { entry_ty };
    ctx.funcs[entry_idx] = Some(HirFunc {
        name: "<entry>".to_string(),
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: entry_ty,
        row: entry_row,
        captures: vec![],
        locals,
        body,
    });
    assemble(ctx, own_defaults, entry_idx)
}

/// A defect in the pinned core sources is an implementation defect,
/// not a user diagnostic.
fn core_defect(d: Diagnostic) -> Diagnostic {
    panic!(
        "the pinned core sources do not check: {} {}",
        d.code, d.message
    );
}

fn assemble(
    ctx: Ctx,
    own_defaults: Vec<Vec<(Option<HExpr>, Vec<TypeId>)>>,
    entry_idx: usize,
) -> Result<HirModule, Diagnostic> {
    let mut hir_classes: Vec<HirClass> = Vec::new();
    for (idx, info) in ctx.classes.iter().enumerate() {
        let (mut defaults, mut default_locals): (Vec<Option<HExpr>>, Vec<Vec<TypeId>>) =
            match info.parent {
                Some(p) => (
                    hir_classes[p as usize].defaults.clone(),
                    hir_classes[p as usize].default_locals.clone(),
                ),
                None => (Vec::new(), Vec::new()),
            };
        for (expr, locals) in &own_defaults[idx] {
            defaults.push(expr.clone());
            default_locals.push(locals.clone());
        }
        debug_assert_eq!(defaults.len(), info.field_tys.len());
        let ctor_kind = if info.kind == ClassKind::EnumCase {
            CtorKind::CaseFields
        } else if info.init.is_some() {
            CtorKind::Init
        } else {
            CtorKind::Defaults
        };
        let (ctor_params, ctor_param_muts) = match (&info.init, info.kind) {
            (_, ClassKind::EnumCase) => {
                let count = info.field_tys.len();
                (info.field_tys.clone(), vec![false; count])
            }
            (Some(init), _) => (init.params.clone(), init.param_muts.clone()),
            (None, _) => (vec![], vec![]),
        };
        let ctor_row = info
            .init
            .as_ref()
            .map(|m| m.row.clone())
            .unwrap_or_default();
        hir_classes.push(HirClass {
            name: info.name.clone(),
            parent: info.parent,
            type_params: info.type_params.len() as u32,
            kind: info.kind,
            ctor_kind,
            field_names: info.field_names.clone(),
            field_tys: info.field_tys.clone(),
            defaults,
            default_locals,
            methods: info
                .methods
                .iter()
                .map(|m| (m.name.clone(), m.func))
                .collect(),
            init: info.init.as_ref().map(|m| m.func),
            ctor_params,
            ctor_param_muts,
            ctor_row,
        });
    }
    let funcs: Vec<HirFunc> = ctx
        .funcs
        .into_iter()
        .map(|f| f.expect("every reserved function is checked"))
        .collect();
    Ok(HirModule {
        store: ctx.store,
        classes: hir_classes,
        funcs,
        entry: entry_idx,
        core: ctx.core,
    })
}

/// Register all class and enum names of one source module.
fn register_type_names(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    let declare = |ctx: &mut Ctx,
                   name: &str,
                   span: Span,
                   type_params: u32,
                   kind: ClassKind|
     -> Result<u32, Diagnostic> {
        let map = if is_core {
            &mut ctx.core_types
        } else {
            &mut ctx.user_types
        };
        if kind != ClassKind::EnumCase && map.contains_key(name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{name}` has more than one definition"),
                span,
            ));
        }
        let id = ctx.store.register_class(name, type_params, kind);
        if kind != ClassKind::EnumCase {
            let map = if is_core {
                &mut ctx.core_types
            } else {
                &mut ctx.user_types
            };
            map.insert(name.to_string(), id.0);
        }
        Ok(id.0)
    };
    for class in &module.classes {
        let (type_names, effect_names) = split_generics(&class.generics);
        if !effect_names.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "a class cannot declare effect parameters",
                class.name_span,
            ));
        }
        declare(
            ctx,
            &class.name,
            class.name_span,
            type_names.len() as u32,
            ClassKind::Normal,
        )?;
    }
    for enum_def in &module.enums {
        let (type_names, effect_names) = split_generics(&enum_def.generics);
        if !effect_names.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "an enum cannot declare effect parameters",
                enum_def.name_span,
            ));
        }
        let parent = declare(
            ctx,
            &enum_def.name,
            enum_def.name_span,
            type_names.len() as u32,
            ClassKind::EnumParent,
        )?;
        let mut seen: Vec<&str> = Vec::new();
        for arm in &enum_def.arms {
            if seen.contains(&arm.name.as_str()) {
                return Err(Diagnostic::new(
                    "E1040",
                    format!("the enum already has an arm named `{}`", arm.name),
                    arm.name_span,
                ));
            }
            seen.push(&arm.name);
            let full = format!("{}.{}", enum_def.name, arm.name);
            let arm_id = declare(
                ctx,
                &full,
                arm.name_span,
                type_names.len() as u32,
                ClassKind::EnumCase,
            )?;
            ctx.store.set_class_parent(ClassId(arm_id), ClassId(parent));
        }
    }
    Ok(())
}

/// Link declared parent classes and reject invalid inheritance.
fn link_class_parents(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name];
        if let Some((pname, pspan)) = &class.parent {
            if !class.generics.is_empty() {
                return Err(Diagnostic::new(
                    "E1024",
                    "a generic class cannot declare a parent in this slice",
                    *pspan,
                ));
            }
            let parent = *map.get(pname).ok_or_else(|| {
                Diagnostic::new("E1038", format!("unknown parent class `{pname}`"), *pspan)
            })?;
            let parent_meta = ctx.store.class_meta(ClassId(parent)).clone();
            match parent_meta.kind {
                ClassKind::EnumParent | ClassKind::EnumCase => {
                    return Err(Diagnostic::new(
                        "E1040",
                        format!("`{pname}` is part of a sealed enum and cannot be a parent"),
                        *pspan,
                    ));
                }
                ClassKind::Normal => {}
            }
            if parent >= idx {
                return Err(Diagnostic::new(
                    "E1038",
                    format!("the parent class `{pname}` must be declared before the subclass"),
                    *pspan,
                ));
            }
            if parent_meta.type_params > 0 {
                return Err(Diagnostic::new(
                    "E1024",
                    "a generic class cannot be a parent in this slice",
                    *pspan,
                ));
            }
            ctx.store.set_class_parent(ClassId(idx), ClassId(parent));
        }
    }
    Ok(())
}

/// Resolve the classes and enums of one source module into
/// `ClassInfo` entries, in class-index order.
fn resolve_all_classes(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name];
        debug_assert_eq!(idx as usize, ctx.classes.len());
        let info = resolve_class(ctx, class, idx, is_core)?;
        ctx.classes.push(info);
    }
    for enum_def in &module.enums {
        resolve_enum(ctx, enum_def, is_core)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn resolve_sig(
    ctx: &mut Ctx,
    env: &TyEnv,
    type_params: Vec<String>,
    effect_params: Vec<String>,
    params: &[ast::Param],
    ret: &Option<ast::TypeExpr>,
    row: &[ast::RowItem],
    self_ty: Option<(TypeId, bool)>,
) -> Result<FnSig, Diagnostic> {
    let mut ptys = Vec::new();
    let mut muts = Vec::new();
    let mut names = Vec::new();
    if let Some((ty, mutable)) = self_ty {
        ptys.push(ty);
        muts.push(mutable);
        names.push("self".to_string());
    }
    let mut seen: Vec<&str> = Vec::new();
    for param in params {
        if seen.contains(&param.name.as_str()) {
            return Err(Diagnostic::new(
                "E1014",
                format!("duplicate parameter name `{}`", param.name),
                param.span,
            ));
        }
        seen.push(&param.name);
        ptys.push(resolve_type(ctx, env, &param.ty)?);
        muts.push(param.mutable);
        names.push(param.name.clone());
    }
    let ret = match ret {
        Some(ty) => resolve_type(ctx, env, ty)?,
        None => UNIT,
    };
    let row = resolve_row(ctx, env, row)?;
    Ok(FnSig {
        type_params,
        effect_params,
        params: ptys,
        param_muts: muts,
        param_names: names,
        ret,
        row,
    })
}

/// Resolve one method declaration into a signature and reserve its
/// function index.
fn resolve_method_sig(
    ctx: &mut Ctx,
    method: &ast::MethodDef,
    class_type_names: &[String],
    self_ty: TypeId,
    is_core: bool,
    is_init: bool,
) -> Result<MethodSig, Diagnostic> {
    let (own_type, own_effect) = split_generics(&method.generics);
    if is_init && (!own_type.is_empty() || !own_effect.is_empty()) {
        return Err(Diagnostic::new(
            "E1038",
            "`init` cannot declare generic parameters",
            method.name_span,
        ));
    }
    let mut type_names = class_type_names.to_vec();
    for own in &own_type {
        if type_names.contains(own) {
            return Err(Diagnostic::new(
                "E1014",
                format!("duplicate generic parameter name `{own}`"),
                method.name_span,
            ));
        }
        type_names.push(own.clone());
    }
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: own_effect.clone(),
        core_scope: is_core,
    };
    let mut_self = is_init || method.mut_self;
    let sig = resolve_sig(
        ctx,
        &env,
        type_names,
        own_effect.clone(),
        &method.params,
        &method.ret,
        &method.row,
        Some((self_ty, mut_self)),
    )?;
    let func = ctx.funcs.len() as u32;
    ctx.sigs.push(sig.clone());
    ctx.funcs.push(None);
    Ok(MethodSig {
        name: method.name.clone(),
        func,
        mut_self: method.mut_self,
        params: sig.params[1..].to_vec(),
        param_muts: sig.param_muts[1..].to_vec(),
        param_names: sig.param_names[1..].to_vec(),
        ret: if is_init { UNIT } else { sig.ret },
        row: sig.row,
        own_type_params: own_type,
        own_effect_params: own_effect,
    })
}

/// Resolve one class declaration: layout, methods, and `init`.
fn resolve_class(
    ctx: &mut Ctx,
    class: &ast::ClassDef,
    idx: u32,
    is_core: bool,
) -> Result<ClassInfo, Diagnostic> {
    let map = if is_core {
        &ctx.core_types
    } else {
        &ctx.user_types
    };
    let parent = class.parent.as_ref().map(|(name, _)| map[name]);
    let (type_names, _) = split_generics(&class.generics);
    let self_ty = if type_names.is_empty() {
        ctx.store.intern(Type::Class(ClassId(idx)))
    } else {
        let vars: Vec<TypeId> = (0..type_names.len())
            .map(|i| ctx.store.intern(Type::Var(i as u32)))
            .collect();
        ctx.store.intern(Type::Inst(ClassId(idx), vars))
    };
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: vec![],
        core_scope: is_core,
    };
    let (mut field_names, mut field_tys, mut has_default) = match parent {
        Some(p) => {
            let info = &ctx.classes[p as usize];
            (
                info.field_names.clone(),
                info.field_tys.clone(),
                info.has_default.clone(),
            )
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let own_start = field_names.len();
    for field in &class.fields {
        if field_names.contains(&field.name) {
            return Err(Diagnostic::new(
                "E1038",
                format!("the class already has a field named `{}`", field.name),
                field.span,
            ));
        }
        field_names.push(field.name.clone());
        field_tys.push(resolve_type(ctx, &env, &field.ty)?);
        has_default.push(field.default.is_some());
    }
    let mut methods: Vec<MethodSig> = Vec::new();
    let mut init: Option<MethodSig> = None;
    for method in &class.methods {
        if method.name == "freeze" {
            return Err(Diagnostic::new(
                "E1038",
                "`freeze` is a reserved method name",
                method.name_span,
            ));
        }
        if field_names.contains(&method.name)
            || methods.iter().any(|m| m.name == method.name)
            || (method.name == "init" && init.is_some())
        {
            return Err(Diagnostic::new(
                "E1038",
                format!("the class already has a member named `{}`", method.name),
                method.name_span,
            ));
        }
        if method.name == "init" {
            if !method.mut_self {
                return Err(Diagnostic::new(
                    "E1038",
                    "`init` must declare `mut self`",
                    method.name_span,
                ));
            }
            if method.ret.is_some() {
                return Err(Diagnostic::new(
                    "E1038",
                    "`init` cannot declare a result type",
                    method.name_span,
                ));
            }
            let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, true)?;
            init = Some(msig);
            continue;
        }
        let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, false)?;
        // Override compatibility with the nearest ancestor method.
        if let Some(p) = parent {
            if let Some(base) = ctx.find_method(p, &method.name) {
                let same_params = base.params == msig.params
                    && base.param_muts == msig.param_muts
                    && base.mut_self == msig.mut_self
                    && base.own_type_params.len() == msig.own_type_params.len()
                    && base.own_effect_params.len() == msig.own_effect_params.len();
                if !same_params {
                    return Err(Diagnostic::new(
                        "E1031",
                        format!(
                            "the override of `{}` must keep the parameter types \
                             and the `mut` markers",
                            method.name
                        ),
                        method.name_span,
                    ));
                }
                if !ctx.store.compatible(base.ret, msig.ret) {
                    return Err(Diagnostic::new(
                        "E1031",
                        format!(
                            "the override of `{}` must keep or narrow the result type",
                            method.name
                        ),
                        method.name_span,
                    ));
                }
                if !ctx.store.row_included(&msig.row, &base.row) {
                    return Err(Diagnostic::new(
                        "E1046",
                        format!(
                            "the override of `{}` widens the effect row; the parent \
                             row is `{}`",
                            method.name,
                            display_row_or_empty(&ctx.store, &base.row)
                        ),
                        method.name_span,
                    ));
                }
            }
        }
        methods.push(msig);
    }
    if init.is_none() {
        if let Some(missing) = has_default.iter().position(|d| !d) {
            return Err(Diagnostic::new(
                "E1038",
                format!(
                    "the class needs an `init` because the field `{}` has no default",
                    field_names[missing]
                ),
                class.name_span,
            ));
        }
    }
    Ok(ClassInfo {
        name: class.name.clone(),
        parent,
        type_params: type_names,
        kind: ClassKind::Normal,
        self_ty,
        field_names,
        field_tys,
        has_default,
        own_start,
        methods,
        init,
        family: None,
        arms: Vec::new(),
        arm_short: String::new(),
    })
}

fn display_row_or_empty(store: &TypeStore, row: &Row) -> String {
    if row.is_empty() {
        "empty".to_string()
    } else {
        store.display_row(row)
    }
}

/// Resolve one enum: the abstract parent, the case classes, and the
/// family methods on the parent.
fn resolve_enum(ctx: &mut Ctx, enum_def: &ast::EnumDef, is_core: bool) -> Result<(), Diagnostic> {
    let map = if is_core {
        &ctx.core_types
    } else {
        &ctx.user_types
    };
    let parent_idx = map[&enum_def.name];
    debug_assert_eq!(parent_idx as usize, ctx.classes.len());
    let (type_names, _) = split_generics(&enum_def.generics);
    let self_ty = if type_names.is_empty() {
        ctx.store.intern(Type::Class(ClassId(parent_idx)))
    } else {
        let vars: Vec<TypeId> = (0..type_names.len())
            .map(|i| ctx.store.intern(Type::Var(i as u32)))
            .collect();
        ctx.store.intern(Type::Inst(ClassId(parent_idx), vars))
    };
    let env = TyEnv {
        type_names: type_names.clone(),
        effect_names: vec![],
        core_scope: is_core,
    };
    // Resolve arm field types first; then methods on the parent.
    let mut arm_infos = Vec::new();
    let arm_base = parent_idx + 1;
    for (aidx, arm) in enum_def.arms.iter().enumerate() {
        let arm_class = arm_base + aidx as u32;
        let mut field_names = Vec::new();
        let mut field_tys = Vec::new();
        for (fname, fty) in &arm.fields {
            field_names.push(fname.clone());
            field_tys.push(resolve_type(ctx, &env, fty)?);
        }
        arm_infos.push((arm_class, arm.name.clone(), field_names, field_tys));
    }
    let mut methods = Vec::new();
    for method in &enum_def.methods {
        if method.name == "freeze" {
            return Err(Diagnostic::new(
                "E1038",
                "`freeze` is a reserved method name",
                method.name_span,
            ));
        }
        if method.name == "init" {
            return Err(Diagnostic::new(
                "E1040",
                "an enum cannot declare `init`",
                method.name_span,
            ));
        }
        if methods.iter().any(|m: &MethodSig| m.name == method.name) {
            return Err(Diagnostic::new(
                "E1038",
                format!("the enum already has a member named `{}`", method.name),
                method.name_span,
            ));
        }
        let msig = resolve_method_sig(ctx, method, &type_names, self_ty, is_core, false)?;
        methods.push(msig);
    }
    let arms: Vec<u32> = arm_infos.iter().map(|(idx, _, _, _)| *idx).collect();
    ctx.classes.push(ClassInfo {
        name: enum_def.name.clone(),
        parent: None,
        type_params: type_names.clone(),
        kind: ClassKind::EnumParent,
        self_ty,
        field_names: Vec::new(),
        field_tys: Vec::new(),
        has_default: Vec::new(),
        own_start: 0,
        methods,
        init: None,
        family: None,
        arms,
        arm_short: String::new(),
    });
    for (arm_class, short, field_names, field_tys) in arm_infos {
        debug_assert_eq!(arm_class as usize, ctx.classes.len());
        let arm_self_ty = if type_names.is_empty() {
            ctx.store.intern(Type::Class(ClassId(arm_class)))
        } else {
            let vars: Vec<TypeId> = (0..type_names.len())
                .map(|i| ctx.store.intern(Type::Var(i as u32)))
                .collect();
            ctx.store.intern(Type::Inst(ClassId(arm_class), vars))
        };
        let count = field_tys.len();
        ctx.classes.push(ClassInfo {
            name: format!("{}.{}", enum_def.name, short),
            parent: Some(parent_idx),
            type_params: type_names.clone(),
            kind: ClassKind::EnumCase,
            self_ty: arm_self_ty,
            field_names,
            field_tys,
            has_default: vec![false; count],
            own_start: 0,
            methods: Vec::new(),
            init: None,
            family: Some(parent_idx),
            arms: Vec::new(),
            arm_short: short,
        });
    }
    Ok(())
}

/// Check the field default expressions of one source module.
fn check_defaults(
    ctx: &mut Ctx,
    module: &ast::Module,
    is_core: bool,
    own_defaults: &mut Vec<Vec<(Option<HExpr>, Vec<TypeId>)>>,
) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let idx = map[&class.name] as usize;
        let mut defaults = Vec::new();
        for field in &class.fields {
            let checked = match &field.default {
                Some(expr) => {
                    let own_start = ctx.classes[idx].own_start;
                    let fidx = own_start + defaults.len();
                    let field_ty = ctx.classes[idx].field_tys[fidx];
                    let env = TyEnv {
                        type_names: ctx.classes[idx].type_params.clone(),
                        effect_names: vec![],
                        core_scope: is_core,
                    };
                    let mut checker = FnChecker::top_level(RetKind::Entry, env, vec![]);
                    let expr = checker.check_expr(ctx, expr, field_ty)?;
                    // The temporary slot types of the default follow
                    // it into lowering, so the `<new>` scratch slots
                    // keep their declared types.
                    let locals: Vec<TypeId> = checker.locals.iter().map(|(t, _)| *t).collect();
                    (Some(expr), locals)
                }
                None => (None, Vec::new()),
            };
            defaults.push(checked);
        }
        debug_assert_eq!(idx, own_defaults.len());
        own_defaults.push(defaults);
    }
    for enum_def in &module.enums {
        // The parent has no fields; each arm has required fields.
        own_defaults.push(Vec::new());
        for arm in &enum_def.arms {
            own_defaults.push(vec![(None, Vec::new()); arm.fields.len()]);
        }
    }
    Ok(())
}

/// Check every method body of one source module.
fn check_all_methods(ctx: &mut Ctx, module: &ast::Module, is_core: bool) -> Result<(), Diagnostic> {
    for class in &module.classes {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let cidx = map[&class.name];
        for method in &class.methods {
            check_method(ctx, cidx, method, is_core)?;
        }
    }
    for enum_def in &module.enums {
        let map = if is_core {
            &ctx.core_types
        } else {
            &ctx.user_types
        };
        let cidx = map[&enum_def.name];
        for method in &enum_def.methods {
            check_method(ctx, cidx, method, is_core)?;
        }
    }
    Ok(())
}

/// Check one method body, with constructor tracking for `init`.
fn check_method(
    ctx: &mut Ctx,
    cidx: u32,
    method: &ast::MethodDef,
    is_core: bool,
) -> Result<(), Diagnostic> {
    let is_init = method.name == "init";
    let (func_idx, sig) = {
        let info = &ctx.classes[cidx as usize];
        let msig = if is_init {
            info.init.as_ref().expect("init resolved")
        } else {
            info.methods
                .iter()
                .find(|m| m.name == method.name)
                .expect("method resolved")
        };
        (msig.func, ctx.sigs[msig.func as usize].clone())
    };
    let env = TyEnv {
        type_names: sig.type_params.clone(),
        effect_names: sig.effect_params.clone(),
        core_scope: is_core,
    };
    let mut checker = FnChecker::top_level(RetKind::Known(sig.ret), env, sig.row.clone());
    checker.self_class = Some(cidx);
    checker.locals.push((sig.params[0], sig.param_muts[0]));
    checker.scopes[0].insert("self".to_string(), 0);
    for (i, param) in method.params.iter().enumerate() {
        let slot = (i + 1) as u32;
        checker
            .locals
            .push((sig.params[i + 1], sig.param_muts[i + 1]));
        checker.scopes[0].insert(param.name.clone(), slot);
    }
    if is_init {
        let info = &ctx.classes[cidx as usize];
        let needs_super = info
            .parent
            .map(|p| ctx.classes[p as usize].init.is_some())
            .unwrap_or(false);
        checker.ctor = Some(crate::checkfn::CtorCtx {
            class: cidx,
            needs_super,
            state: crate::checkfn::CtorState {
                inited: info.has_default.clone(),
                super_done: false,
            },
        });
    }
    let checked = checker.check_callable(ctx, &method.body, sig.ret, method.span)?;
    // A constructor must complete on its normal exit.
    if is_init && !checked.diverges {
        let checker_state = checked.ctor.expect("ctor state present");
        crate::checkfn::require_complete(ctx, cidx, &checker_state, method.span)?;
    }
    ctx.funcs[func_idx as usize] = Some(HirFunc {
        name: format!("{}.{}", ctx.classes[cidx as usize].name, method.name),
        type_params: sig.type_params.len() as u32,
        effect_params: sig.effect_params.len() as u32,
        params: sig.params.clone(),
        param_muts: sig.param_muts.clone(),
        ret: sig.ret,
        row: sig.row.clone(),
        captures: vec![],
        locals: checked.locals,
        body: checked.body,
    });
    Ok(())
}
