//! Bidirectional type checking from AST to typed HIR.
//!
//! The checker synthesizes a type for each expression, or checks the
//! expression against an expected type. It resolves names to local
//! slots, capture indices, function indices, class indices, and field
//! layout indices. It tracks reference capability, closure captures,
//! and definite field initialization inside constructors. It stops at
//! the first error and returns one precise diagnostic.

use crate::hir::*;
use lm_source::ast::{self, BinOp, ExprKind, StmtKind, TypeExprKind};
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{
    ClassId, Type, TypeId, TypeStore, BOOL, BYTE_BUFFER, INT, NEVER, STRING, STRING_BUILDER, UNIT,
};
use std::collections::HashMap;

/// One callable signature. Methods include `self` as parameter zero.
#[derive(Clone)]
struct FnSig {
    params: Vec<TypeId>,
    param_muts: Vec<bool>,
    ret: TypeId,
}

/// One declared method, without `self` in the parameter lists.
#[derive(Clone)]
struct MethodSig {
    name: String,
    func: u32,
    mut_self: bool,
    params: Vec<TypeId>,
    param_muts: Vec<bool>,
    ret: TypeId,
}

/// Checker-side class information with the full field layout.
struct ClassInfo {
    name: String,
    parent: Option<u32>,
    ty: TypeId,
    field_names: Vec<String>,
    field_tys: Vec<TypeId>,
    has_default: Vec<bool>,
    /// The layout index where own fields start.
    own_start: usize,
    methods: Vec<MethodSig>,
    init: Option<MethodSig>,
}

/// Shared module state for all function checkers.
struct Ctx {
    store: TypeStore,
    classes: Vec<ClassInfo>,
    class_by_name: HashMap<String, u32>,
    func_index: HashMap<String, u32>,
    sigs: Vec<FnSig>,
    funcs: Vec<Option<HirFunc>>,
}

impl Ctx {
    /// Find a method by name, walking the ancestor chain.
    fn find_method(&self, mut class: u32, name: &str) -> Option<MethodSig> {
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
    fn find_field(&self, class: u32, name: &str) -> Option<usize> {
        self.classes[class as usize]
            .field_names
            .iter()
            .position(|n| n == name)
    }

    /// Register one more checked function and return its index.
    fn push_func(&mut self, func: HirFunc, sig: FnSig) -> u32 {
        let idx = self.funcs.len() as u32;
        self.funcs.push(Some(func));
        self.sigs.push(sig);
        idx
    }
}

fn resolve_type(ctx: &mut Ctx, ty: &ast::TypeExpr) -> Result<TypeId, Diagnostic> {
    match &ty.kind {
        TypeExprKind::Unit => Ok(UNIT),
        TypeExprKind::Name(name) => {
            if let Some(id) = ctx.store.by_name(name) {
                return Ok(id);
            }
            if let Some(class) = ctx.class_by_name.get(name) {
                let class_id = ClassId(*class);
                return Ok(ctx.store.intern(Type::Class(class_id)));
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
        TypeExprKind::Apply(name, args) => match name.as_str() {
            "List" => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`List` takes 1 type argument, found {}", args.len()),
                        ty.span,
                    ));
                }
                let elem = resolve_type(ctx, &args[0])?;
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
                let key = resolve_type(ctx, &args[0])?;
                check_key_type(ctx, key, args[0].span)?;
                let value = resolve_type(ctx, &args[1])?;
                Ok(ctx.store.intern(Type::Map(key, value)))
            }
            other => Err(Diagnostic::new(
                "E1024",
                format!(
                    "the type `{other}` does not take type arguments; \
                     user generic types arrive in a later slice"
                ),
                ty.span,
            )),
        },
        TypeExprKind::ListShort(elem) => {
            let elem = resolve_type(ctx, elem)?;
            Ok(ctx.store.intern(Type::List(elem)))
        }
        TypeExprKind::MapShort(key, value) => {
            let key_ty = resolve_type(ctx, key)?;
            check_key_type(ctx, key_ty, key.span)?;
            let value = resolve_type(ctx, value)?;
            Ok(ctx.store.intern(Type::Map(key_ty, value)))
        }
        TypeExprKind::Fn(params, ret) => {
            let mut ptys = Vec::new();
            for p in params {
                ptys.push(resolve_type(ctx, p)?);
            }
            let ret = resolve_type(ctx, ret)?;
            Ok(ctx.store.intern_fn(ptys, ret))
        }
    }
}

fn check_key_type(ctx: &Ctx, key: TypeId, span: Span) -> Result<(), Diagnostic> {
    if matches!(key, BOOL | INT | STRING) {
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

/// Check a parsed module and produce typed HIR.
pub fn check_module(module: &ast::Module) -> Result<HirModule, Diagnostic> {
    let mut ctx = Ctx {
        store: TypeStore::new(),
        classes: Vec::new(),
        class_by_name: HashMap::new(),
        func_index: HashMap::new(),
        sigs: Vec::new(),
        funcs: Vec::new(),
    };
    // Pass 1: register class names and check parent links.
    for (idx, class) in module.classes.iter().enumerate() {
        if ctx.class_by_name.contains_key(&class.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", class.name),
                class.name_span,
            ));
        }
        ctx.class_by_name.insert(class.name.clone(), idx as u32);
        ctx.store.register_class(class.name.clone());
    }
    for (idx, class) in module.classes.iter().enumerate() {
        if let Some((pname, pspan)) = &class.parent {
            let parent = *ctx.class_by_name.get(pname).ok_or_else(|| {
                Diagnostic::new("E1038", format!("unknown parent class `{pname}`"), *pspan)
            })?;
            if parent as usize >= idx {
                return Err(Diagnostic::new(
                    "E1038",
                    format!("the parent class `{pname}` must be declared before the subclass"),
                    *pspan,
                ));
            }
            ctx.store
                .set_class_parent(ClassId(idx as u32), ClassId(parent));
        }
    }
    // Pass 2a: predeclare top-level function signatures.
    for (idx, func) in module.funcs.iter().enumerate() {
        if ctx.func_index.contains_key(&func.name) || ctx.class_by_name.contains_key(&func.name) {
            return Err(Diagnostic::new(
                "E1010",
                format!("the name `{}` has more than one definition", func.name),
                func.name_span,
            ));
        }
        let sig = resolve_sig(&mut ctx, &func.params, &func.ret, None)?;
        ctx.func_index.insert(func.name.clone(), idx as u32);
        ctx.sigs.push(sig);
        ctx.funcs.push(None);
    }
    // Pass 2b: resolve class fields and method signatures. Function
    // indices for methods follow the top-level functions.
    for (idx, class) in module.classes.iter().enumerate() {
        let info = resolve_class(&mut ctx, class, idx as u32)?;
        ctx.classes.push(info);
    }
    // Reserve the entry function index.
    let entry_idx = ctx.funcs.len();
    ctx.funcs.push(None);
    ctx.sigs.push(FnSig {
        params: vec![],
        param_muts: vec![],
        ret: UNIT,
    });
    // Pass 3: check field defaults.
    let mut own_defaults: Vec<Vec<Option<HExpr>>> = Vec::new();
    for (idx, class) in module.classes.iter().enumerate() {
        let mut defaults = Vec::new();
        for field in &class.fields {
            let checked = match &field.default {
                Some(expr) => {
                    let own_start = ctx.classes[idx].own_start;
                    let fidx = own_start + defaults.len();
                    let field_ty = ctx.classes[idx].field_tys[fidx];
                    let mut checker = FnChecker::top_level(RetKind::Entry);
                    Some(checker.check_expr(&mut ctx, expr, field_ty)?)
                }
                None => None,
            };
            defaults.push(checked);
        }
        own_defaults.push(defaults);
    }
    // Pass 4: check top-level function bodies.
    for (idx, func) in module.funcs.iter().enumerate() {
        let sig = ctx.sigs[idx].clone();
        let mut checker = FnChecker::top_level(RetKind::Known(sig.ret));
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
            params: sig.params.clone(),
            ret: sig.ret,
            captures: vec![],
            locals: checked.locals,
            body: checked.body,
        });
    }
    // Pass 5: check method bodies.
    for (cidx, class) in module.classes.iter().enumerate() {
        for method in &class.methods {
            check_method(&mut ctx, cidx as u32, class, method)?;
        }
    }
    // Pass 6: check the entry statements.
    let entry_span = module
        .entry
        .last()
        .map(|s| s.span)
        .unwrap_or(Span::new(0, 0));
    let checker = FnChecker::top_level(RetKind::Entry);
    let (body, entry_ty, _mutable, locals) =
        checker.check_entry(&mut ctx, &module.entry, entry_span)?;
    let entry_ty = if entry_ty == NEVER { UNIT } else { entry_ty };
    ctx.funcs[entry_idx] = Some(HirFunc {
        name: "<entry>".to_string(),
        params: vec![],
        ret: entry_ty,
        captures: vec![],
        locals,
        body,
    });
    // Assemble the HIR classes with full-layout defaults.
    let mut hir_classes: Vec<HirClass> = Vec::new();
    for (idx, info) in ctx.classes.iter().enumerate() {
        let mut defaults: Vec<Option<HExpr>> = match info.parent {
            Some(p) => hir_classes[p as usize].defaults.clone(),
            None => Vec::new(),
        };
        defaults.extend(own_defaults[idx].iter().cloned());
        debug_assert_eq!(defaults.len(), info.field_tys.len());
        hir_classes.push(HirClass {
            name: info.name.clone(),
            parent: info.parent,
            field_names: info.field_names.clone(),
            field_tys: info.field_tys.clone(),
            defaults,
            methods: info
                .methods
                .iter()
                .map(|m| (m.name.clone(), m.func))
                .collect(),
            init: info.init.as_ref().map(|m| m.func),
            ctor_params: info
                .init
                .as_ref()
                .map(|m| m.params.clone())
                .unwrap_or_default(),
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
    })
}

fn resolve_sig(
    ctx: &mut Ctx,
    params: &[ast::Param],
    ret: &Option<ast::TypeExpr>,
    self_ty: Option<(TypeId, bool)>,
) -> Result<FnSig, Diagnostic> {
    let mut ptys = Vec::new();
    let mut muts = Vec::new();
    if let Some((ty, mutable)) = self_ty {
        ptys.push(ty);
        muts.push(mutable);
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
        ptys.push(resolve_type(ctx, &param.ty)?);
        muts.push(param.mutable);
    }
    let ret = match ret {
        Some(ty) => resolve_type(ctx, ty)?,
        None => UNIT,
    };
    Ok(FnSig {
        params: ptys,
        param_muts: muts,
        ret,
    })
}

/// Resolve one class declaration: layout, methods, and `init`.
fn resolve_class(ctx: &mut Ctx, class: &ast::ClassDef, idx: u32) -> Result<ClassInfo, Diagnostic> {
    let parent = class
        .parent
        .as_ref()
        .map(|(name, _)| *ctx.class_by_name.get(name).expect("parent checked"));
    let ty = ctx.store.intern(Type::Class(ClassId(idx)));
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
        field_tys.push(resolve_type(ctx, &field.ty)?);
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
        let func = ctx.funcs.len() as u32;
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
            let sig = resolve_sig(ctx, &method.params, &method.ret, Some((ty, true)))?;
            ctx.sigs.push(sig.clone());
            ctx.funcs.push(None);
            init = Some(MethodSig {
                name: method.name.clone(),
                func,
                mut_self: true,
                params: sig.params[1..].to_vec(),
                param_muts: sig.param_muts[1..].to_vec(),
                ret: UNIT,
            });
            continue;
        }
        let sig = resolve_sig(
            ctx,
            &method.params,
            &method.ret,
            Some((ty, method.mut_self)),
        )?;
        let msig = MethodSig {
            name: method.name.clone(),
            func,
            mut_self: method.mut_self,
            params: sig.params[1..].to_vec(),
            param_muts: sig.param_muts[1..].to_vec(),
            ret: sig.ret,
        };
        // Override compatibility with the nearest ancestor method.
        if let Some(p) = parent {
            if let Some(base) = ctx.find_method(p, &method.name) {
                let same_params = base.params == msig.params
                    && base.param_muts == msig.param_muts
                    && base.mut_self == msig.mut_self;
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
            }
        }
        ctx.sigs.push(sig);
        ctx.funcs.push(None);
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
        ty,
        field_names,
        field_tys,
        has_default,
        own_start,
        methods,
        init,
    })
}

/// Check one method body, with constructor tracking for `init`.
fn check_method(
    ctx: &mut Ctx,
    cidx: u32,
    _class: &ast::ClassDef,
    method: &ast::MethodDef,
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
    let mut checker = FnChecker::top_level(RetKind::Known(sig.ret));
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
        checker.ctor = Some(CtorCtx {
            class: cidx,
            needs_super,
            state: CtorState {
                inited: info.has_default.clone(),
                super_done: false,
            },
        });
    }
    let checked = checker.check_callable(ctx, &method.body, sig.ret, method.span)?;
    // A constructor must complete on its normal exit.
    if is_init && !checked.diverges {
        let checker_state = checked.ctor.expect("ctor state present");
        require_complete(ctx, cidx, &checker_state, method.span)?;
    }
    ctx.funcs[func_idx as usize] = Some(HirFunc {
        name: format!("{}.{}", ctx.classes[cidx as usize].name, method.name),
        params: sig.params.clone(),
        ret: sig.ret,
        captures: vec![],
        locals: checked.locals,
        body: checked.body,
    });
    Ok(())
}

fn require_complete(ctx: &Ctx, cidx: u32, c: &CtorCtx, span: Span) -> Result<(), Diagnostic> {
    if c.needs_super && !c.state.super_done {
        return Err(Diagnostic::new(
            "E1030",
            "the initializer must call `super.init` exactly once on every path",
            span,
        ));
    }
    if let Some(missing) = c.state.inited.iter().position(|i| !i) {
        return Err(Diagnostic::new(
            "E1028",
            format!(
                "the field `{}` is not initialized on this path",
                ctx.classes[cidx as usize].field_names[missing]
            ),
            span,
        ));
    }
    Ok(())
}

/// How a block result is used.
#[derive(Clone, Copy, PartialEq)]
enum BlockMode {
    /// The block value is discarded. The block type is `()`.
    Stmt,
    /// The block must produce a value of the expected type.
    Value(TypeId),
    /// Synthesize the block type from its final expression.
    Synth,
}

/// The result-type context of the checked callable.
#[derive(Clone, Copy, PartialEq)]
enum RetKind {
    /// The module entry: `return` is invalid.
    Entry,
    /// A closure without a declared result: `return` is invalid.
    ClosureInfer,
    /// A declared result type.
    Known(TypeId),
}

/// The definite-initialization state inside one constructor path.
#[derive(Clone, PartialEq)]
struct CtorState {
    inited: Vec<bool>,
    super_done: bool,
}

struct CtorCtx {
    class: u32,
    needs_super: bool,
    state: CtorState,
}

/// The source of one captured value in the enclosing frame.
#[derive(Clone, Copy)]
enum CapSource {
    Local(u32),
    Capture(u32),
}

struct CaptureRec {
    name: String,
    ty: TypeId,
    mutable: bool,
    source: CapSource,
}

/// Name lookup across closure boundaries. The parent registers
/// transitive captures on demand.
trait OuterScope {
    fn capture_lookup(
        &mut self,
        name: &str,
    ) -> Result<Option<(TypeId, bool, CapSource)>, Diagnostic>;
}

/// The result of checking one callable body.
struct CheckedBody {
    body: Vec<HStmt>,
    locals: Vec<TypeId>,
    diverges: bool,
    ctor: Option<CtorCtx>,
}

enum NameRes {
    Local(u32, TypeId, bool),
    Capture(u32, TypeId, bool),
}

struct FnChecker<'o> {
    outer: Option<&'o mut dyn OuterScope>,
    locals: Vec<(TypeId, bool)>,
    scopes: Vec<HashMap<String, u32>>,
    captures: Vec<CaptureRec>,
    is_closure: bool,
    loop_depth: u32,
    ret: RetKind,
    self_class: Option<u32>,
    ctor: Option<CtorCtx>,
}

impl<'o> OuterScope for FnChecker<'o> {
    fn capture_lookup(
        &mut self,
        name: &str,
    ) -> Result<Option<(TypeId, bool, CapSource)>, Diagnostic> {
        if let Some(slot) = self.lookup_slot(name) {
            if name == "self" {
                if let Some(c) = &self.ctor {
                    if !ctor_complete(c) {
                        return Err(Diagnostic::new(
                            "E1029",
                            "`self` cannot be captured before the initializer \
                             assigns every required field",
                            Span::new(0, 0),
                        ));
                    }
                }
            }
            let (ty, mutable) = self.locals[slot as usize];
            return Ok(Some((ty, mutable, CapSource::Local(slot))));
        }
        if self.is_closure {
            if let Some(idx) = self.captures.iter().position(|c| c.name == name) {
                let c = &self.captures[idx];
                return Ok(Some((c.ty, c.mutable, CapSource::Capture(idx as u32))));
            }
            if let Some(outer) = self.outer.as_mut() {
                if let Some((ty, mutable, source)) = outer.capture_lookup(name)? {
                    let idx = self.captures.len() as u32;
                    self.captures.push(CaptureRec {
                        name: name.to_string(),
                        ty,
                        mutable,
                        source,
                    });
                    return Ok(Some((ty, mutable, CapSource::Capture(idx))));
                }
            }
        }
        Ok(None)
    }
}

fn ctor_complete(c: &CtorCtx) -> bool {
    c.state.inited.iter().all(|i| *i) && (!c.needs_super || c.state.super_done)
}

impl<'o> FnChecker<'o> {
    fn top_level(ret: RetKind) -> FnChecker<'static> {
        FnChecker {
            outer: None,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            captures: Vec::new(),
            is_closure: false,
            loop_depth: 0,
            ret,
            self_class: None,
            ctor: None,
        }
    }

    fn lookup_slot(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Some(*slot);
            }
        }
        None
    }

    /// Resolve a name to a local or a capture, registering transitive
    /// captures on demand.
    fn resolve_name(&mut self, name: &str) -> Result<Option<NameRes>, Diagnostic> {
        if let Some(slot) = self.lookup_slot(name) {
            let (ty, mutable) = self.locals[slot as usize];
            return Ok(Some(NameRes::Local(slot, ty, mutable)));
        }
        if self.is_closure {
            if let Some(idx) = self.captures.iter().position(|c| c.name == name) {
                let c = &self.captures[idx];
                return Ok(Some(NameRes::Capture(idx as u32, c.ty, c.mutable)));
            }
            if let Some(outer) = self.outer.as_mut() {
                if let Some((ty, mutable, source)) = outer.capture_lookup(name)? {
                    let idx = self.captures.len() as u32;
                    self.captures.push(CaptureRec {
                        name: name.to_string(),
                        ty,
                        mutable,
                        source,
                    });
                    return Ok(Some(NameRes::Capture(idx, ty, mutable)));
                }
            }
        }
        Ok(None)
    }

    fn mismatch(&self, ctx: &Ctx, expected: TypeId, found: TypeId, span: Span) -> Diagnostic {
        Diagnostic::new(
            "E1004",
            format!(
                "expected {}, found {}",
                ctx.store.display(expected),
                ctx.store.display(found)
            ),
            span,
        )
    }

    /// Check a full callable body and package the result.
    fn check_callable(
        mut self,
        ctx: &mut Ctx,
        stmts: &[ast::Stmt],
        ret: TypeId,
        span: Span,
    ) -> Result<CheckedBody, Diagnostic> {
        let mode = if ret == UNIT {
            BlockMode::Stmt
        } else {
            BlockMode::Value(ret)
        };
        let (body, _, _) = self.check_block(ctx, stmts, mode, span)?;
        let diverges = body.last().map(HStmt::diverges).unwrap_or(false);
        Ok(CheckedBody {
            body,
            locals: self.locals.iter().map(|(t, _)| *t).collect(),
            diverges,
            ctor: self.ctor,
        })
    }

    /// Check the module entry block and synthesize its type.
    fn check_entry(
        mut self,
        ctx: &mut Ctx,
        stmts: &[ast::Stmt],
        span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool, Vec<TypeId>), Diagnostic> {
        let (body, ty, mutable) = self.check_block(ctx, stmts, BlockMode::Synth, span)?;
        let locals = self.locals.iter().map(|(t, _)| *t).collect();
        Ok((body, ty, mutable, locals))
    }

    /// Check a statement list. Return the statements, the block type,
    /// and the block-value capability.
    fn check_block(
        &mut self,
        ctx: &mut Ctx,
        stmts: &[ast::Stmt],
        mode: BlockMode,
        block_span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool), Diagnostic> {
        let mut out = Vec::new();
        for (idx, stmt) in stmts.iter().enumerate() {
            if let Some(prev) = out.last() {
                let prev: &HStmt = prev;
                if prev.diverges() {
                    return Err(Diagnostic::new(
                        "E1021",
                        "this statement is unreachable",
                        stmt.span,
                    ));
                }
            }
            let is_last = idx + 1 == stmts.len();
            if is_last {
                if let BlockMode::Value(expected) = mode {
                    let (checked, mutable) = self.check_tail(ctx, stmt, expected)?;
                    out.push(checked);
                    return Ok((out, expected, mutable));
                }
            }
            out.push(self.check_stmt(ctx, stmt)?);
        }
        let (block_ty, mutable) = match mode {
            BlockMode::Stmt => (UNIT, true),
            BlockMode::Value(expected) => {
                // The list is empty, so the block value is `()`.
                if expected != UNIT {
                    return Err(self.mismatch(ctx, expected, UNIT, block_span));
                }
                (UNIT, true)
            }
            BlockMode::Synth => match out.last() {
                Some(HStmt::Expr(e)) => (e.ty, e.mutable),
                Some(stmt) if stmt.diverges() => (NEVER, true),
                _ => (UNIT, true),
            },
        };
        Ok((out, block_ty, mutable))
    }

    /// Check the final statement of a value block against an expected type.
    fn check_tail(
        &mut self,
        ctx: &mut Ctx,
        stmt: &ast::Stmt,
        expected: TypeId,
    ) -> Result<(HStmt, bool), Diagnostic> {
        match &stmt.kind {
            StmtKind::Expr(expr) => {
                let value = self.check_expr(ctx, expr, expected)?;
                let mutable = value.mutable;
                Ok((HStmt::Expr(value), mutable))
            }
            // A diverging tail satisfies any expected type.
            StmtKind::Return { .. } | StmtKind::Break | StmtKind::Continue => {
                Ok((self.check_stmt(ctx, stmt)?, true))
            }
            // A statement tail has the value `()`, so it satisfies an
            // expected unit type.
            _ if expected == UNIT => Ok((self.check_stmt(ctx, stmt)?, true)),
            _ => Err(self.mismatch(ctx, expected, UNIT, stmt.span)),
        }
    }

    fn check_stmt(&mut self, ctx: &mut Ctx, stmt: &ast::Stmt) -> Result<HStmt, Diagnostic> {
        match &stmt.kind {
            StmtKind::Assign {
                name,
                name_span,
                ty,
                value,
            } => self.check_assign(ctx, name, *name_span, ty, value),
            StmtKind::AssignField {
                recv,
                field,
                field_span,
                value,
            } => self.check_assign_field(ctx, recv, field, *field_span, value),
            StmtKind::While { cond, body } => {
                let before = self.ctor.as_ref().map(|c| c.state.clone());
                let cond = self.check_expr(ctx, cond, BOOL)?;
                self.ctor_guard_loop(&before, stmt.span)?;
                let snapshot = self.ctor.as_ref().map(|c| c.state.clone());
                self.scopes.push(HashMap::new());
                self.loop_depth += 1;
                let result = self.check_block(ctx, body, BlockMode::Stmt, stmt.span);
                self.loop_depth -= 1;
                self.scopes.pop();
                let (body, _, _) = result?;
                self.ctor_guard_loop(&snapshot, stmt.span)?;
                // The loop body may run zero times, so the state after
                // the loop is the state before it.
                if let (Some(c), Some(snap)) = (self.ctor.as_mut(), snapshot) {
                    c.state = snap;
                }
                Ok(HStmt::While { cond, body })
            }
            StmtKind::Return { value } => {
                let ret = match self.ret {
                    RetKind::Known(t) => t,
                    RetKind::Entry => {
                        return Err(Diagnostic::new(
                            "E1016",
                            "`return` is not valid at the top level of a module",
                            stmt.span,
                        ));
                    }
                    RetKind::ClosureInfer => {
                        return Err(Diagnostic::new(
                            "E1016",
                            "`return` needs a declared result type on the closure",
                            stmt.span,
                        ));
                    }
                };
                let value = match value {
                    Some(expr) => Some(self.check_expr(ctx, expr, ret)?),
                    None => {
                        if ret != UNIT {
                            return Err(self.mismatch(ctx, ret, UNIT, stmt.span));
                        }
                        None
                    }
                };
                // A constructor must be complete at every return.
                if let Some(c) = &self.ctor {
                    require_complete(ctx, c.class, c, stmt.span)?;
                }
                Ok(HStmt::Return { value })
            }
            StmtKind::Break => {
                if self.loop_depth == 0 {
                    return Err(Diagnostic::new(
                        "E1008",
                        "`break` is only valid inside a loop",
                        stmt.span,
                    ));
                }
                Ok(HStmt::Break)
            }
            StmtKind::Continue => {
                if self.loop_depth == 0 {
                    return Err(Diagnostic::new(
                        "E1008",
                        "`continue` is only valid inside a loop",
                        stmt.span,
                    ));
                }
                Ok(HStmt::Continue)
            }
            StmtKind::Expr(expr) => Ok(HStmt::Expr(self.synth_expr(ctx, expr)?)),
        }
    }

    /// Reject a `super.init` call inside a loop condition or body.
    fn ctor_guard_loop(&self, before: &Option<CtorState>, span: Span) -> Result<(), Diagnostic> {
        if let (Some(c), Some(before)) = (&self.ctor, before) {
            if c.state.super_done != before.super_done {
                return Err(Diagnostic::new(
                    "E1030",
                    "`super.init` is not valid inside a loop",
                    span,
                ));
            }
        }
        Ok(())
    }

    fn check_assign(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        ty: &Option<ast::TypeExpr>,
        value: &ast::Expr,
    ) -> Result<HStmt, Diagnostic> {
        match self.resolve_name(name)? {
            Some(NameRes::Local(slot, expected, was_mutable)) => {
                if ty.is_some() {
                    return Err(Diagnostic::new(
                        "E1020",
                        format!("the name `{name}` already has a declaration"),
                        name_span,
                    ));
                }
                let value = self.check_expr(ctx, value, expected)?;
                if was_mutable && !value.mutable && ctx.store.is_heap(expected) {
                    return Err(Diagnostic::new(
                        "E1035",
                        format!(
                            "the value is read-only, but the name `{name}` holds \
                             a mutable reference"
                        ),
                        name_span,
                    ));
                }
                Ok(HStmt::Assign { slot, value })
            }
            Some(NameRes::Capture(..)) => Err(Diagnostic::new(
                "E1036",
                format!("the captured name `{name}` cannot be rebound inside a closure"),
                name_span,
            )),
            None => {
                if ctx.func_index.contains_key(name) || ctx.class_by_name.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1019",
                        format!("cannot assign to `{name}`"),
                        name_span,
                    ));
                }
                // The first assignment declares a new local.
                let (value, local_ty) = match ty {
                    Some(annotation) => {
                        let annotated = resolve_type(ctx, annotation)?;
                        let value = self.check_expr(ctx, value, annotated)?;
                        (value, annotated)
                    }
                    None => {
                        let value = self.synth_expr(ctx, value)?;
                        let ty = value.ty;
                        (value, ty)
                    }
                };
                let slot = self.locals.len() as u32;
                self.locals.push((local_ty, value.mutable));
                self.scopes
                    .last_mut()
                    .expect("a scope is always open")
                    .insert(name.to_string(), slot);
                Ok(HStmt::Assign { slot, value })
            }
        }
    }

    fn check_assign_field(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        field: &str,
        field_span: Span,
        value: &ast::Expr,
    ) -> Result<HStmt, Diagnostic> {
        // A field assignment on `self` inside a constructor records
        // definite initialization and needs no completeness.
        if matches!(recv.kind, ExprKind::SelfRef) && self.ctor.is_some() {
            let cidx = self.ctor.as_ref().expect("ctor").class;
            let fidx = ctx
                .find_field(cidx, field)
                .ok_or_else(|| unknown_field(ctx, cidx, field, field_span))?;
            let field_ty = ctx.classes[cidx as usize].field_tys[fidx];
            let value = self.check_expr(ctx, value, field_ty)?;
            let c = self.ctor.as_mut().expect("ctor");
            c.state.inited[fidx] = true;
            let self_expr = self.self_value(ctx);
            return Ok(HStmt::AssignField {
                recv: self_expr,
                field: fidx as u32,
                value,
            });
        }
        let recv_h = self.synth_expr(ctx, recv)?;
        let class = match ctx.store.get(recv_h.ty) {
            Type::Class(c) => c.0,
            _ => {
                return Err(Diagnostic::new(
                    "E1027",
                    format!("the type {} has no fields", ctx.store.display(recv_h.ty)),
                    recv.span,
                ));
            }
        };
        let fidx = ctx
            .find_field(class, field)
            .ok_or_else(|| unknown_field(ctx, class, field, field_span))?;
        if !recv_h.mutable {
            return Err(Diagnostic::new(
                "E1035",
                format!("cannot write the field `{field}` through a read-only reference"),
                field_span,
            ));
        }
        let field_ty = ctx.classes[class as usize].field_tys[fidx];
        let value = self.check_expr(ctx, value, field_ty)?;
        Ok(HStmt::AssignField {
            recv: recv_h,
            field: fidx as u32,
            value,
        })
    }

    /// Build the `self` expression for a method body.
    fn self_value(&self, ctx: &Ctx) -> HExpr {
        let cidx = self.self_class.expect("self exists");
        let (ty, mutable) = self.locals[0];
        let _ = ctx;
        debug_assert_eq!(self.lookup_slot("self"), Some(0));
        let _ = cidx;
        HExpr {
            ty,
            mutable,
            kind: HExprKind::Local(0),
        }
    }

    /// Check an expression against an expected type.
    fn check_expr(
        &mut self,
        ctx: &mut Ctx,
        expr: &ast::Expr,
        expected: TypeId,
    ) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::If { arms, else_body } => {
                self.check_if(ctx, arms, else_body, Some(expected), expr.span)
            }
            ExprKind::ListLit(items) => {
                if let Type::List(elem) = ctx.store.get(expected) {
                    let elem = *elem;
                    let mut checked = Vec::new();
                    for item in items {
                        checked.push(self.check_expr(ctx, item, elem)?);
                    }
                    return Ok(HExpr {
                        ty: expected,
                        mutable: true,
                        kind: HExprKind::ListLit(checked),
                    });
                }
                let found = self.synth_expr(ctx, expr)?;
                if !ctx.store.compatible(expected, found.ty) {
                    return Err(self.mismatch(ctx, expected, found.ty, expr.span));
                }
                Ok(found)
            }
            ExprKind::MapLit(entries) => {
                if let Type::Map(k, v) = ctx.store.get(expected) {
                    let (k, v) = (*k, *v);
                    let mut checked = Vec::new();
                    for (key, value) in entries {
                        let key = self.check_expr(ctx, key, k)?;
                        let value = self.check_expr(ctx, value, v)?;
                        checked.push((key, value));
                    }
                    return Ok(HExpr {
                        ty: expected,
                        mutable: true,
                        kind: HExprKind::MapLit(checked),
                    });
                }
                let found = self.synth_expr(ctx, expr)?;
                if !ctx.store.compatible(expected, found.ty) {
                    return Err(self.mismatch(ctx, expected, found.ty, expr.span));
                }
                Ok(found)
            }
            ExprKind::Closure { params, ret, body } => {
                let expected_ret = match (ret, ctx.store.get(expected)) {
                    (None, Type::Fn(_, r)) => Some(*r),
                    _ => None,
                };
                let found = self.check_closure(ctx, params, ret, expected_ret, body, expr.span)?;
                if !ctx.store.compatible(expected, found.ty) {
                    return Err(self.mismatch(ctx, expected, found.ty, expr.span));
                }
                Ok(found)
            }
            _ => {
                let found = self.synth_expr(ctx, expr)?;
                if !ctx.store.compatible(expected, found.ty) {
                    return Err(self.mismatch(ctx, expected, found.ty, expr.span));
                }
                Ok(found)
            }
        }
    }

    /// Synthesize an expression type.
    fn synth_expr(&mut self, ctx: &mut Ctx, expr: &ast::Expr) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::Int(v) => Ok(HExpr {
                ty: INT,
                mutable: true,
                kind: HExprKind::Int(*v),
            }),
            ExprKind::Str(v) => Ok(HExpr {
                ty: STRING,
                mutable: true,
                kind: HExprKind::Str(v.clone()),
            }),
            ExprKind::Bool(v) => Ok(HExpr {
                ty: BOOL,
                mutable: true,
                kind: HExprKind::Bool(*v),
            }),
            ExprKind::Interp(parts) => self.synth_interp(ctx, parts, expr.span),
            ExprKind::SelfRef => self.synth_self(ctx, expr.span),
            ExprKind::Name(name) => {
                if let Some(res) = self.resolve_name(name)? {
                    return Ok(match res {
                        NameRes::Local(slot, ty, mutable) => HExpr {
                            ty,
                            mutable,
                            kind: HExprKind::Local(slot),
                        },
                        NameRes::Capture(idx, ty, mutable) => HExpr {
                            ty,
                            mutable,
                            kind: HExprKind::Capture(idx),
                        },
                    });
                }
                if ctx.func_index.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1018",
                        format!("the function `{name}` is not a value in this language slice"),
                        expr.span,
                    ));
                }
                if ctx.class_by_name.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1018",
                        format!("the class `{name}` is not a value in this language slice"),
                        expr.span,
                    ));
                }
                Err(Diagnostic::new(
                    "E1005",
                    format!("cannot find `{name}` in this scope"),
                    expr.span,
                ))
            }
            ExprKind::Not(inner) => {
                let inner = self.check_expr(ctx, inner, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Not(Box::new(inner)),
                })
            }
            ExprKind::Neg(inner) => {
                let inner = self.check_expr(ctx, inner, INT)?;
                Ok(HExpr {
                    ty: INT,
                    mutable: true,
                    kind: HExprKind::Neg(Box::new(inner)),
                })
            }
            ExprKind::Binary { op, left, right } => self.synth_binary(ctx, *op, left, right),
            ExprKind::And(left, right) => {
                let left = self.check_expr(ctx, left, BOOL)?;
                let right = self.check_expr(ctx, right, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::And(Box::new(left), Box::new(right)),
                })
            }
            ExprKind::Or(left, right) => {
                let left = self.check_expr(ctx, left, BOOL)?;
                let right = self.check_expr(ctx, right, BOOL)?;
                Ok(HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Or(Box::new(left), Box::new(right)),
                })
            }
            ExprKind::Call {
                name,
                name_span,
                args,
            } => self.synth_call(ctx, name, *name_span, args, expr.span),
            ExprKind::CallExpr { callee, args } => {
                let callee_h = self.synth_expr(ctx, callee)?;
                self.synth_call_value(ctx, callee_h, args, callee.span, expr.span)
            }
            ExprKind::Field {
                recv,
                name,
                name_span,
            } => self.synth_field(ctx, recv, name, *name_span),
            ExprKind::MethodCall {
                recv,
                name,
                name_span,
                args,
            } => self.synth_method_call(ctx, recv, name, *name_span, args, expr.span),
            ExprKind::SuperCall {
                name,
                name_span,
                args,
            } => self.synth_super_call(ctx, name, *name_span, args, expr.span),
            ExprKind::Index { recv, index } => self.synth_index(ctx, recv, index, expr.span),
            ExprKind::ListLit(items) => {
                let mut checked: Vec<HExpr> = Vec::new();
                let mut elem: Option<TypeId> = None;
                for item in items {
                    let h = self.synth_expr(ctx, item)?;
                    elem = Some(match elem {
                        None => h.ty,
                        Some(prev) => ctx
                            .store
                            .join(prev, h.ty)
                            .ok_or_else(|| self.mismatch(ctx, prev, h.ty, item.span))?,
                    });
                    checked.push(h);
                }
                let elem = elem.ok_or_else(|| {
                    Diagnostic::new(
                        "E1037",
                        "an empty list literal needs an expected type",
                        expr.span,
                    )
                })?;
                let ty = ctx.store.intern(Type::List(elem));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::ListLit(checked),
                })
            }
            ExprKind::MapLit(entries) => {
                let mut checked = Vec::new();
                let mut key_ty: Option<TypeId> = None;
                let mut val_ty: Option<TypeId> = None;
                for (key, value) in entries {
                    let k = self.synth_expr(ctx, key)?;
                    check_key_type(ctx, k.ty, key.span)?;
                    key_ty = Some(match key_ty {
                        None => k.ty,
                        Some(prev) => ctx
                            .store
                            .join(prev, k.ty)
                            .ok_or_else(|| self.mismatch(ctx, prev, k.ty, key.span))?,
                    });
                    let v = self.synth_expr(ctx, value)?;
                    val_ty = Some(match val_ty {
                        None => v.ty,
                        Some(prev) => ctx
                            .store
                            .join(prev, v.ty)
                            .ok_or_else(|| self.mismatch(ctx, prev, v.ty, value.span))?,
                    });
                    checked.push((k, v));
                }
                let (Some(k), Some(v)) = (key_ty, val_ty) else {
                    return Err(Diagnostic::new(
                        "E1037",
                        "an empty map literal needs an expected type",
                        expr.span,
                    ));
                };
                let ty = ctx.store.intern(Type::Map(k, v));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::MapLit(checked),
                })
            }
            ExprKind::Closure { params, ret, body } => {
                self.check_closure(ctx, params, ret, None, body, expr.span)
            }
            ExprKind::If { arms, else_body } => {
                self.check_if(ctx, arms, else_body, None, expr.span)
            }
        }
    }

    fn synth_self(&mut self, ctx: &mut Ctx, span: Span) -> Result<HExpr, Diagnostic> {
        let _ = ctx;
        match self.resolve_name("self").map_err(|d| reposition(d, span))? {
            Some(res) => {
                // Inside a constructor, `self` may not escape before
                // every required field is assigned.
                if let Some(c) = &self.ctor {
                    if !ctor_complete(c) {
                        return Err(Diagnostic::new(
                            "E1029",
                            "`self` cannot escape before the initializer assigns \
                             every required field",
                            span,
                        ));
                    }
                }
                Ok(match res {
                    NameRes::Local(slot, ty, mutable) => HExpr {
                        ty,
                        mutable,
                        kind: HExprKind::Local(slot),
                    },
                    NameRes::Capture(idx, ty, mutable) => HExpr {
                        ty,
                        mutable,
                        kind: HExprKind::Capture(idx),
                    },
                })
            }
            None => Err(Diagnostic::new(
                "E1039",
                "`self` is only valid inside a method",
                span,
            )),
        }
    }

    fn synth_interp(
        &mut self,
        ctx: &mut Ctx,
        parts: &[ast::InterpPart],
        _span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let mut checked = Vec::new();
        for part in parts {
            match part {
                ast::InterpPart::Lit(text) => checked.push(HInterpPart::Lit(text.clone())),
                ast::InterpPart::Expr(e) => {
                    let h = self.synth_expr(ctx, e)?;
                    if !matches!(h.ty, INT | BOOL | STRING) {
                        return Err(Diagnostic::new(
                            "E1034",
                            format!(
                                "cannot interpolate a value of type {}; this slice \
                                 interpolates Int, Bool, and String",
                                ctx.store.display(h.ty)
                            ),
                            e.span,
                        ));
                    }
                    checked.push(HInterpPart::Expr(h));
                }
            }
        }
        Ok(HExpr {
            ty: STRING,
            mutable: true,
            kind: HExprKind::Interp(checked),
        })
    }

    /// Check a call of a plain name: a local closure, a top-level
    /// function, a class constructor, or a native builder constructor.
    fn synth_call(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if let Some(res) = self.resolve_name(name)? {
            let (ty, kind) = match res {
                NameRes::Local(slot, ty, _) => (ty, HExprKind::Local(slot)),
                NameRes::Capture(idx, ty, _) => (ty, HExprKind::Capture(idx)),
            };
            let callee = HExpr {
                ty,
                mutable: true,
                kind,
            };
            return self.synth_call_value(ctx, callee, args, name_span, span);
        }
        if let Some(func) = ctx.func_index.get(name).copied() {
            let sig = ctx.sigs[func as usize].clone();
            let args = self.check_args(ctx, args, &sig.params, &sig.param_muts, name, span)?;
            return Ok(HExpr {
                ty: sig.ret,
                mutable: true,
                kind: HExprKind::Call { func, args },
            });
        }
        if let Some(class) = ctx.class_by_name.get(name).copied() {
            let info = &ctx.classes[class as usize];
            let ty = info.ty;
            let (params, muts) = match &info.init {
                Some(init) => (init.params.clone(), init.param_muts.clone()),
                None => (vec![], vec![]),
            };
            let args = self.check_args(ctx, args, &params, &muts, name, span)?;
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Construct { class, args },
            });
        }
        match name {
            "StringBuilder" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        "`StringBuilder` expects 0 argument(s)",
                        span,
                    ));
                }
                Ok(HExpr {
                    ty: STRING_BUILDER,
                    mutable: true,
                    kind: HExprKind::Native {
                        op: NativeOp::SbNew,
                        args: vec![],
                    },
                })
            }
            "ByteBuffer" => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        "`ByteBuffer` expects 0 argument(s)",
                        span,
                    ));
                }
                Ok(HExpr {
                    ty: BYTE_BUFFER,
                    mutable: true,
                    kind: HExprKind::Native {
                        op: NativeOp::BbNew,
                        args: vec![],
                    },
                })
            }
            _ => Err(Diagnostic::new(
                "E1005",
                format!("cannot find a function named `{name}`"),
                name_span,
            )),
        }
    }

    /// Check a call of a closure-typed value.
    fn synth_call_value(
        &mut self,
        ctx: &mut Ctx,
        callee: HExpr,
        args: &[ast::Expr],
        callee_span: Span,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let (params, ret) = match ctx.store.get(callee.ty) {
            Type::Fn(params, ret) => (params.clone(), *ret),
            _ => {
                return Err(Diagnostic::new(
                    "E1032",
                    format!(
                        "cannot call a value of type {}; it is not a closure",
                        ctx.store.display(callee.ty)
                    ),
                    callee_span,
                ));
            }
        };
        let muts = vec![false; params.len()];
        let args = self.check_args(ctx, args, &params, &muts, "closure", span)?;
        Ok(HExpr {
            ty: ret,
            mutable: true,
            kind: HExprKind::CallValue {
                callee: Box::new(callee),
                args,
            },
        })
    }

    fn check_args(
        &mut self,
        ctx: &mut Ctx,
        args: &[ast::Expr],
        params: &[TypeId],
        param_muts: &[bool],
        what: &str,
        span: Span,
    ) -> Result<Vec<HExpr>, Diagnostic> {
        if args.len() != params.len() {
            return Err(Diagnostic::new(
                "E1006",
                format!(
                    "`{what}` expects {} argument(s), found {}",
                    params.len(),
                    args.len()
                ),
                span,
            ));
        }
        let mut checked = Vec::new();
        for ((arg, param), is_mut) in args.iter().zip(params.iter()).zip(param_muts.iter()) {
            let h = self.check_expr(ctx, arg, *param)?;
            if *is_mut && !h.mutable {
                return Err(Diagnostic::new(
                    "E1035",
                    "a `mut` parameter needs a mutable value",
                    arg.span,
                ));
            }
            checked.push(h);
        }
        Ok(checked)
    }

    fn synth_field(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        name: &str,
        name_span: Span,
    ) -> Result<HExpr, Diagnostic> {
        // A field read on `self` inside a constructor checks definite
        // initialization and needs no completeness.
        if matches!(recv.kind, ExprKind::SelfRef) && self.ctor.is_some() {
            let cidx = self.ctor.as_ref().expect("ctor").class;
            let fidx = ctx
                .find_field(cidx, name)
                .ok_or_else(|| unknown_field(ctx, cidx, name, name_span))?;
            let c = self.ctor.as_ref().expect("ctor");
            if !c.state.inited[fidx] {
                return Err(Diagnostic::new(
                    "E1028",
                    format!("the field `{name}` is read before its first assignment"),
                    name_span,
                ));
            }
            let self_expr = self.self_value(ctx);
            let ty = ctx.classes[cidx as usize].field_tys[fidx];
            let mutable = self_expr.mutable;
            return Ok(HExpr {
                ty,
                mutable,
                kind: HExprKind::FieldGet {
                    recv: Box::new(self_expr),
                    field: fidx as u32,
                },
            });
        }
        let recv_h = self.synth_expr(ctx, recv)?;
        let class = match ctx.store.get(recv_h.ty) {
            Type::Class(c) => c.0,
            _ => {
                return Err(Diagnostic::new(
                    "E1027",
                    format!("the type {} has no fields", ctx.store.display(recv_h.ty)),
                    recv.span,
                ));
            }
        };
        let fidx = ctx
            .find_field(class, name)
            .ok_or_else(|| unknown_field(ctx, class, name, name_span))?;
        let ty = ctx.classes[class as usize].field_tys[fidx];
        let mutable = recv_h.mutable;
        Ok(HExpr {
            ty,
            mutable,
            kind: HExprKind::FieldGet {
                recv: Box::new(recv_h),
                field: fidx as u32,
            },
        })
    }

    fn synth_method_call(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let recv_h = self.synth_expr(ctx, recv)?;
        let recv_ty = recv_h.ty;
        // Class methods first, then the universal `freeze`.
        if let Type::Class(c) = ctx.store.get(recv_ty) {
            let class = c.0;
            if let Some(sig) = ctx.find_method(class, name) {
                if sig.name == "init" {
                    return Err(Diagnostic::new(
                        "E1026",
                        "`init` cannot be called as a method",
                        name_span,
                    ));
                }
                if sig.mut_self && !recv_h.mutable {
                    return Err(Diagnostic::new(
                        "E1035",
                        format!("the method `{name}` needs a mutable receiver"),
                        name_span,
                    ));
                }
                let args = self.check_args(ctx, args, &sig.params, &sig.param_muts, name, span)?;
                return Ok(HExpr {
                    ty: sig.ret,
                    mutable: true,
                    kind: HExprKind::MethodCall {
                        recv: Box::new(recv_h),
                        selector: name.to_string(),
                        args,
                    },
                });
            }
            if name == "freeze" && args.is_empty() {
                return Ok(freeze_expr(recv_h));
            }
            return Err(Diagnostic::new(
                "E1026",
                format!(
                    "the class `{}` has no method named `{name}`",
                    ctx.classes[class as usize].name
                ),
                name_span,
            ));
        }
        // Native methods on collections and builders.
        let store_ty = ctx.store.get(recv_ty).clone();
        let native = |op: NativeOp, params: Vec<TypeId>, ret: TypeId, needs_mut: bool| {
            (op, params, ret, needs_mut)
        };
        let (op, params, ret, needs_mut) = match (&store_ty, name) {
            (Type::List(e), "len") => {
                let _ = e;
                native(NativeOp::ListLen, vec![], INT, false)
            }
            (Type::List(e), "at") => native(NativeOp::ListAt, vec![INT], *e, false),
            (Type::List(e), "push") => native(NativeOp::ListPush, vec![*e], UNIT, true),
            (Type::Map(_, _), "len") => native(NativeOp::MapLen, vec![], INT, false),
            (Type::Map(k, _), "has") => native(NativeOp::MapHas, vec![*k], BOOL, false),
            (Type::Map(k, v), "at") => native(NativeOp::MapAt, vec![*k], *v, false),
            (Type::Map(k, v), "put") => native(NativeOp::MapPut, vec![*k, *v], UNIT, true),
            (Type::StringBuilder, "append") => {
                native(NativeOp::SbAppend, vec![STRING], STRING_BUILDER, true)
            }
            (Type::StringBuilder, "build") => native(NativeOp::SbBuild, vec![], STRING, false),
            (Type::ByteBuffer, "append") => {
                native(NativeOp::BbAppend, vec![INT], BYTE_BUFFER, true)
            }
            (Type::ByteBuffer, "len") => native(NativeOp::BbLen, vec![], INT, false),
            (Type::ByteBuffer, "build") => native(NativeOp::BbBuild, vec![], STRING, false),
            _ if name == "freeze" && ctx.store.is_heap(recv_ty) && args.is_empty() => {
                return Ok(freeze_expr(recv_h));
            }
            _ => {
                return Err(Diagnostic::new(
                    if ctx.store.is_heap(recv_ty) {
                        "E1026"
                    } else {
                        "E1027"
                    },
                    format!(
                        "the type {} has no method named `{name}`",
                        ctx.store.display(recv_ty)
                    ),
                    name_span,
                ));
            }
        };
        if needs_mut && !recv_h.mutable {
            return Err(Diagnostic::new(
                "E1035",
                format!("the method `{name}` needs a mutable receiver"),
                name_span,
            ));
        }
        let muts = vec![false; params.len()];
        let mut all_args = vec![recv_h];
        let checked = self.check_args(ctx, args, &params, &muts, name, span)?;
        all_args.extend(checked);
        // Element reads keep the receiver capability.
        let mutable = match op {
            NativeOp::ListAt | NativeOp::MapAt => all_args[0].mutable,
            _ => true,
        };
        Ok(HExpr {
            ty: ret,
            mutable,
            kind: HExprKind::Native { op, args: all_args },
        })
    }

    fn synth_super_call(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let Some(cidx) = self.self_class else {
            return Err(Diagnostic::new(
                "E1039",
                "`super` is only valid inside a method",
                span,
            ));
        };
        let Some(parent) = ctx.classes[cidx as usize].parent else {
            return Err(Diagnostic::new(
                "E1030",
                "the class has no superclass",
                span,
            ));
        };
        if name == "init" {
            let Some(c) = &self.ctor else {
                return Err(Diagnostic::new(
                    "E1030",
                    "`super.init` is only valid inside `init`",
                    span,
                ));
            };
            if !c.needs_super {
                return Err(Diagnostic::new(
                    "E1030",
                    "the parent class has no `init`",
                    span,
                ));
            }
            if c.state.super_done {
                return Err(Diagnostic::new(
                    "E1030",
                    "`super.init` was already called on this path",
                    span,
                ));
            }
            let parent_init = ctx.classes[parent as usize]
                .init
                .clone()
                .expect("needs_super implies parent init");
            let checked = self.check_args(
                ctx,
                args,
                &parent_init.params,
                &parent_init.param_muts,
                "super.init",
                span,
            )?;
            let c = self.ctor.as_mut().expect("ctor");
            let parent_len = ctx.classes[parent as usize].field_tys.len();
            for i in 0..parent_len {
                c.state.inited[i] = true;
            }
            c.state.super_done = true;
            let mut all_args = vec![self.self_value(ctx)];
            all_args.extend(checked);
            return Ok(HExpr {
                ty: UNIT,
                mutable: true,
                kind: HExprKind::Call {
                    func: parent_init.func,
                    args: all_args,
                },
            });
        }
        let sig = ctx.find_method(parent, name).ok_or_else(|| {
            Diagnostic::new(
                "E1026",
                format!("the superclass has no method named `{name}`"),
                name_span,
            )
        })?;
        // The receiver escapes into the superclass method.
        let self_expr = self.synth_self(ctx, span)?;
        if sig.mut_self && !self_expr.mutable {
            return Err(Diagnostic::new(
                "E1035",
                format!("the method `{name}` needs a mutable receiver"),
                name_span,
            ));
        }
        let checked = self.check_args(ctx, args, &sig.params, &sig.param_muts, name, span)?;
        let mut all_args = vec![self_expr];
        all_args.extend(checked);
        Ok(HExpr {
            ty: sig.ret,
            mutable: true,
            kind: HExprKind::Call {
                func: sig.func,
                args: all_args,
            },
        })
    }

    fn synth_index(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        index: &ast::Expr,
        _span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let recv_h = self.synth_expr(ctx, recv)?;
        match ctx.store.get(recv_h.ty).clone() {
            Type::List(elem) => {
                let idx = self.check_expr(ctx, index, INT)?;
                let mutable = recv_h.mutable;
                Ok(HExpr {
                    ty: elem,
                    mutable,
                    kind: HExprKind::Native {
                        op: NativeOp::ListAt,
                        args: vec![recv_h, idx],
                    },
                })
            }
            Type::Map(k, v) => {
                let key = self.check_expr(ctx, index, k)?;
                let mutable = recv_h.mutable;
                Ok(HExpr {
                    ty: v,
                    mutable,
                    kind: HExprKind::Native {
                        op: NativeOp::MapAt,
                        args: vec![recv_h, key],
                    },
                })
            }
            _ => Err(Diagnostic::new(
                "E1027",
                format!(
                    "the type {} does not support indexing",
                    ctx.store.display(recv_h.ty)
                ),
                recv.span,
            )),
        }
    }

    fn check_closure(
        &mut self,
        ctx: &mut Ctx,
        params: &[ast::Param],
        ret: &Option<ast::TypeExpr>,
        expected_ret: Option<TypeId>,
        body: &[ast::Stmt],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let mut ptys = Vec::new();
        let mut pmuts = Vec::new();
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
            ptys.push(resolve_type(ctx, &param.ty)?);
            pmuts.push(param.mutable);
        }
        let declared_ret = match ret {
            Some(ty) => Some(resolve_type(ctx, ty)?),
            None => expected_ret,
        };
        let ret_kind = match declared_ret {
            Some(t) => RetKind::Known(t),
            None => RetKind::ClosureInfer,
        };
        let mut child = FnChecker {
            outer: Some(self as &mut dyn OuterScope),
            locals: ptys
                .iter()
                .zip(pmuts.iter())
                .map(|(t, m)| (*t, *m))
                .collect(),
            scopes: vec![params
                .iter()
                .enumerate()
                .map(|(i, p)| (p.name.clone(), i as u32))
                .collect()],
            captures: Vec::new(),
            is_closure: true,
            loop_depth: 0,
            ret: ret_kind,
            self_class: None,
            ctor: None,
        };
        let (body_h, body_ty, diverges) = match declared_ret {
            Some(t) => {
                let mode = if t == UNIT {
                    BlockMode::Stmt
                } else {
                    BlockMode::Value(t)
                };
                let (b, _, _) = child.check_block(ctx, body, mode, span)?;
                let diverges = b.last().map(HStmt::diverges).unwrap_or(false);
                (b, t, diverges)
            }
            None => {
                let (b, ty, _) = child.check_block(ctx, body, BlockMode::Synth, span)?;
                let diverges = b.last().map(HStmt::diverges).unwrap_or(false);
                let ty = if ty == NEVER { UNIT } else { ty };
                (b, ty, diverges)
            }
        };
        let _ = diverges;
        let locals: Vec<TypeId> = child.locals.iter().map(|(t, _)| *t).collect();
        let capture_tys: Vec<TypeId> = child.captures.iter().map(|c| c.ty).collect();
        let capture_inits: Vec<HExpr> = child
            .captures
            .iter()
            .map(|c| HExpr {
                ty: c.ty,
                mutable: c.mutable,
                kind: match c.source {
                    CapSource::Local(slot) => HExprKind::Local(slot),
                    CapSource::Capture(idx) => HExprKind::Capture(idx),
                },
            })
            .collect();
        child.outer = None;
        let name = format!("<closure {}>", ctx.funcs.len());
        let func = ctx.push_func(
            HirFunc {
                name,
                params: ptys.clone(),
                ret: body_ty,
                captures: capture_tys,
                locals,
                body: body_h,
            },
            FnSig {
                params: ptys.clone(),
                param_muts: pmuts,
                ret: body_ty,
            },
        );
        let fn_ty = ctx.store.intern_fn(ptys, body_ty);
        Ok(HExpr {
            ty: fn_ty,
            mutable: true,
            kind: HExprKind::MakeClosure {
                func,
                captures: capture_inits,
            },
        })
    }

    fn synth_binary(
        &mut self,
        ctx: &mut Ctx,
        op: BinOp,
        left: &ast::Expr,
        right: &ast::Expr,
    ) -> Result<HExpr, Diagnostic> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let l = self.check_expr(ctx, left, INT)?;
                let r = self.check_expr(ctx, right, INT)?;
                Ok(HExpr {
                    ty: INT,
                    mutable: true,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty: INT,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = self.check_expr(ctx, left, INT)?;
                let r = self.check_expr(ctx, right, INT)?;
                Ok(HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty: INT,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
            BinOp::Eq | BinOp::Ne => {
                let l = self.synth_expr(ctx, left)?;
                let r = self.synth_expr(ctx, right)?;
                let related = ctx.store.compatible(l.ty, r.ty) || ctx.store.compatible(r.ty, l.ty);
                if !related {
                    return Err(self.mismatch(ctx, l.ty, r.ty, right.span));
                }
                let operand_ty = if l.ty == NEVER { r.ty } else { l.ty };
                let comparable =
                    matches!(operand_ty, INT | BOOL | STRING) || ctx.store.is_heap(operand_ty);
                if !comparable {
                    return Err(Diagnostic::new(
                        "E1017",
                        format!(
                            "cannot compare {} values with `{}`",
                            ctx.store.display(operand_ty),
                            op.text()
                        ),
                        left.span,
                    ));
                }
                Ok(HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Binary {
                        op,
                        operand_ty,
                        left: Box::new(l),
                        right: Box::new(r),
                    },
                })
            }
        }
    }

    /// Check an `if` expression. `expected` is `Some` in check mode.
    fn check_if(
        &mut self,
        ctx: &mut Ctx,
        arms: &[(ast::Expr, Vec<ast::Stmt>)],
        else_body: &Option<Vec<ast::Stmt>>,
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if let Some(expected) = expected {
            if else_body.is_none() && expected != UNIT {
                return Err(self.mismatch(ctx, expected, UNIT, span));
            }
        }
        let branch_mode = match (expected, else_body) {
            (Some(t), _) => BlockMode::Value(t),
            (None, Some(_)) => BlockMode::Synth,
            // Without `else` the `if` value is `()`. Each branch must
            // also produce `()`. A non-unit branch value is an error,
            // not a silent discard.
            (None, None) => BlockMode::Value(UNIT),
        };
        let mut checked_arms = Vec::new();
        let mut branch_types: Vec<(TypeId, bool, Span)> = Vec::new();
        // Constructor states fork per branch and merge afterwards.
        let mut branch_states: Vec<(CtorState, bool)> = Vec::new();
        let mut ctor_entry: Option<CtorState> = None;
        for (aidx, (cond, body)) in arms.iter().enumerate() {
            // Condition effects flow into every later branch, so the
            // fork snapshot follows the first condition.
            let cond = self.check_expr(ctx, cond, BOOL)?;
            if aidx == 0 {
                ctor_entry = self.ctor.as_ref().map(|c| c.state.clone());
            } else if let (Some(c), Some(entry)) = (self.ctor.as_mut(), &ctor_entry) {
                c.state = entry.clone();
            }
            self.scopes.push(HashMap::new());
            let result = self.check_block(ctx, body, branch_mode, span);
            self.scopes.pop();
            let (body_h, ty, mutable) = result?;
            let diverged = body_h.last().map(HStmt::diverges).unwrap_or(false);
            if let Some(c) = &self.ctor {
                branch_states.push((c.state.clone(), diverged));
            }
            let branch_span = body.last().map(|s| s.span).unwrap_or(span);
            branch_types.push((ty, mutable, branch_span));
            checked_arms.push((cond, body_h));
        }
        let else_h = match else_body {
            Some(body) => {
                if let (Some(c), Some(entry)) = (self.ctor.as_mut(), &ctor_entry) {
                    c.state = entry.clone();
                }
                self.scopes.push(HashMap::new());
                let result = self.check_block(ctx, body, branch_mode, span);
                self.scopes.pop();
                let (body_h, ty, mutable) = result?;
                let diverged = body_h.last().map(HStmt::diverges).unwrap_or(false);
                if let Some(c) = &self.ctor {
                    branch_states.push((c.state.clone(), diverged));
                }
                let branch_span = body.last().map(|s| s.span).unwrap_or(span);
                branch_types.push((ty, mutable, branch_span));
                Some(body_h)
            }
            None => {
                if let Some(entry) = &ctor_entry {
                    branch_states.push((entry.clone(), false));
                }
                None
            }
        };
        // Merge constructor states across the non-diverging branches.
        if let (Some(c), Some(entry)) = (self.ctor.as_mut(), ctor_entry) {
            let live: Vec<&CtorState> = branch_states
                .iter()
                .filter(|(_, diverged)| !diverged)
                .map(|(s, _)| s)
                .collect();
            match live.split_first() {
                None => c.state = entry,
                Some((first, rest)) => {
                    let mut merged = (*first).clone();
                    for state in rest {
                        for (have, new) in merged.inited.iter_mut().zip(state.inited.iter()) {
                            *have = *have && *new;
                        }
                        if merged.super_done != state.super_done {
                            return Err(Diagnostic::new(
                                "E1030",
                                "`super.init` must run exactly once on every path",
                                span,
                            ));
                        }
                    }
                    c.state = merged;
                }
            }
        }
        let (ty, mutable) = match expected {
            Some(t) => {
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (t, mutable)
            }
            None => {
                if else_h.is_none() {
                    (UNIT, true)
                } else {
                    let ty = self.join_branches(ctx, &branch_types)?;
                    let mutable = branch_types.iter().all(|(_, m, _)| *m);
                    (ty, mutable)
                }
            }
        };
        Ok(HExpr {
            ty,
            mutable,
            kind: HExprKind::If {
                arms: checked_arms,
                else_body: else_h,
            },
        })
    }

    /// Join branch types. `Never` branches do not contribute.
    fn join_branches(
        &self,
        ctx: &Ctx,
        branches: &[(TypeId, bool, Span)],
    ) -> Result<TypeId, Diagnostic> {
        let mut joined: Option<TypeId> = None;
        for (ty, _, span) in branches {
            if *ty == NEVER {
                continue;
            }
            match joined {
                None => joined = Some(*ty),
                Some(first) => {
                    joined = Some(
                        ctx.store
                            .join(first, *ty)
                            .ok_or_else(|| self.mismatch(ctx, first, *ty, *span))?,
                    );
                }
            }
        }
        Ok(joined.unwrap_or(NEVER))
    }
}

fn unknown_field(ctx: &Ctx, class: u32, name: &str, span: Span) -> Diagnostic {
    Diagnostic::new(
        "E1025",
        format!(
            "the class `{}` has no field named `{name}`",
            ctx.classes[class as usize].name
        ),
        span,
    )
}

fn freeze_expr(recv: HExpr) -> HExpr {
    let ty = recv.ty;
    let mutable = recv.mutable;
    HExpr {
        ty,
        mutable,
        kind: HExprKind::Native {
            op: NativeOp::Freeze,
            args: vec![recv],
        },
    }
}

/// Replace an empty diagnostic span with a real one.
fn reposition(mut d: Diagnostic, span: Span) -> Diagnostic {
    if d.span.lo == 0 && d.span.hi == 0 {
        d.span = span;
    }
    d
}
