//! Bidirectional expression checking.
//!
//! The checker synthesizes a type for each expression, or checks the
//! expression against an expected type. It resolves names to local
//! slots, capture indices, function indices, class indices, and field
//! layout indices. It infers first-order generic arguments, computes
//! effect rows, proves enum exhaustiveness, applies `is` flow
//! refinement, and tracks definite field initialization inside
//! constructors. It stops at the first error and returns one precise
//! diagnostic.

mod body;
mod call;
mod expr;
mod flow;
mod map_key;
mod member;
mod operator;
mod pattern;
mod scope;
mod sysabi;

use map_key::{map_key_parameter, MapKeyParameter, MapKeyUse};

use crate::check::{
    camel_member, check_key_type, resolve_param_type, resolve_row, resolve_type, snake_member,
    sys_group_name, Ctx, FnSig, InterfaceUse, MethodSig, TyEnv, UseBinding,
};
use crate::exhaust::{useful, APat, PatMeta};
use crate::hir::*;
use lm_source::ast::{self, BinOp, ExprKind, PatternKind};
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{
    ClassId, ClassKind, Row, Type, TypeId, BOOL, DIGEST, FAULT, INT, NEVER, STRING, UNIT,
};
use std::collections::HashSet;

/// The work budget for one pattern usefulness analysis.
const PATTERN_BUDGET: u64 = 1_000_000;

/// How a block result is used.
#[derive(Clone, Copy, PartialEq)]
enum BlockMode {
    /// The block value is discarded. The block type is `()`.
    Discard,
    /// The block must produce a value of the expected type.
    Value(TypeId),
    /// Synthesize the block type from its final expression.
    Synth,
}

#[derive(Clone, Copy)]
enum LoopMode {
    /// `while` and `for` accept only bare breaks.
    UnitOnly,
    /// The loop result is discarded.
    Discard,
    /// Every break value must produce this type.
    Value(TypeId),
    /// The checker joins the break types.
    Synth,
}

struct LoopContext {
    mode: LoopMode,
    breaks: Vec<(TypeId, bool, Span)>,
    inference_gap: Option<Diagnostic>,
}

/// The result-type context of the checked callable.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum RetKind {
    /// The module entry: `return` is invalid.
    Entry,
    /// A closure without a declared result: `return` is invalid.
    ClosureInfer,
    /// A declared result type.
    Known(TypeId),
}

/// The definite-initialization state inside one constructor path.
#[derive(Clone, PartialEq)]
pub(crate) struct CtorState {
    pub(crate) inited: Vec<bool>,
    pub(crate) super_done: bool,
}

pub(crate) struct CtorCtx {
    pub(crate) class: u32,
    pub(crate) needs_super: bool,
    pub(crate) state: CtorState,
}

pub(crate) fn require_complete(
    ctx: &Ctx,
    cidx: u32,
    c: &CtorCtx,
    span: Span,
) -> Result<(), Diagnostic> {
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

/// The source of one captured value in the enclosing frame.
#[derive(Clone, Copy)]
enum CapSource {
    Local(u32),
    Capture(u32),
}

/// One stable value place that a `for` loop reads.
#[derive(Clone, PartialEq, Eq)]
enum IteratedPlace {
    Local(u32),
    Capture(u32),
    Field(Box<IteratedPlace>, u32),
}

fn iterated_place(expr: &HExpr) -> Option<IteratedPlace> {
    match &expr.kind {
        HExprKind::Local(slot) => Some(IteratedPlace::Local(*slot)),
        HExprKind::Capture(slot) => Some(IteratedPlace::Capture(*slot)),
        HExprKind::FieldGet { recv, field } => Some(IteratedPlace::Field(
            Box::new(iterated_place(recv)?),
            *field,
        )),
        _ => None,
    }
}

/// Read one dotted expression as its qualified name and root name.
fn qualified_expr_name(expr: &ast::Expr) -> Option<(String, &str)> {
    fn collect<'a>(expr: &'a ast::Expr, parts: &mut Vec<&'a str>) -> Option<()> {
        match &expr.kind {
            ExprKind::Name(name) => parts.push(name),
            ExprKind::Field { recv, name, .. } => {
                collect(recv, parts)?;
                parts.push(name);
            }
            _ => return None,
        }
        Some(())
    }

    let mut parts = Vec::new();
    collect(expr, &mut parts)?;
    let root = *parts.first()?;
    Some((parts.join("."), root))
}

/// Add one final member to a dotted expression name.
fn qualified_expr_name_with_member<'a>(
    expr: &'a ast::Expr,
    member: &str,
) -> Option<(String, &'a str)> {
    let (mut name, root) = qualified_expr_name(expr)?;
    name.push('.');
    name.push_str(member);
    Some((name, root))
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

/// The result of checking the module entry: body, type, capability,
/// locals, and the collected entry row.
pub(crate) type CheckedEntry = (Vec<HStmt>, TypeId, bool, Vec<TypeId>, Row);

/// The result of checking one callable body.
pub(crate) struct CheckedBody {
    pub(crate) body: Vec<HStmt>,
    pub(crate) locals: Vec<TypeId>,
    pub(crate) type_bounds: Vec<Vec<InterfaceUse>>,
    pub(crate) diverges: bool,
    pub(crate) ctor: Option<CtorCtx>,
}

enum NameRes {
    Local(u32, TypeId, bool),
    Capture(u32, TypeId, bool),
}

/// One lexical scope. Loom scopes usually contain few names.
#[derive(Default)]
pub(crate) struct Scope {
    bindings: Vec<(String, u32)>,
}

impl Scope {
    pub(crate) fn insert(&mut self, name: String, slot: u32) -> Option<u32> {
        if let Some((_, old)) = self.bindings.iter_mut().find(|(item, _)| item == &name) {
            return Some(std::mem::replace(old, slot));
        }
        self.bindings.push((name, slot));
        None
    }

    pub(crate) fn get(&self, name: &str) -> Option<&u32> {
        self.bindings
            .iter()
            .find(|(item, _)| item == name)
            .map(|(_, slot)| slot)
    }

    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &u32)> {
        self.bindings.iter().map(|(name, slot)| (name, slot))
    }
}

impl FromIterator<(String, u32)> for Scope {
    fn from_iter<T: IntoIterator<Item = (String, u32)>>(items: T) -> Self {
        let mut scope = Scope::default();
        for (name, slot) in items {
            scope.insert(name, slot);
        }
        scope
    }
}

/// The resolved meaning of one called name.
enum Callee {
    Value(HExpr),
    Func(u32),
    Class(u32),
    Ctor {
        arm: u32,
    },
    /// `List[T]()` with explicit arguments.
    ListCtor(TypeId),
    /// `Map[K, V]()` with explicit arguments.
    MapCtor(TypeId, TypeId),
    /// A `use`-bound callable `sys` member, for example `write` after
    /// `use sys.io.write`.
    SysMember {
        group: String,
        member: String,
    },
    /// A `use`-bound `sys` group object, which is not callable.
    SysGroup(String),
}

/// The output of one polymorphic call check.
struct PolyOut {
    targs: Vec<TypeId>,
    rowargs: Vec<Row>,
    args: Vec<HExpr>,
    ret: TypeId,
}

pub(crate) struct FnChecker<'o> {
    outer: Option<&'o mut dyn OuterScope>,
    pub(crate) locals: Vec<(TypeId, bool)>,
    pub(crate) scopes: Vec<Scope>,
    captures: Vec<CaptureRec>,
    is_closure: bool,
    loops: Vec<LoopContext>,
    iterated_places: Vec<IteratedPlace>,
    /// True when the body holds a `return`. It witnesses the
    /// declared result type.
    saw_return: bool,
    ret: RetKind,
    pub(crate) self_class: Option<u32>,
    pub(crate) ctor: Option<CtorCtx>,
    pub(crate) env: TyEnv,
    declared_row: Row,
    /// True for the module entry: charged rows accumulate into
    /// `declared_row` instead of raising `E1046`.
    collect_row: bool,
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

/// One resolved operator hook: the receiver class, its type
/// arguments, and the method the class declares.
type OperatorHook = (u32, Vec<TypeId>, (std::rc::Rc<MethodSig>, Vec<TypeId>, u32));

/// Extract the nominal class and arguments of one instance type.
fn class_of(ctx: &Ctx, ty: TypeId) -> Option<(u32, Vec<TypeId>)> {
    ctx.store
        .nominal_class(ty)
        .map(|(class, args)| (class.0, args))
}

/// Collect the type-variable indices inside one type.
fn collect_vars(ctx: &Ctx, ty: TypeId, out: &mut HashSet<u32>) {
    match ctx.store.get(ty) {
        Type::Var(i) => {
            out.insert(*i);
        }
        Type::Inst(_, args) => {
            for a in args.clone() {
                collect_vars(ctx, a, out);
            }
        }
        Type::List(e) => collect_vars(ctx, *e, out),
        Type::Map(k, v) => {
            let (k, v) = (*k, *v);
            collect_vars(ctx, k, out);
            collect_vars(ctx, v, out);
        }
        Type::Tuple(elems) => {
            for e in elems.clone() {
                collect_vars(ctx, e, out);
            }
        }
        Type::Fn(params, _, ret, _) => {
            let ret = *ret;
            for p in params.clone() {
                collect_vars(ctx, p, out);
            }
            collect_vars(ctx, ret, out);
        }
        _ => {}
    }
}

/// True when the field types of a constructor determine every family
/// type parameter.
fn ctor_determined(ctx: &Ctx, field_tys: &[TypeId], param_count: usize) -> bool {
    let mut seen = HashSet::new();
    for ty in field_tys {
        collect_vars(ctx, *ty, &mut seen);
    }
    (0..param_count as u32).all(|i| seen.contains(&i))
}

/// First-order unification: bind type variables of `decl` against the
/// concrete type `actual`. Binding is best effort; the final per-
/// argument compatibility check reports precise mismatches.
fn unify(
    ctx: &mut Ctx,
    decl: TypeId,
    actual: TypeId,
    targs: &mut [Option<TypeId>],
    rowargs: &mut [Option<Row>],
    covariant: bool,
) {
    if actual == NEVER {
        return;
    }
    let d = ctx.store.get(decl).clone();
    let a = ctx.store.get(actual).clone();
    match (d, a) {
        (Type::Var(i), _) => {
            let i = i as usize;
            if i >= targs.len() {
                return;
            }
            match targs[i] {
                None => targs[i] = Some(actual),
                Some(prev) if prev == actual => {}
                Some(prev) => {
                    if covariant {
                        if let Some(joined) = ctx.store.join(prev, actual) {
                            targs[i] = Some(joined);
                        }
                    }
                }
            }
        }
        (Type::Inst(dc, dargs), Type::Inst(ac, aargs)) => {
            // Applications are invariant, so the arguments must be
            // equal in both subtype directions. Either nominal
            // direction can bind: an argument value narrows the
            // declared family; an expected result widens it.
            let related = ctx.store.class_extends(ac, dc) || ctx.store.class_extends(dc, ac);
            if dargs.len() == aargs.len() && related {
                for (d, a) in dargs.iter().zip(aargs.iter()) {
                    unify(ctx, *d, *a, targs, rowargs, false);
                }
            }
        }
        (Type::List(d), Type::List(a)) => unify(ctx, d, a, targs, rowargs, false),
        (Type::Map(dk, dv), Type::Map(ak, av)) => {
            unify(ctx, dk, ak, targs, rowargs, false);
            unify(ctx, dv, av, targs, rowargs, false);
        }
        (Type::Tuple(ds), Type::Tuple(as_)) => {
            if ds.len() == as_.len() {
                for (d, a) in ds.iter().zip(as_.iter()) {
                    unify(ctx, *d, *a, targs, rowargs, covariant);
                }
            }
        }
        (Type::Fn(dp, _, dr, drow), Type::Fn(ap, _, ar, arow))
        | (Type::Callback(dp, _, dr, drow), Type::Fn(ap, _, ar, arow))
        | (Type::Callback(dp, _, dr, drow), Type::Callback(ap, _, ar, arow)) => {
            if dp.len() == ap.len() {
                for (d, a) in dp.iter().zip(ap.iter()) {
                    unify(ctx, *d, *a, targs, rowargs, false);
                }
                unify(ctx, dr, ar, targs, rowargs, true);
                if let [lm_types::RowElem::Var(e)] = drow[..] {
                    let e = e as usize;
                    if e < rowargs.len() {
                        let mut merged = rowargs[e].clone().unwrap_or_default();
                        merged.extend_from_slice(&arow);
                        rowargs[e] = Some(ctx.store.canonical_row(merged));
                    }
                }
            }
        }
        _ => {}
    }
}

/// Infer one effect argument from an interface application row.
fn infer_bound_row(ctx: &Ctx, declared: &Row, actual: &Row, rowargs: &mut [Option<Row>]) {
    let mut variables = Vec::new();
    for elem in declared {
        let lm_types::RowElem::Var(index) = elem else {
            continue;
        };
        let index = *index as usize;
        if index < rowargs.len() && !variables.contains(&index) {
            variables.push(index);
        }
    }
    let solve = if variables.len() == 1 {
        Some(variables[0])
    } else {
        let unresolved: Vec<usize> = variables
            .iter()
            .copied()
            .filter(|index| rowargs[*index].is_none())
            .collect();
        (unresolved.len() == 1).then(|| unresolved[0])
    };
    let Some(solve) = solve else {
        return;
    };

    let mut fixed = Vec::new();
    for elem in declared {
        match elem {
            lm_types::RowElem::Var(index) if *index as usize == solve => {}
            lm_types::RowElem::Var(index) => {
                let Some(Some(row)) = rowargs.get(*index as usize) else {
                    return;
                };
                fixed.extend_from_slice(row);
            }
            lm_types::RowElem::Op(_) => fixed.push(*elem),
        }
    }
    let fixed = ctx.store.canonical_row(fixed);
    if fixed.iter().any(|elem| !actual.contains(elem)) {
        return;
    }
    let inferred = ctx.store.canonical_row(
        actual
            .iter()
            .copied()
            .filter(|elem| !fixed.contains(elem))
            .collect(),
    );
    if rowargs[solve]
        .as_ref()
        .is_none_or(|held| ctx.store.row_included(held, &inferred))
    {
        rowargs[solve] = Some(inferred);
    }
}

/// Return true when one function value fits a callback parameter.
fn callback_accepts(ctx: &Ctx, expected: TypeId, found: TypeId) -> bool {
    let Type::Callback(ep, em, er, erow) = ctx.store.get(expected) else {
        return false;
    };
    let (fp, fm, fr, frow) = match ctx.store.get(found) {
        Type::Fn(params, muts, ret, row) => (params, muts, ret, row),
        _ => return false,
    };
    fp.len() == ep.len()
        && fp
            .iter()
            .zip(ep.iter())
            .all(|(actual, required)| ctx.store.compatible(*actual, *required))
        && fm
            .iter()
            .zip(em.iter())
            .all(|(actual, required)| !*actual || *required)
        && ctx.store.compatible(*er, *fr)
        && ctx.store.row_included(frow, erow)
}

/// True when the diagnostic reports unresolved constructor type
/// arguments that a sibling type can bind.
fn is_inference_gap(d: &Diagnostic) -> bool {
    d.code == "E1045"
}

/// Widen every arm-typed nominal position to its enum family, at the
/// top level and inside collection, tuple, and application arguments.
/// The result serves as the sibling inference hint: it is the type
/// every sibling of a literal or branch join can adopt.
fn deep_widen(ctx: &mut Ctx, ty: TypeId) -> TypeId {
    match ctx.store.get(ty).clone() {
        Type::Class(c) => match ctx.family_of(c.0) {
            Some(f) if f != c.0 => ctx.store.intern(Type::Class(ClassId(f))),
            _ => ty,
        },
        Type::Inst(c, args) => {
            let args: Vec<TypeId> = args.iter().map(|a| deep_widen(ctx, *a)).collect();
            let class = ctx.family_of(c.0).unwrap_or(c.0);
            ctx.store.intern(Type::Inst(ClassId(class), args))
        }
        Type::List(e) => {
            let e = deep_widen(ctx, e);
            ctx.store.intern(Type::List(e))
        }
        Type::Map(k, v) => {
            let k = deep_widen(ctx, k);
            let v = deep_widen(ctx, v);
            ctx.store.intern(Type::Map(k, v))
        }
        Type::Tuple(elems) => {
            let elems: Vec<TypeId> = elems.iter().map(|e| deep_widen(ctx, *e)).collect();
            ctx.store.intern(Type::Tuple(elems))
        }
        _ => ty,
    }
}

/// Extend a call mismatch when the constructor name collides with an
/// arm of the expected enum. The unqualified name resolved against a
/// different family, so the note names the qualified form.
fn note_ctor_collision(ctx: &Ctx, mut d: Diagnostic, name: &str, expected: TypeId) -> Diagnostic {
    if d.code != "E1004" {
        return d;
    }
    let Some((class, _)) = class_of(ctx, expected) else {
        return d;
    };
    let Some(family) = ctx.family_of(class) else {
        return d;
    };
    if ctx.find_arm(family, name).is_none() {
        return d;
    }
    let family_name = &ctx.classes[family as usize].name;
    d.message.push_str(&format!(
        "; the enum `{family_name}` has an arm named `{name}`; write \
         `{family_name}.{name}(...)` to select it"
    ));
    d
}

/// The patterns that cover every value OUTSIDE the static type `ty`.
///
/// An arm-typed position excludes its sibling arms, at the top level
/// and inside every arm-typed field position. A family-typed or
/// non-nominal position excludes nothing. The recursion follows the
/// finite type tree, so it terminates.
fn impossible_patterns(ctx: &Ctx, ty: TypeId) -> Vec<APat> {
    let Some((class, class_args)) = class_of(ctx, ty) else {
        return Vec::new();
    };
    if ctx.classes[class as usize].kind != ClassKind::EnumCase {
        return Vec::new();
    }
    let family = ctx.classes[class as usize].family.expect("case has family");
    let mut out = Vec::new();
    for sibling in &ctx.classes[family as usize].arms {
        if *sibling != class {
            let arity = ctx.classes[*sibling as usize].field_tys.len();
            out.push(APat::Ctor(*sibling, vec![APat::Wild; arity]));
        }
    }
    let field_tys = ctx.classes[class as usize].field_tys.clone();
    for (fidx, field_ty) in field_tys.iter().enumerate() {
        // The stored field types are in the family variable space;
        // the class arguments instantiate them.
        let sub_ty = subst_ro(ctx, *field_ty, &class_args);
        for inner in impossible_patterns(ctx, sub_ty) {
            let mut args = vec![APat::Wild; field_tys.len()];
            args[fidx] = inner;
            out.push(APat::Ctor(class, args));
        }
    }
    out
}

/// Substitute class arguments without mutable store access. The
/// exhaustiveness pass runs with a shared context, so this helper
/// resolves already-interned substitutions only and keeps a variable
/// when the result type was never interned.
fn subst_ro(ctx: &Ctx, ty: TypeId, args: &[TypeId]) -> TypeId {
    if args.is_empty() {
        return ty;
    }
    match ctx.store.get(ty) {
        Type::Var(i) => args.get(*i as usize).copied().unwrap_or(ty),
        _ => ty,
    }
}

/// The names of a call target that declares none. A call through a
/// closure value and a direct operation call both use it: a closure
/// type carries no names, and the operation manifest carries none.
const NO_NAMES: &[&str] = &[];

/// Arrange call arguments against the declared parameter names.
///
/// Labels follow the positional arguments and match declared names in
/// any order. The result unwraps the labels and orders the arguments
/// by parameter declaration. Labels change nothing in the call ABI.
/// The caller checks the argument count first.
fn arrange_args<'a, N: AsRef<str>>(
    args: &'a [ast::Expr],
    param_names: &[N],
    what: &str,
) -> Result<Vec<&'a ast::Expr>, Diagnostic> {
    if args
        .iter()
        .all(|a| !matches!(a.kind, ExprKind::Labeled { .. }))
    {
        return Ok(args.iter().collect());
    }
    // A declaration states one name for each parameter, or no name at
    // all. The caller matched the argument count against the
    // parameter count, so a label position indexes `slots`.
    debug_assert!(
        param_names.is_empty() || param_names.len() == args.len(),
        "`{what}` states {} name(s) for {} parameter(s)",
        param_names.len(),
        args.len()
    );
    let mut slots: Vec<Option<&ast::Expr>> = vec![None; args.len()];
    let mut positional = 0usize;
    let mut in_labels = false;
    for arg in args {
        match &arg.kind {
            ExprKind::Labeled { label, value } => {
                in_labels = true;
                let Some(pos) = param_names.iter().position(|n| n.as_ref() == label) else {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`{what}` does not declare a parameter named `{label}`"),
                        arg.span,
                    ));
                };
                if pos < positional {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "the label `{label}:` names a parameter that a \
                             positional argument already fills"
                        ),
                        arg.span,
                    ));
                }
                if slots[pos].is_some() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("the argument label `{label}:` appears more than one time"),
                        arg.span,
                    ));
                }
                slots[pos] = Some(value.as_ref());
            }
            _ => {
                if in_labels {
                    return Err(Diagnostic::new(
                        "E1006",
                        "a positional argument cannot follow a labeled argument",
                        arg.span,
                    ));
                }
                slots[positional] = Some(arg);
                positional += 1;
            }
        }
    }
    // The count check passed and no slot was filled twice, so every
    // slot holds one argument.
    Ok(slots
        .into_iter()
        .map(|s| s.expect("every parameter is filled"))
        .collect())
}

fn hpat_to_apat(pat: &HPattern) -> APat {
    match pat {
        HPattern::Wildcard | HPattern::Bind(_) => APat::Wild,
        HPattern::Tuple { elems, .. } => APat::Tuple(elems.iter().map(hpat_to_apat).collect()),
        // An operation test keeps its identity. The inner patterns
        // stay out of the analysis, so a request arm never reads as
        // exhaustive and never hides a later arm.
        HPattern::Project {
            projection: Projection::AsCall(op),
            ..
        } => APat::Call(*op),
        HPattern::Project { inner, .. } => hpat_to_apat(inner),
        HPattern::And(subs) => subs
            .iter()
            .map(hpat_to_apat)
            .find(|a| *a != APat::Wild)
            .unwrap_or(APat::Wild),
        HPattern::Int(v) => APat::Int(*v),
        HPattern::Bool(v) => APat::Bool(*v),
        HPattern::Char(v) => APat::Char(*v),
        HPattern::Str(v) => APat::Str(v.clone()),
        HPattern::Ctor { class, args, .. } => {
            APat::Ctor(*class, args.iter().map(hpat_to_apat).collect())
        }
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
        flow: Flow::Normal,
        ty,
        mutable,
        kind: HExprKind::Native {
            op: NativeOp::Freeze,
            args: vec![recv],
        },
    }
}

/// The canonical digest of one frozen graph. The result is a frozen
/// `Digest` value, so it is comparable by value and sendable.
fn digest_expr(recv: HExpr) -> HExpr {
    HExpr {
        flow: Flow::Normal,
        ty: DIGEST,
        mutable: true,
        kind: HExprKind::Native {
            op: NativeOp::Digest,
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

/// True when every element of the tuple type supports `==` under the
/// structural tuple rule.
fn tuple_comparable(store: &lm_types::TypeStore, ty: TypeId) -> bool {
    match store.get(ty) {
        Type::Tuple(elems) => {
            let elems = elems.clone();
            elems.iter().all(|e| match store.get(*e) {
                Type::Tuple(_) => tuple_comparable(store, *e),
                _ => {
                    *e == lm_types::UNIT
                        || matches!(*e, lm_types::INT | lm_types::BOOL | lm_types::STRING)
                        || store.is_heap(*e)
                }
            })
        }
        _ => false,
    }
}

/// Build the nested `Choice` pattern for one select arm.
fn select_pattern(arm: &ast::SelectArm, index: usize, count: usize) -> ast::Pattern {
    let mut pattern = ast::Pattern {
        kind: if arm.binding == "_" {
            PatternKind::Wildcard
        } else {
            PatternKind::Name(arm.binding.clone())
        },
        span: arm.binding_span,
    };
    if index == 0 {
        for _ in 0..count.saturating_sub(1) {
            pattern = wrap_choice_pattern(pattern, "First", arm.span);
        }
    } else {
        pattern = wrap_choice_pattern(pattern, "Second", arm.span);
        for _ in 0..count.saturating_sub(index + 1) {
            pattern = wrap_choice_pattern(pattern, "First", arm.span);
        }
    }
    pattern
}

fn wrap_choice_pattern(inner: ast::Pattern, name: &str, span: Span) -> ast::Pattern {
    ast::Pattern {
        kind: PatternKind::Ctor {
            qualifier: Some("Choice".to_string()),
            name: name.to_string(),
            args: vec![inner],
            has_parens: true,
        },
        span,
    }
}
