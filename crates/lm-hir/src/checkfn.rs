//! Bidirectional expression and statement checking.
//!
//! The checker synthesizes a type for each expression, or checks the
//! expression against an expected type. It resolves names to local
//! slots, capture indices, function indices, class indices, and field
//! layout indices. It infers first-order generic arguments, computes
//! effect rows, proves enum exhaustiveness, applies `is` flow
//! refinement, and tracks definite field initialization inside
//! constructors. It stops at the first error and returns one precise
//! diagnostic.

use crate::check::{
    camel_member, check_key_type, resolve_row, resolve_type, snake_member, sys_group_name, Ctx,
    FnSig, MethodSig, TyEnv, UseBinding,
};
use crate::exhaust::{useful, APat, PatMeta};
use crate::hir::*;
use lm_source::ast::{self, BinOp, ExprKind, PatternKind, StmtKind};
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{ClassId, ClassKind, Row, Type, TypeId, BOOL, DIGEST, INT, NEVER, STRING, UNIT};
use std::collections::{HashMap, HashSet};

/// The work budget for one pattern usefulness analysis.
const PATTERN_BUDGET: u64 = 1_000_000;

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
    pub(crate) diverges: bool,
    pub(crate) ctor: Option<CtorCtx>,
}

enum NameRes {
    Local(u32, TypeId, bool),
    Capture(u32, TypeId, bool),
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
    /// A `use`-bound callable `sys` member, for example `print` after
    /// `use sys.io.print`.
    SysMember {
        group: &'static str,
        member: String,
    },
    /// A `use`-bound `sys` group object, which is not callable.
    SysGroup(&'static str),
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
    pub(crate) scopes: Vec<HashMap<String, u32>>,
    captures: Vec<CaptureRec>,
    is_closure: bool,
    loop_depth: u32,
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
type OperatorHook = (u32, Vec<TypeId>, (MethodSig, Vec<TypeId>, u32));

/// Extract the nominal class and arguments of one instance type.
fn class_of(ctx: &Ctx, ty: TypeId) -> Option<(u32, Vec<TypeId>)> {
    match ctx.store.get(ty) {
        Type::Int => ctx
            .core_types
            .get("Int")
            .copied()
            .map(|class| (class, vec![])),
        Type::Bool => ctx
            .core_types
            .get("Bool")
            .copied()
            .map(|class| (class, vec![])),
        Type::String => ctx
            .core_types
            .get("String")
            .copied()
            .map(|class| (class, vec![])),
        Type::Bytes => ctx
            .core_types
            .get("Bytes")
            .copied()
            .map(|class| (class, vec![])),
        Type::Class(c) => Some((c.0, vec![])),
        Type::Inst(c, args) => Some((c.0, args.clone())),
        _ => None,
    }
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
        (Type::Fn(dp, _, dr, drow), Type::Fn(ap, _, ar, arow)) => {
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

impl<'o> FnChecker<'o> {
    pub(crate) fn top_level(ret: RetKind, env: TyEnv, declared_row: Row) -> FnChecker<'static> {
        FnChecker {
            outer: None,
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            captures: Vec::new(),
            is_closure: false,
            loop_depth: 0,
            saw_return: false,
            ret,
            self_class: None,
            ctor: None,
            env,
            declared_row,
            collect_row: false,
        }
    }

    /// The checker for the module entry. The entry has no declared
    /// row; the charged rows accumulate into the inferred entry row.
    pub(crate) fn entry_collect(env: TyEnv) -> FnChecker<'static> {
        let mut checker = FnChecker::top_level(RetKind::Entry, env, vec![]);
        checker.collect_row = true;
        checker
    }

    fn lookup_slot(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Some(*slot);
            }
        }
        None
    }

    /// Find one user module function in a user body.
    fn module_func(&self, ctx: &Ctx, name: &str) -> Option<u32> {
        if self.env.core_scope {
            None
        } else {
            ctx.func_index.get(name).copied()
        }
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

    /// Require the row of a call to be inside the declared row. The
    /// entry checker collects the union instead.
    fn charge_row(&mut self, ctx: &Ctx, row: &Row, span: Span) -> Result<(), Diagnostic> {
        if ctx.store.row_included(row, &self.declared_row) {
            return Ok(());
        }
        if self.collect_row {
            let mut merged = self.declared_row.clone();
            merged.extend_from_slice(row);
            self.declared_row = ctx.store.canonical_row(merged);
            return Ok(());
        }
        let declared = if self.declared_row.is_empty() {
            "empty".to_string()
        } else {
            format!("`{}`", ctx.store.display_row(&self.declared_row))
        };
        Err(Diagnostic::new(
            "E1046",
            format!(
                "this call needs the effect row `{}`, but the declared row \
                 of the enclosing callable is {declared}",
                ctx.store.display_row(row)
            ),
            span,
        ))
    }

    /// Check a full callable body and package the result.
    pub(crate) fn check_callable(
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
        // A callable that never completes normally and holds no
        // `return` produces no value of its declared type. Nothing in
        // the body states that type, so `Never` is the honest one.
        // `()` stays legal: it claims no value in the first place.
        if diverges && !self.saw_return && ret != UNIT && ret != NEVER {
            return Err(Diagnostic::new(
                "E1063",
                format!(
                    "this callable never returns, and no `return` gives a value \
                     of type {}; declare the result type `Never`",
                    ctx.store.display(ret)
                ),
                span,
            ));
        }
        Ok(CheckedBody {
            body,
            locals: self.locals.iter().map(|(t, _)| *t).collect(),
            diverges,
            ctor: self.ctor,
        })
    }

    /// Check the module entry block and synthesize its type. The last
    /// result element is the collected entry row.
    pub(crate) fn check_entry(
        mut self,
        ctx: &mut Ctx,
        stmts: &[ast::Stmt],
        span: Span,
    ) -> Result<CheckedEntry, Diagnostic> {
        let (body, ty, mutable) = self.check_block(ctx, stmts, BlockMode::Synth, span)?;
        let locals = self.locals.iter().map(|(t, _)| *t).collect();
        Ok((body, ty, mutable, locals, self.declared_row))
    }

    /// Check one expression against an expected type, exposed for
    /// field defaults.
    pub(crate) fn check_expr(
        &mut self,
        ctx: &mut Ctx,
        expr: &ast::Expr,
        expected: TypeId,
    ) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::If { arms, else_body } => {
                self.check_if(ctx, arms, else_body, Some(expected), expr.span)
            }
            ExprKind::Case { scrut, arms } => {
                self.check_case(ctx, scrut, arms, Some(expected), expr.span)
            }
            ExprKind::Select { arms } => self.check_select(ctx, arms, Some(expected), expr.span),
            ExprKind::TupleLit(items) => {
                if let Type::Tuple(elems) = ctx.store.get(expected).clone() {
                    if elems.len() == items.len() {
                        let mut checked = Vec::new();
                        for (item, elem) in items.iter().zip(elems.iter()) {
                            checked.push(self.check_expr(ctx, item, *elem)?);
                        }
                        return Ok(HExpr {
                            ty: expected,
                            mutable: true,
                            kind: HExprKind::TupleLit(checked),
                        });
                    }
                }
                let found = self.synth_expr(ctx, expr)?;
                self.expect_compatible(ctx, expected, found, expr.span)
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
                self.expect_compatible(ctx, expected, found, expr.span)
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
                self.expect_compatible(ctx, expected, found, expr.span)
            }
            ExprKind::Closure {
                params,
                ret,
                row,
                body,
            } => {
                let expected_ret = match (ret, ctx.store.get(expected)) {
                    (None, Type::Fn(_, _, r, _)) => Some(*r),
                    _ => None,
                };
                let found =
                    self.check_closure(ctx, params, ret, row, expected_ret, body, expr.span)?;
                self.expect_compatible(ctx, expected, found, expr.span)
            }
            ExprKind::Name(name) => {
                if self.lookup_slot(name).is_none() {
                    if let Some(found) = self.try_ctor_name(ctx, name, expr.span, Some(expected))? {
                        return self.expect_compatible(ctx, expected, found, expr.span);
                    }
                }
                let found = self.synth_expr(ctx, expr)?;
                self.expect_compatible(ctx, expected, found, expr.span)
            }
            ExprKind::Call {
                name,
                name_span,
                type_args,
                args,
            } => {
                let found = self.call_named(
                    ctx,
                    name,
                    *name_span,
                    type_args,
                    args,
                    Some(expected),
                    expr.span,
                )?;
                self.expect_compatible(ctx, expected, found, expr.span)
                    .map_err(|d| note_ctor_collision(ctx, d, name, expected))
            }
            ExprKind::MethodCall {
                recv,
                name,
                name_span,
                type_args,
                args,
            } => {
                let found = self.synth_method_call(
                    ctx,
                    recv,
                    name,
                    *name_span,
                    type_args,
                    args,
                    Some(expected),
                    expr.span,
                )?;
                self.expect_compatible(ctx, expected, found, expr.span)
            }
            ExprKind::Field {
                recv,
                name,
                name_span,
            } => {
                let found = self.synth_field(ctx, recv, name, *name_span, Some(expected))?;
                self.expect_compatible(ctx, expected, found, expr.span)
            }
            _ => {
                let found = self.synth_expr(ctx, expr)?;
                self.expect_compatible(ctx, expected, found, expr.span)
            }
        }
    }

    fn expect_compatible(
        &self,
        ctx: &Ctx,
        expected: TypeId,
        found: HExpr,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if !ctx.store.compatible(expected, found.ty) {
            return Err(self.mismatch(ctx, expected, found.ty, span));
        }
        Ok(found)
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
            // A loop with no `break` is a diverging tail too. The
            // body decides, so this checks the loop first.
            StmtKind::While { .. } => {
                let checked = self.check_stmt(ctx, stmt)?;
                if checked.diverges() || expected == UNIT {
                    Ok((checked, true))
                } else {
                    Err(self.mismatch(ctx, expected, UNIT, stmt.span))
                }
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
                self.saw_return = true;
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
            Some(NameRes::Local(slot, _, was_mutable)) => {
                if ty.is_some() {
                    return Err(Diagnostic::new(
                        "E1020",
                        format!("the name `{name}` already has a declaration"),
                        name_span,
                    ));
                }
                let expected = self.locals[slot as usize].0;
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
                if self.module_func(ctx, name).is_some()
                    || ctx.lookup_type(name, &self.env).is_some()
                {
                    return Err(Diagnostic::new(
                        "E1019",
                        format!("cannot assign to `{name}`"),
                        name_span,
                    ));
                }
                // The first assignment declares a new local.
                let (value, local_ty) = match ty {
                    Some(annotation) => {
                        let annotated = resolve_type(ctx, &self.env, annotation)?;
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
            let self_expr = self.self_value();
            return Ok(HStmt::AssignField {
                recv: self_expr,
                field: fidx as u32,
                value,
            });
        }
        let recv_h = self.synth_expr(ctx, recv)?;
        let Some((class, class_args)) = class_of(ctx, recv_h.ty) else {
            return Err(Diagnostic::new(
                "E1027",
                format!("the type {} has no fields", ctx.store.display(recv_h.ty)),
                recv.span,
            ));
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
        let declared = ctx.classes[class as usize].field_tys[fidx];
        let field_ty = ctx.store.substitute(declared, &class_args, &[]);
        let value = self.check_expr(ctx, value, field_ty)?;
        Ok(HStmt::AssignField {
            recv: recv_h,
            field: fidx as u32,
            value,
        })
    }

    /// Build the `self` expression for a method body.
    fn self_value(&self) -> HExpr {
        let (ty, mutable) = self.locals[0];
        debug_assert_eq!(self.lookup_slot("self"), Some(0));
        HExpr {
            ty,
            mutable,
            kind: HExprKind::Local(0),
        }
    }

    /// Synthesize an expression type.
    fn synth_expr(&mut self, ctx: &mut Ctx, expr: &ast::Expr) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::Unit => Ok(HExpr {
                ty: UNIT,
                mutable: true,
                kind: HExprKind::Unit,
            }),
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
            ExprKind::Interp(parts) => self.synth_interp(ctx, parts),
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
                if let Some(found) = self.try_ctor_name(ctx, name, expr.span, None)? {
                    return Ok(found);
                }
                if name == "sys" {
                    return Err(Diagnostic::new(
                        "E1051",
                        "`sys` is not a value; use `sys.<group>.<operation>`",
                        expr.span,
                    ));
                }
                match self.use_binding(ctx, name)? {
                    Some(UseBinding::SysMember { group, member }) => {
                        return self.check_sys_value(ctx, group, &member, expr.span);
                    }
                    Some(UseBinding::SysGroup(group)) => {
                        return Err(Diagnostic::new(
                            "E1051",
                            format!(
                                "`{name}` is the `sys.{}` group object and not a \
                                 value; name an operation such as `{name}.<operation>`",
                                group.to_ascii_lowercase()
                            ),
                            expr.span,
                        ));
                    }
                    Some(UseBinding::Module(path)) => {
                        return Err(Diagnostic::new(
                            "E1051",
                            format!(
                                "`{name}` names the module `{path}` and is not a \
                                 value; name one of its definitions"
                            ),
                            expr.span,
                        ));
                    }
                    None => {}
                }
                if let Some(func) = self.module_func(ctx, name) {
                    let sig = ctx.sigs[func as usize].clone();
                    if !sig.type_params.is_empty() || !sig.effect_params.is_empty() {
                        return Err(Diagnostic::new(
                            "E1024",
                            format!("the generic function `{name}` needs a direct call"),
                            expr.span,
                        ));
                    }
                    let ty = ctx
                        .store
                        .intern_fn(sig.params, sig.param_muts, sig.ret, sig.row);
                    return Ok(HExpr {
                        ty,
                        mutable: true,
                        kind: HExprKind::MakeClosure {
                            func,
                            captures: vec![],
                        },
                    });
                }
                if let Some(class) = ctx.lookup_type(name, &self.env) {
                    let what = if ctx.classes[class as usize].kind == ClassKind::EnumParent {
                        "enum"
                    } else {
                        "class"
                    };
                    return Err(Diagnostic::new(
                        "E1018",
                        format!("the {what} `{name}` is not a value in this language slice"),
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
                Ok(Self::primitive_operator(
                    ctx,
                    "Bool",
                    "__not__",
                    vec![inner],
                ))
            }
            ExprKind::Neg(inner) => {
                let value = self.synth_expr(ctx, inner)?;
                if let Some((class, cargs, found)) =
                    Self::find_operator_hook(ctx, value.ty, "__neg__")
                {
                    return self.operator_hook(
                        ctx,
                        value,
                        class,
                        cargs,
                        found,
                        "__neg__",
                        &[],
                        expr.span,
                    );
                }
                let value = self.expect_compatible(ctx, INT, value, inner.span)?;
                Ok(Self::primitive_operator(ctx, "Int", "__neg__", vec![value]))
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
            ExprKind::Is { value, ty } => self.synth_is(ctx, value, ty, expr.span, false),
            ExprKind::Cast { value, ty } => self.synth_is(ctx, value, ty, expr.span, true),
            ExprKind::Call {
                name,
                name_span,
                type_args,
                args,
            } => self.call_named(ctx, name, *name_span, type_args, args, None, expr.span),
            ExprKind::CallExpr { callee, args } => {
                let callee_h = self.synth_expr(ctx, callee)?;
                self.synth_call_value(ctx, callee_h, args, callee.span, expr.span)
            }
            ExprKind::Field {
                recv,
                name,
                name_span,
            } => self.synth_field(ctx, recv, name, *name_span, None),
            ExprKind::MethodCall {
                recv,
                name,
                name_span,
                type_args,
                args,
            } => self.synth_method_call(
                ctx, recv, name, *name_span, type_args, args, None, expr.span,
            ),
            ExprKind::SuperCall {
                name,
                name_span,
                args,
            } => self.synth_super_call(ctx, name, *name_span, args, expr.span),
            ExprKind::Index { recv, index } => self.synth_index(ctx, recv, index, expr.span),
            ExprKind::TupleLit(items) => {
                let mut checked = Vec::new();
                let mut tys = Vec::new();
                for item in items {
                    let h = self.synth_expr(ctx, item)?;
                    tys.push(h.ty);
                    checked.push(h);
                }
                let ty = ctx.store.intern(Type::Tuple(tys));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::TupleLit(checked),
                })
            }
            ExprKind::ListLit(items) => {
                if items.is_empty() {
                    return Err(Diagnostic::new(
                        "E1037",
                        "an empty list literal needs an expected type",
                        expr.span,
                    ));
                }
                let elems: Vec<&ast::Expr> = items.iter().collect();
                let (checked, elem) = self.synth_join_elems(ctx, &elems)?;
                let ty = ctx.store.intern(Type::List(elem));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::ListLit(checked),
                })
            }
            ExprKind::MapLit(entries) => {
                if entries.is_empty() {
                    return Err(Diagnostic::new(
                        "E1037",
                        "an empty map literal needs an expected type",
                        expr.span,
                    ));
                }
                let mut keys = Vec::new();
                let mut key_ty: Option<TypeId> = None;
                for (key, _) in entries {
                    let k = self.synth_expr(ctx, key)?;
                    check_key_type(ctx, k.ty, key.span)?;
                    key_ty = Some(match key_ty {
                        None => k.ty,
                        Some(prev) => ctx
                            .store
                            .join(prev, k.ty)
                            .ok_or_else(|| self.mismatch(ctx, prev, k.ty, key.span))?,
                    });
                    keys.push(k);
                }
                let values: Vec<&ast::Expr> = entries.iter().map(|(_, v)| v).collect();
                let (checked_values, v) = self.synth_join_elems(ctx, &values)?;
                let k = key_ty.expect("the literal has entries");
                let checked: Vec<(HExpr, HExpr)> = keys.into_iter().zip(checked_values).collect();
                let ty = ctx.store.intern(Type::Map(k, v));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::MapLit(checked),
                })
            }
            ExprKind::Closure {
                params,
                ret,
                row,
                body,
            } => self.check_closure(ctx, params, ret, row, None, body, expr.span),
            ExprKind::If { arms, else_body } => {
                self.check_if(ctx, arms, else_body, None, expr.span)
            }
            ExprKind::Case { scrut, arms } => self.check_case(ctx, scrut, arms, None, expr.span),
            ExprKind::Select { arms } => self.check_select(ctx, arms, None, expr.span),
            ExprKind::Labeled { label, .. } => Err(Diagnostic::new(
                "E1006",
                format!(
                    "the argument label `{label}:` is not valid here; a label \
                     names a declared parameter of the called function"
                ),
                expr.span,
            )),
        }
    }

    /// Lower `select` to one wait and one case expression.
    fn check_select(
        &mut self,
        ctx: &mut Ctx,
        arms: &[ast::SelectArm],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let Some((first, rest)) = arms.split_first() else {
            return Err(Diagnostic::new("E1062", "a select needs two arms", span));
        };
        let mut combined = first.wait.clone();
        for arm in rest {
            combined = ast::Expr {
                kind: ExprKind::MethodCall {
                    recv: Box::new(combined),
                    name: "choose".to_string(),
                    name_span: arm.wait.span,
                    type_args: Vec::new(),
                    args: vec![arm.wait.clone()],
                },
                span: first.wait.span.to(arm.wait.span),
            };
        }
        let waited = ast::Expr {
            kind: ExprKind::MethodCall {
                recv: Box::new(combined),
                name: "wait".to_string(),
                name_span: span,
                type_args: Vec::new(),
                args: Vec::new(),
            },
            span,
        };
        let count = arms.len();
        let case_arms = arms
            .iter()
            .enumerate()
            .map(|(index, arm)| ast::CaseArm {
                pattern: select_pattern(arm, index, count),
                body: arm.body.clone(),
                span: arm.span,
            })
            .collect::<Vec<_>>();
        match expected {
            Some(ty) => self.check_case(ctx, &waited, &case_arms, Some(ty), span),
            None => self.check_case(ctx, &waited, &case_arms, None, span),
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

    /// Try to resolve a bare name as a zero-field enum constructor.
    fn try_ctor_name(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Option<HExpr>, Diagnostic> {
        let Some(arm) = self.resolve_ctor(ctx, None, name, expected, span, false)? else {
            return Ok(None);
        };
        Ok(Some(self.construct_arm(
            ctx,
            arm,
            &[],
            &[],
            expected,
            span,
        )?))
    }

    /// Resolve a constructor name to an arm class. `required` selects
    /// whether an unknown name is an error or `None`.
    fn resolve_ctor(
        &mut self,
        ctx: &mut Ctx,
        qualifier: Option<&str>,
        name: &str,
        expected: Option<TypeId>,
        span: Span,
        required: bool,
    ) -> Result<Option<u32>, Diagnostic> {
        if let Some(q) = qualifier {
            let Some(family) = ctx.lookup_type(q, &self.env) else {
                return Err(Diagnostic::new(
                    "E1013",
                    format!("unknown type name `{q}`"),
                    span,
                ));
            };
            if ctx.classes[family as usize].kind != ClassKind::EnumParent {
                return Err(Diagnostic::new(
                    "E1040",
                    format!("`{q}` is not an enum"),
                    span,
                ));
            }
            let Some(arm) = ctx.find_arm(family, name) else {
                return Err(Diagnostic::new(
                    "E1041",
                    format!("the enum `{q}` has no arm named `{name}`"),
                    span,
                ));
            };
            return Ok(Some(arm));
        }
        // The expected type selects the family when it names one.
        if let Some(expected) = expected {
            if let Some((class, _)) = class_of(ctx, expected) {
                if let Some(family) = ctx.family_of(class) {
                    if let Some(arm) = ctx.find_arm(family, name) {
                        return Ok(Some(arm));
                    }
                }
            }
        }
        let families = ctx.ctor_families(&self.env, name);
        match families.len() {
            0 => {
                if required {
                    Err(Diagnostic::new(
                        "E1005",
                        format!("cannot find `{name}` in this scope"),
                        span,
                    ))
                } else {
                    Ok(None)
                }
            }
            1 => Ok(ctx.find_arm(families[0], name)),
            _ => Err(Diagnostic::new(
                "E1045",
                format!(
                    "the constructor `{name}` is ambiguous; use a qualified name \
                     such as `{}.{name}`",
                    ctx.classes[families[0] as usize].name
                ),
                span,
            )),
        }
    }

    /// Construct one enum arm with inference over the family
    /// parameters.
    fn construct_arm(
        &mut self,
        ctx: &mut Ctx,
        arm: u32,
        explicit: &[ast::TypeExpr],
        args: &[ast::Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let info = &ctx.classes[arm as usize];
        let type_names = info.type_params.clone();
        let field_tys = info.field_tys.clone();
        let field_names = info.field_names.clone();
        let ret = info.self_ty;
        let short = info.arm_short.clone();
        let muts = vec![false; field_tys.len()];
        let out = self.check_poly_call(
            ctx,
            &short,
            span,
            &type_names,
            0,
            vec![None; type_names.len()],
            0,
            explicit,
            &field_tys,
            &muts,
            &field_names,
            ret,
            &[],
            args,
            expected,
        )?;
        Ok(HExpr {
            ty: out.ret,
            mutable: true,
            kind: HExprKind::Construct {
                class: arm,
                targs: out.targs,
                args: out.args,
            },
        })
    }

    /// Check a call of a plain name: a local closure, a top-level
    /// function, a class constructor, an enum constructor, or a
    /// native builder constructor.
    #[allow(clippy::too_many_arguments)]
    fn call_named(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if self.env.core_scope && name == "intrinsic" {
            return self.call_intrinsic(ctx, type_args, args, span);
        }
        let callee = self.resolve_callee(ctx, name, name_span, type_args, expected, span)?;
        match callee {
            Callee::Value(callee_h) => {
                if !type_args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1024",
                        "a closure value does not take type arguments",
                        name_span,
                    ));
                }
                self.synth_call_value(ctx, callee_h, args, name_span, span)
            }
            Callee::Func(func) => {
                let sig = ctx.sigs[func as usize].clone();
                let out = self.check_poly_call(
                    ctx,
                    name,
                    span,
                    &sig.type_params.clone(),
                    sig.effect_params.len(),
                    vec![None; sig.type_params.len()],
                    0,
                    type_args,
                    &sig.params,
                    &sig.param_muts,
                    &sig.param_names,
                    sig.ret,
                    &sig.row,
                    args,
                    expected,
                )?;
                Ok(HExpr {
                    ty: out.ret,
                    mutable: true,
                    kind: HExprKind::Call {
                        func,
                        targs: out.targs,
                        rowargs: out.rowargs,
                        args: out.args,
                    },
                })
            }
            Callee::Class(class) => {
                if ctx.classes[class as usize].native_repr == Some(NativeRepr::Bytes)
                    && args.len() == 1
                {
                    if !type_args.is_empty() {
                        return Err(Diagnostic::new(
                            "E1024",
                            "`Bytes` does not take type arguments",
                            name_span,
                        ));
                    }
                    let text = self.check_expr(ctx, &args[0], STRING)?;
                    return Ok(HExpr {
                        ty: lm_types::BYTES,
                        mutable: false,
                        kind: HExprKind::Native {
                            op: NativeOp::BytesNew,
                            args: vec![text],
                        },
                    });
                }
                let info = &ctx.classes[class as usize];
                let type_names = info.type_params.clone();
                let ret = info.self_ty;
                let (params, muts, names, row) = match &info.init {
                    Some(init) => (
                        init.params.clone(),
                        init.param_muts.clone(),
                        init.param_names.clone(),
                        init.row.clone(),
                    ),
                    None => (vec![], vec![], vec![], vec![]),
                };
                let out = self.check_poly_call(
                    ctx,
                    name,
                    span,
                    &type_names,
                    0,
                    vec![None; type_names.len()],
                    0,
                    type_args,
                    &params,
                    &muts,
                    &names,
                    ret,
                    &row,
                    args,
                    expected,
                )?;
                Ok(HExpr {
                    ty: out.ret,
                    mutable: true,
                    kind: HExprKind::Construct {
                        class,
                        targs: out.targs,
                        args: out.args,
                    },
                })
            }
            Callee::Ctor { arm } => self.construct_arm(ctx, arm, type_args, args, expected, span),
            Callee::ListCtor(elem) => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        "`List[T]()` expects 0 argument(s); use a list literal",
                        span,
                    ));
                }
                let ty = ctx.store.intern(Type::List(elem));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::ListLit(vec![]),
                })
            }
            Callee::MapCtor(k, v) => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        "`Map[K, V]()` expects 0 argument(s); use a map literal",
                        span,
                    ));
                }
                let ty = ctx.store.intern(Type::Map(k, v));
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::MapLit(vec![]),
                })
            }
            Callee::SysMember { group, member } => {
                // A `use`-bound callable member: the same operation
                // call rule as the qualified `sys` path. The alias
                // grants nothing and the row charge is identical.
                self.check_sys_call(ctx, group, &member, name_span, type_args, args, span)
            }
            Callee::SysGroup(group) => Err(Diagnostic::new(
                "E1051",
                format!(
                    "`{name}` is the `sys.{}` group object and not callable; \
                     name an operation such as `{name}.<operation>`",
                    group.to_ascii_lowercase()
                ),
                name_span,
            )),
        }
    }

    /// Check one named intrinsic inside the core image.
    fn call_intrinsic(
        &mut self,
        ctx: &mut Ctx,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "an intrinsic does not take type arguments",
                span,
            ));
        }
        let Some((name_expr, operands)) = args.split_first() else {
            return Err(Diagnostic::new(
                "E1006",
                "`intrinsic` needs a manifest name",
                span,
            ));
        };
        let ExprKind::Str(name) = &name_expr.kind else {
            return Err(Diagnostic::new(
                "E1006",
                "the intrinsic name must be a string literal",
                name_expr.span,
            ));
        };
        let intrinsic = lm_abi::intrinsic_by_name(name).ok_or_else(|| {
            Diagnostic::new(
                "E1026",
                format!("the intrinsic manifest has no `{name}` entry"),
                name_expr.span,
            )
        })?;
        let def = *lm_abi::intrinsic(intrinsic);
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|param| Self::abi_type_id(ctx, *param))
            .collect();
        let checked = self.check_args_simple(
            ctx,
            operands,
            &params,
            &vec![false; params.len()],
            NO_NAMES,
            def.name,
            span,
        )?;
        Ok(HExpr {
            ty: Self::abi_type_id(ctx, def.reply),
            mutable: true,
            kind: HExprKind::Intrinsic {
                intrinsic,
                args: checked,
            },
        })
    }

    /// Resolve one called name to its meaning.
    fn resolve_callee(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<Callee, Diagnostic> {
        if let Some(res) = self.resolve_name(name)? {
            let (ty, kind) = match res {
                NameRes::Local(slot, ty, _) => (ty, HExprKind::Local(slot)),
                NameRes::Capture(idx, ty, _) => (ty, HExprKind::Capture(idx)),
            };
            return Ok(Callee::Value(HExpr {
                ty,
                mutable: true,
                kind,
            }));
        }
        if let Some(func) = self.module_func(ctx, name) {
            return Ok(Callee::Func(func));
        }
        if let Some(class) = ctx.lookup_type(name, &self.env) {
            match ctx.classes[class as usize].kind {
                ClassKind::Normal => return Ok(Callee::Class(class)),
                ClassKind::EnumParent => {
                    return Err(Diagnostic::new(
                        "E1040",
                        format!(
                            "the enum `{name}` cannot be constructed; use one of \
                             its arms"
                        ),
                        name_span,
                    ));
                }
                ClassKind::EnumCase => {}
            }
        }
        match name {
            "List" => {
                if type_args.len() == 1 {
                    let elem = resolve_type(ctx, &self.env.clone(), &type_args[0])?;
                    return Ok(Callee::ListCtor(elem));
                }
                return Err(Diagnostic::new(
                    "E1024",
                    "`List` needs 1 explicit type argument here, for example \
                     `List[Int]()`",
                    name_span,
                ));
            }
            "Map" => {
                if type_args.len() == 2 {
                    let env = self.env.clone();
                    let k = resolve_type(ctx, &env, &type_args[0])?;
                    check_key_type(ctx, k, type_args[0].span)?;
                    let v = resolve_type(ctx, &env, &type_args[1])?;
                    return Ok(Callee::MapCtor(k, v));
                }
                return Err(Diagnostic::new(
                    "E1024",
                    "`Map` needs 2 explicit type arguments here, for example \
                     `Map[String, Int]()`",
                    name_span,
                ));
            }
            _ => {}
        }
        if let Some(arm) = self.resolve_ctor(ctx, None, name, expected, span, false)? {
            return Ok(Callee::Ctor { arm });
        }
        match self.use_binding(ctx, name)? {
            Some(UseBinding::SysMember { group, member }) => {
                return Ok(Callee::SysMember { group, member });
            }
            Some(UseBinding::SysGroup(group)) => {
                return Ok(Callee::SysGroup(group));
            }
            Some(UseBinding::Module(path)) => {
                return Err(Diagnostic::new(
                    "E1051",
                    format!(
                        "`{name}` names the module `{path}` and is not a value; \
                         call one of its definitions, for example \
                         `{name}.<definition>(...)`"
                    ),
                    name_span,
                ));
            }
            None => {}
        }
        Err(Diagnostic::new(
            "E1005",
            format!("cannot find a function named `{name}`"),
            name_span,
        ))
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
        // Calling an identity-indexed operation value performs the
        // operation. The row charge follows the identity in the type.
        if let Type::Op(op, fn_ty) = ctx.store.get(callee.ty).clone() {
            let (params, ret) = match ctx.store.get(fn_ty) {
                Type::Fn(params, _, ret, _) => (params.clone(), *ret),
                _ => unreachable!("an Op type embeds a function type"),
            };
            self.charge_op(ctx, op, span)?;
            let muts = vec![false; params.len()];
            let args =
                self.check_args_simple(ctx, args, &params, &muts, NO_NAMES, "operation", span)?;
            return Ok(HExpr {
                ty: ret,
                mutable: true,
                kind: HExprKind::CallValue {
                    callee: Box::new(callee),
                    args,
                },
            });
        }
        let (params, muts, ret, row) = match ctx.store.get(callee.ty) {
            Type::Fn(params, muts, ret, row) => (params.clone(), muts.clone(), *ret, row.clone()),
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
        self.charge_row(ctx, &row, span)?;
        // The function type carries the `mut` markers, so a call
        // through a value needs mutable capability at mut positions.
        let args = self.check_args_simple(ctx, args, &params, &muts, NO_NAMES, "closure", span)?;
        Ok(HExpr {
            ty: ret,
            mutable: true,
            kind: HExprKind::CallValue {
                callee: Box::new(callee),
                args,
            },
        })
    }

    /// Check arguments against concrete parameter types.
    ///
    /// `param_names` holds the declared names. A declaration with
    /// names accepts labels; an empty list accepts positional
    /// arguments only. Both `&[String]` and `&[&str]` work, so a
    /// native method states its names as literals.
    #[allow(clippy::too_many_arguments)]
    fn check_args_simple<N: AsRef<str>>(
        &mut self,
        ctx: &mut Ctx,
        args: &[ast::Expr],
        params: &[TypeId],
        param_muts: &[bool],
        param_names: &[N],
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
        let args = arrange_args(args, param_names, what)?;
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

    /// True when an argument can be synthesized without an expected
    /// type. This gates the inference pass; a wrong `true` degrades to
    /// an inner diagnostic that asks for explicit arguments.
    fn can_synth(&self, ctx: &Ctx, expr: &ast::Expr) -> bool {
        match &expr.kind {
            ExprKind::ListLit(items) => !items.is_empty(),
            ExprKind::MapLit(entries) => !entries.is_empty(),
            ExprKind::TupleLit(items) => items.iter().all(|i| self.can_synth(ctx, i)),
            ExprKind::Name(name) => {
                if self.lookup_slot(name).is_some() || self.module_func(ctx, name).is_some() {
                    return true;
                }
                let families = ctx.ctor_families(&self.env, name);
                if families.len() == 1 {
                    let arm = ctx
                        .find_arm(families[0], name)
                        .expect("candidate family has the arm");
                    let info = &ctx.classes[arm as usize];
                    return ctor_determined(ctx, &info.field_tys, info.type_params.len());
                }
                true
            }
            ExprKind::Call {
                name, type_args, ..
            } => {
                if !type_args.is_empty()
                    || self.lookup_slot(name).is_some()
                    || self.module_func(ctx, name).is_some()
                {
                    return true;
                }
                if let Some(class) = ctx.lookup_type(name, &self.env) {
                    let info = &ctx.classes[class as usize];
                    if info.kind == ClassKind::Normal {
                        let params = info
                            .init
                            .as_ref()
                            .map(|i| i.params.clone())
                            .unwrap_or_default();
                        return ctor_determined(ctx, &params, info.type_params.len());
                    }
                }
                let families = ctx.ctor_families(&self.env, name);
                if families.len() == 1 {
                    let arm = ctx
                        .find_arm(families[0], name)
                        .expect("candidate family has the arm");
                    let info = &ctx.classes[arm as usize];
                    return ctor_determined(ctx, &info.field_tys, info.type_params.len());
                }
                true
            }
            _ => true,
        }
    }

    /// Check one call with first-order generic inference.
    ///
    /// `pre_bound` pre-binds class type parameters from a receiver.
    /// `own_start` is the first position explicit arguments fill.
    #[allow(clippy::too_many_arguments)]
    fn check_poly_call(
        &mut self,
        ctx: &mut Ctx,
        what: &str,
        span: Span,
        type_names: &[String],
        effect_count: usize,
        pre_bound: Vec<Option<TypeId>>,
        own_start: usize,
        explicit: &[ast::TypeExpr],
        decl_params: &[TypeId],
        param_muts: &[bool],
        param_names: &[String],
        decl_ret: TypeId,
        decl_row: &[lm_types::RowElem],
        args: &[ast::Expr],
        expected: Option<TypeId>,
    ) -> Result<PolyOut, Diagnostic> {
        if args.len() != decl_params.len() {
            return Err(Diagnostic::new(
                "E1006",
                format!(
                    "`{what}` expects {} argument(s), found {}",
                    decl_params.len(),
                    args.len()
                ),
                span,
            ));
        }
        let args = arrange_args(args, param_names, what)?;
        let mut targs: Vec<Option<TypeId>> = pre_bound;
        debug_assert_eq!(targs.len(), type_names.len());
        let mut rowargs: Vec<Option<Row>> = vec![None; effect_count];
        if !explicit.is_empty() {
            let own = type_names.len() - own_start;
            if explicit.len() != own {
                return Err(Diagnostic::new(
                    "E1024",
                    format!(
                        "`{what}` takes {own} type argument(s), found {}",
                        explicit.len()
                    ),
                    span,
                ));
            }
            let env = self.env.clone();
            for (i, texpr) in explicit.iter().enumerate() {
                targs[own_start + i] = Some(resolve_type(ctx, &env, texpr)?);
            }
        }
        // Inference from the expected result.
        if let Some(expected) = expected {
            unify(ctx, decl_ret, expected, &mut targs, &mut rowargs, false);
        }
        // Pass A: synthesize the arguments whose declared parameter
        // still contains an unresolved type or effect variable, so an
        // explicitly rowed function argument binds the effect
        // variable of a higher-order callee.
        let mut pre: Vec<Option<HExpr>> = Vec::with_capacity(args.len());
        for (arg, decl) in args.iter().copied().zip(decl_params.iter()) {
            let part = self.partial_substitute(ctx, *decl, &targs, &rowargs);
            if (ctx.store.contains_var(part) || ctx.store.contains_effect_var(part))
                && self.can_synth(ctx, arg)
            {
                let h = self.synth_expr(ctx, arg)?;
                unify(ctx, part, h.ty, &mut targs, &mut rowargs, true);
                pre.push(Some(h));
            } else {
                pre.push(None);
            }
        }
        // Every remaining type parameter is ambiguous.
        for (i, slot) in targs.iter().enumerate() {
            if slot.is_none() {
                return Err(Diagnostic::new(
                    "E1045",
                    format!(
                        "cannot infer the type argument `{}` of `{what}`; \
                         give explicit type arguments",
                        type_names[i]
                    ),
                    span,
                ));
            }
        }
        let targs: Vec<TypeId> = targs.into_iter().map(|t| t.expect("bound")).collect();
        let rowargs: Vec<Row> = rowargs.into_iter().map(|r| r.unwrap_or_default()).collect();
        // Pass B: check every argument against its substituted type.
        let mut checked = Vec::with_capacity(args.len());
        for ((arg, decl), (h, is_mut)) in args
            .iter()
            .copied()
            .zip(decl_params.iter())
            .zip(pre.into_iter().zip(param_muts.iter()))
        {
            let want = ctx.store.substitute(*decl, &targs, &rowargs);
            let h = match h {
                Some(h) => {
                    if !ctx.store.compatible(want, h.ty) {
                        return Err(self.mismatch(ctx, want, h.ty, arg.span));
                    }
                    h
                }
                None => self.check_expr(ctx, arg, want)?,
            };
            if *is_mut && !h.mutable {
                return Err(Diagnostic::new(
                    "E1035",
                    "a `mut` parameter needs a mutable value",
                    arg.span,
                ));
            }
            checked.push(h);
        }
        let ret = ctx.store.substitute(decl_ret, &targs, &rowargs);
        let row = ctx.store.substitute_row(decl_row, &rowargs);
        self.charge_row(ctx, &row, span)?;
        Ok(PolyOut {
            targs,
            rowargs,
            args: checked,
            ret,
        })
    }

    /// Substitute the type parameters that are bound so far and keep
    /// the unresolved ones as variables.
    fn partial_substitute(
        &self,
        ctx: &mut Ctx,
        ty: TypeId,
        targs: &[Option<TypeId>],
        rowargs: &[Option<Row>],
    ) -> TypeId {
        let filled: Vec<TypeId> = targs
            .iter()
            .enumerate()
            .map(|(i, t)| t.unwrap_or_else(|| ctx.store.intern(Type::Var(i as u32))))
            .collect();
        let rows: Vec<Row> = rowargs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                r.clone()
                    .unwrap_or_else(|| vec![lm_types::RowElem::Var(i as u32)])
            })
            .collect();
        ctx.store.substitute(ty, &filled, &rows)
    }

    /// Try to read a receiver expression as an enum qualifier, for
    /// example the `Option` in `Option.Some`.
    fn enum_qualifier(&self, ctx: &Ctx, recv: &ast::Expr) -> Option<u32> {
        if let ExprKind::Name(name) = &recv.kind {
            if self.lookup_slot(name).is_none() && self.module_func(ctx, name).is_none() {
                if let Some(class) = ctx.lookup_type(name, &self.env) {
                    if ctx.classes[class as usize].kind == ClassKind::EnumParent {
                        return Some(class);
                    }
                }
            }
        }
        None
    }

    fn synth_field(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        name: &str,
        name_span: Span,
        expected: Option<TypeId>,
    ) -> Result<HExpr, Diagnostic> {
        // A canonical qualified constructor such as `Option.None`.
        if let Some(family) = self.enum_qualifier(ctx, recv) {
            let qualifier = ctx.classes[family as usize].name.clone();
            let arm = self
                .resolve_ctor(ctx, Some(&qualifier), name, expected, name_span, true)?
                .expect("qualified constructors resolve or fail");
            return self.construct_arm(ctx, arm, &[], &[], expected, name_span);
        }
        // A first-class operation value `sys.<group>.<Member>`.
        if let Some(group) = self.sys_group_of(ctx, recv)? {
            return self.check_sys_value(ctx, group, name, name_span);
        }
        // A bare `sys.<group>` is not a value.
        if matches!(recv.kind, ExprKind::Name(ref n) if n == "sys") && self.sys_in_scope()? {
            if Self::sys_group(name).is_some() {
                return Err(Diagnostic::new(
                    "E1051",
                    format!(
                        "`sys.{name}` is not a value; name an operation such as \
                         `sys.{name}.<operation>`"
                    ),
                    name_span,
                ));
            }
            return Err(Diagnostic::new(
                "E1051",
                format!("`sys` has no group named `{name}`"),
                name_span,
            ));
        }
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
            let self_expr = self.self_value();
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
        let Some((class, class_args)) = class_of(ctx, recv_h.ty) else {
            return Err(Diagnostic::new(
                "E1027",
                format!("the type {} has no fields", ctx.store.display(recv_h.ty)),
                recv.span,
            ));
        };
        let fidx = ctx
            .find_field(class, name)
            .ok_or_else(|| unknown_field(ctx, class, name, name_span))?;
        let declared = ctx.classes[class as usize].field_tys[fidx];
        let ty = ctx.store.substitute(declared, &class_args, &[]);
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

    #[allow(clippy::too_many_arguments)]
    fn synth_method_call(
        &mut self,
        ctx: &mut Ctx,
        recv: &ast::Expr,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        // A canonical qualified constructor call such as
        // `Option.Some(1)`.
        if let Some(family) = self.enum_qualifier(ctx, recv) {
            let qualifier = ctx.classes[family as usize].name.clone();
            let arm = self
                .resolve_ctor(ctx, Some(&qualifier), name, expected, name_span, true)?
                .expect("qualified constructors resolve or fail");
            return self.construct_arm(ctx, arm, type_args, args, expected, span);
        }
        // A direct operation call `sys.<group>.<Member>(args)`.
        if let Some(group) = self.sys_group_of(ctx, recv)? {
            return self.check_sys_call(ctx, group, name, name_span, type_args, args, span);
        }
        // `Class.spawn(args...)`, the sugar of specification 18.3.
        if name == "spawn" {
            if let ExprKind::Name(class_name) = &recv.kind {
                if let Some(class) = ctx.lookup_type(class_name, &self.env) {
                    if !type_args.is_empty() {
                        return Err(Diagnostic::new(
                            "E1024",
                            "`spawn` does not take type arguments",
                            name_span,
                        ));
                    }
                    return self.check_spawn(ctx, class, args, name_span, span);
                }
            }
        }
        // A call into a `use`-bound module: `matrix.det(x)` or the
        // constructor `matrix.Matrix(2, 3)`. The materialized import
        // carries the qualified key, so the ordinary call path
        // resolves it.
        if let ExprKind::Name(alias) = &recv.kind {
            if let Some(UseBinding::Module(path)) = self.use_binding(ctx, alias)? {
                let qualified = format!("{alias}.{name}");
                if !ctx.func_index.contains_key(&qualified)
                    && ctx.lookup_type(&qualified, &self.env).is_none()
                {
                    return Err(Diagnostic::new(
                        "E1005",
                        format!("the module `{path}` exports no `{name}`"),
                        name_span,
                    ));
                }
                return self
                    .call_named(ctx, &qualified, name_span, type_args, args, expected, span);
            }
        }
        // `Fault.denied(reason)`: the one fault a program can build.
        // The receiver is the type name, and no value carries it.
        if let ExprKind::Name(type_name) = &recv.kind {
            if type_name == "Fault"
                && self.lookup_slot(type_name).is_none()
                && ctx.lookup_type(type_name, &self.env).is_none()
            {
                return self.check_fault_denied(ctx, name, name_span, type_args, args, span);
            }
        }
        let recv_h = self.synth_expr(ctx, recv)?;
        let recv_ty = recv_h.ty;
        // Native control methods on the VM surface types.
        if matches!(
            ctx.store.get(recv_ty),
            Type::EmptyVm
                | Type::Vm(_)
                | Type::Wait(_)
                | Type::PolicyTable
                | Type::Request
                | Type::PendingCall(_, _)
                | Type::Handle(_, _)
                | Type::ResourceHandle
                | Type::Fault
        ) {
            let out =
                self.check_control_method(ctx, recv_h, name, name_span, type_args, args, span)?;
            return Ok(out.expect("control receivers resolve or fail"));
        }
        // Class and enum methods first, then the universal `freeze`.
        if let Some((class, class_args)) = class_of(ctx, recv_ty) {
            if let Some(found) = ctx.find_method_owner(class, name) {
                return self.check_declared_method(
                    ctx, recv_h, class, class_args, found, name, name_span, type_args, args,
                    expected, span,
                );
            }
            if name == "freeze" && args.is_empty() && type_args.is_empty() {
                return Ok(freeze_expr(recv_h));
            }
            if name == "digest" && args.is_empty() && type_args.is_empty() {
                return Ok(digest_expr(recv_h));
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
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "a native method does not take type arguments",
                name_span,
            ));
        }
        self.check_native_method(ctx, recv_h, recv_ty, name, name_span, args, span)
    }

    /// Check a call of one method the class declares.
    ///
    /// `check_method_call` reaches it by name. The operator sugar of
    /// specification 6.4 reaches it with a hook name such as
    /// `__add__`, so both spellings take one path: the same dispatch
    /// rule, the same effect charge, and the same generic binding.
    #[allow(clippy::too_many_arguments)]
    fn check_declared_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        class: u32,
        class_args: Vec<TypeId>,
        found: (MethodSig, Vec<TypeId>, u32),
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let (sig, owner_args, owner) = found;
        {
            {
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
                let class_names = ctx.classes[class as usize].type_params.clone();
                let mut type_names = class_names.clone();
                type_names.extend(sig.own_type_params.iter().cloned());
                let mut pre_bound: Vec<Option<TypeId>> =
                    class_args.iter().map(|a| Some(*a)).collect();
                pre_bound.extend(vec![None; sig.own_type_params.len()]);
                let own_start = class_names.len();
                let out = self.check_poly_call(
                    ctx,
                    name,
                    span,
                    &type_names,
                    sig.own_effect_params.len(),
                    pre_bound,
                    own_start,
                    type_args,
                    &sig.params,
                    &sig.param_muts,
                    &sig.param_names,
                    sig.ret,
                    &sig.row,
                    args,
                    expected,
                )?;
                let own_targs = out.targs[own_start..].to_vec();
                if ctx.classes[class as usize].is_final {
                    let mut direct_targs = if owner == class {
                        out.targs[..own_start].to_vec()
                    } else {
                        owner_args
                            .iter()
                            .map(|arg| ctx.store.substitute(*arg, &out.targs, &[]))
                            .collect()
                    };
                    direct_targs.extend(own_targs);
                    let mut all_args = vec![recv_h];
                    all_args.extend(out.args);
                    return Ok(HExpr {
                        ty: out.ret,
                        mutable: true,
                        kind: HExprKind::Call {
                            func: sig.func,
                            targs: direct_targs,
                            rowargs: out.rowargs,
                            args: all_args,
                        },
                    });
                }
                Ok(HExpr {
                    ty: out.ret,
                    mutable: true,
                    kind: HExprKind::MethodCall {
                        recv: Box::new(recv_h),
                        selector: name.to_string(),
                        generic_owner: !owner_args.is_empty(),
                        own_targs,
                        own_rowargs: out.rowargs,
                        args: out.args,
                    },
                })
            }
        }
    }

    /// Check a call of one native method: a collection, a builder, or
    /// a file handle. The receiver carries no declared class.
    #[allow(clippy::too_many_arguments)]
    fn check_native_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: TypeId,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        // Native methods on collections and builders.
        let store_ty = ctx.store.get(recv_ty).clone();
        if store_ty == Type::FileHandle {
            // The operation manifest carries no parameter names, so
            // the method surface names them here. The receiver is the
            // first manifest parameter, and the list skips it.
            let (op, names) = match name {
                "read" => (lm_abi::OP_FS_READ, &["max_bytes"][..]),
                "write" => (lm_abi::OP_FS_WRITE, &["bytes"][..]),
                "seek" => (lm_abi::OP_FS_SEEK, &["from"][..]),
                "flush" => (lm_abi::OP_FS_FLUSH, NO_NAMES),
                "close" => (lm_abi::OP_FS_CLOSE, NO_NAMES),
                _ => {
                    return Err(Diagnostic::new(
                        "E1026",
                        format!("the type FileHandle has no method named `{name}`"),
                        name_span,
                    ))
                }
            };
            let def = lm_abi::op(op);
            let params: Vec<TypeId> = def
                .params
                .iter()
                .skip(1)
                .map(|param| Self::abi_type_id(ctx, *param))
                .collect();
            let muts = vec![false; params.len()];
            let checked = self.check_args_simple(ctx, args, &params, &muts, names, name, span)?;
            self.charge_op(ctx, op, span)?;
            let mut all_args = vec![recv_h];
            all_args.extend(checked);
            return Ok(HExpr {
                ty: Self::abi_type_id(ctx, def.reply),
                mutable: true,
                kind: HExprKind::Perform { op, args: all_args },
            });
        }
        // Each entry states its parameter names beside its parameter
        // types. The names come from the core method tables of
        // specification 24.4 and 24.5, so a label matches the
        // published signature.
        let native = |op: NativeOp,
                      params: Vec<TypeId>,
                      names: &'static [&'static str],
                      ret: TypeId,
                      needs_mut: bool| { (op, params, names, ret, needs_mut) };
        let (op, params, names, ret, needs_mut) = match (&store_ty, name) {
            (Type::List(_), "len") => native(NativeOp::ListLen, vec![], NO_NAMES, INT, false),
            (Type::List(e), "at") => native(NativeOp::ListAt, vec![INT], &["index"], *e, false),
            (Type::List(e), "push") => native(NativeOp::ListPush, vec![*e], &["value"], UNIT, true),
            (Type::List(e), "get") => {
                let ret = ctx.option_of(*e);
                native(NativeOp::ListGet, vec![INT], &["index"], ret, false)
            }
            (Type::Map(_, _), "len") => native(NativeOp::MapLen, vec![], NO_NAMES, INT, false),
            (Type::Map(k, _), "has") => native(NativeOp::MapHas, vec![*k], &["key"], BOOL, false),
            (Type::Map(k, v), "at") => native(NativeOp::MapAt, vec![*k], &["key"], *v, false),
            (Type::Map(k, v), "put") => native(
                NativeOp::MapPut,
                vec![*k, *v],
                &["key", "value"],
                UNIT,
                true,
            ),
            (Type::Map(k, v), "get") => {
                let ret = ctx.option_of(*v);
                native(NativeOp::MapGet, vec![*k], &["key"], ret, false)
            }
            _ if name == "freeze" && ctx.store.is_heap(recv_ty) && args.is_empty() => {
                return Ok(freeze_expr(recv_h));
            }
            _ if name == "digest" && ctx.store.is_heap(recv_ty) && args.is_empty() => {
                return Ok(digest_expr(recv_h));
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
        let checked = self.check_args_simple(ctx, args, &params, &muts, names, name, span)?;
        all_args.extend(checked);
        // Element reads keep the receiver capability.
        let mutable = match op {
            NativeOp::ListAt | NativeOp::MapAt | NativeOp::ListGet | NativeOp::MapGet => {
                all_args[0].mutable
            }
            _ => true,
        };
        Ok(HExpr {
            ty: ret,
            mutable,
            kind: HExprKind::Native { op, args: all_args },
        })
    }

    /// Check `Fault.denied(reason)`.
    ///
    /// A holder denies one request with `reject`, and `reject` needs
    /// a `Fault`. No other expression builds one, so a holder could
    /// not deny a request whose reply type carries no error arm.
    ///
    /// The code is always `PolicyDenied`. A machine-internal code
    /// such as `OutOfFuel` states something about the runtime, and a
    /// program must not claim it. The value is pure: it needs no
    /// authority, because only `reject` installs it, and `reject`
    /// charges `Vm`.
    fn check_fault_denied(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if name != "denied" {
            return Err(Diagnostic::new(
                "E1026",
                format!("the type Fault has no constructor named `{name}`"),
                name_span,
            ));
        }
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "`Fault.denied` does not take type arguments",
                name_span,
            ));
        }
        if args.len() != 1 {
            return Err(Diagnostic::new(
                "E1006",
                format!("`denied` expects 1 argument(s), found {}", args.len()),
                span,
            ));
        }
        let args = arrange_args(args, &["reason"], "denied")?;
        let reason = self.check_expr(ctx, args[0], STRING)?;
        Ok(HExpr {
            ty: lm_types::FAULT,
            mutable: true,
            kind: HExprKind::FaultDenied {
                reason: Box::new(reason),
            },
        })
    }

    /// Check `Class.spawn(args...)`.
    ///
    /// The rule of specification 18.3: the class must inherit
    /// `Proc[M]` and declare a valid `on_spawn`. The result type is
    /// `Handle[M, R]`, where `R` is the result of `on_spawn`.
    fn check_spawn(
        &mut self,
        ctx: &mut Ctx,
        class: u32,
        args: &[ast::Expr],
        name_span: Span,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let class_name = ctx.classes[class as usize].name.clone();
        let proc = *ctx.core_types.get("Proc").expect("the core declares Proc");
        let mailbox = ctx
            .store
            .ancestor_args(lm_types::ClassId(class), &[], lm_types::ClassId(proc))
            .and_then(|args| args.first().copied())
            .ok_or_else(|| {
                Diagnostic::new(
                    "E1026",
                    format!("`spawn` needs a subclass of `Proc`; `{class_name}` is not one"),
                    name_span,
                )
            })?;
        let (body, body_owner_args, body_owner) =
            ctx.find_method_owner(class, "on_spawn").ok_or_else(|| {
                Diagnostic::new(
                    "E1026",
                    format!("the proc class `{class_name}` declares no `on_spawn`"),
                    name_span,
                )
            })?;
        if !body.params.is_empty()
            || !body.own_type_params.is_empty()
            || !body.own_effect_params.is_empty()
            || !body_owner_args.is_empty()
        {
            return Err(Diagnostic::new(
                "E1026",
                format!("`{class_name}.on_spawn` must take `self` only and declare no generics"),
                name_span,
            ));
        }
        let declared_row = body.row.clone();
        // The constructor signature of the proc class.
        let info = &ctx.classes[class as usize];
        let (params, muts, names) = match &info.init {
            Some(init) => (
                init.params.clone(),
                init.param_muts.clone(),
                init.param_names.clone(),
            ),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        let checked = self.check_args_simple(ctx, args, &params, &muts, &names, "spawn", span)?;
        // The spawner charges `Proc.Spawn` and the declared row of
        // `on_spawn`. The birth grant gives the child that same row, so
        // the spawner passes only authority it already holds.
        self.charge_op(ctx, lm_abi::OP_PROC_SPAWN, span)?;
        self.charge_row(ctx, &declared_row, span)?;
        if !checked.is_empty() {
            // Lowering reads the interned tuple type of the arguments.
            let elems: Vec<TypeId> = checked.iter().map(|a| a.ty).collect();
            ctx.store.intern(Type::Tuple(elems));
        }
        // The verifier reads a closure type out of the module type
        // table, so both function types enter the checker store here
        // and the lowering pass copies them across.
        let self_ty = ctx.store.intern(Type::Class(lm_types::ClassId(class)));
        let ctor_row = ctx.classes[class as usize]
            .init
            .as_ref()
            .map(|init| init.row.clone())
            .unwrap_or_default();
        let ctor_ty = ctx.store.intern_fn(params, muts, self_ty, ctor_row);
        // `on_spawn` may come from an ancestor. The body function then
        // declares that ancestor as its receiver, and the constructed
        // instance is a subclass of it.
        let body_self = ctx.store.intern(Type::Class(lm_types::ClassId(body_owner)));
        let body_ty = ctx.store.intern_fn(
            vec![body_self],
            vec![body.mut_self],
            body.ret,
            body.row.clone(),
        );
        let ty = ctx.store.intern(Type::Handle(mailbox, body.ret));
        Ok(HExpr {
            ty,
            mutable: true,
            kind: HExprKind::Spawn {
                class,
                body: body.func,
                ctor_ty,
                body_ty,
                args: checked,
            },
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
        // A generic parent contributes its type arguments to every
        // signature the subclass inherits.
        let parent_args = ctx
            .store
            .class_meta(lm_types::ClassId(cidx))
            .parent_args
            .clone();
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
            let init_params: Vec<TypeId> = parent_init
                .params
                .iter()
                .map(|p| ctx.store.substitute(*p, &parent_args, &[]))
                .collect();
            self.charge_row(ctx, &parent_init.row, span)?;
            let checked = self.check_args_simple(
                ctx,
                args,
                &init_params,
                &parent_init.param_muts,
                &parent_init.param_names,
                "super.init",
                span,
            )?;
            let c = self.ctor.as_mut().expect("ctor");
            let parent_len = ctx.classes[parent as usize].field_tys.len();
            for i in 0..parent_len {
                c.state.inited[i] = true;
            }
            c.state.super_done = true;
            let mut all_args = vec![self.self_value()];
            all_args.extend(checked);
            return Ok(HExpr {
                ty: UNIT,
                mutable: true,
                kind: HExprKind::Call {
                    func: parent_init.func,
                    targs: parent_args,
                    rowargs: vec![],
                    args: all_args,
                },
            });
        }
        // The superclass method is read in the subclass view.
        let arity = ctx.classes[cidx as usize].type_params.len();
        let (sig, owner_args, _) = ctx
            .lookup_method(parent, parent_args, arity, name)
            .ok_or_else(|| {
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
        let out = self.check_poly_call(
            ctx,
            name,
            span,
            &sig.own_type_params.clone(),
            sig.own_effect_params.len(),
            vec![None; sig.own_type_params.len()],
            0,
            &[],
            &sig.params,
            &sig.param_muts,
            &sig.param_names,
            sig.ret,
            &sig.row,
            args,
            None,
        )?;
        let mut all_args = vec![self_expr];
        all_args.extend(out.args);
        // The callee reads its class parameters first, so the owner
        // arguments come before the method's own arguments.
        let mut targs = owner_args;
        targs.extend(out.targs);
        Ok(HExpr {
            ty: out.ret,
            mutable: true,
            kind: HExprKind::Call {
                func: sig.func,
                targs,
                rowargs: out.rowargs,
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
            Type::Tuple(elems) => {
                let ExprKind::Int(pos) = index.kind else {
                    return Err(Diagnostic::new(
                        "E1048",
                        "a tuple index must be an integer literal",
                        index.span,
                    ));
                };
                if pos < 0 || pos as usize >= elems.len() {
                    return Err(Diagnostic::new(
                        "E1048",
                        format!(
                            "the tuple has {} element(s); the index {pos} is out \
                             of range",
                            elems.len()
                        ),
                        index.span,
                    ));
                }
                let mutable = recv_h.mutable;
                Ok(HExpr {
                    ty: elems[pos as usize],
                    mutable,
                    kind: HExprKind::TupleGet {
                        tuple: Box::new(recv_h),
                        index: pos as u32,
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

    /// Check `value is Type` and `value as Type`.
    fn synth_is(
        &mut self,
        ctx: &mut Ctx,
        value: &ast::Expr,
        ty: &ast::TypeExpr,
        span: Span,
        is_cast: bool,
    ) -> Result<HExpr, Diagnostic> {
        let what = if is_cast { "as" } else { "is" };
        let v = self.synth_expr(ctx, value)?;
        let Some((vc, vargs)) = class_of(ctx, v.ty) else {
            return Err(Diagnostic::new(
                "E1047",
                format!(
                    "`{what}` needs a class or enum instance, found {}",
                    ctx.store.display(v.ty)
                ),
                value.span,
            ));
        };
        let env = self.env.clone();
        let target = resolve_type(ctx, &env, ty)?;
        let Some((tc, targs)) = class_of(ctx, target) else {
            return Err(Diagnostic::new(
                "E1047",
                format!(
                    "the target of `{what}` must be a class or enum type, \
                     found {}",
                    ctx.store.display(target)
                ),
                ty.span,
            ));
        };
        let related = ctx.store.class_extends(ClassId(tc), ClassId(vc))
            || ctx.store.class_extends(ClassId(vc), ClassId(tc));
        if !related || vargs != targs {
            return Err(Diagnostic::new(
                "E1047",
                format!(
                    "`{what}` between {} and {} can never succeed",
                    ctx.store.display(v.ty),
                    ctx.store.display(target)
                ),
                span,
            ));
        }
        if is_cast {
            let mutable = v.mutable;
            Ok(HExpr {
                ty: target,
                mutable,
                kind: HExprKind::CastType {
                    value: Box::new(v),
                    ty: target,
                },
            })
        } else {
            Ok(HExpr {
                ty: BOOL,
                mutable: true,
                kind: HExprKind::IsType {
                    value: Box::new(v),
                    ty: target,
                },
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_closure(
        &mut self,
        ctx: &mut Ctx,
        params: &[ast::Param],
        ret: &Option<ast::TypeExpr>,
        row: &[ast::RowItem],
        expected_ret: Option<TypeId>,
        body: &[ast::Stmt],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let env = self.env.clone();
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
            ptys.push(resolve_type(ctx, &env, &param.ty)?);
            pmuts.push(param.mutable);
        }
        let declared_ret = match ret {
            Some(ty) => Some(resolve_type(ctx, &env, ty)?),
            None => expected_ret,
        };
        let declared_row = resolve_row(ctx, &env, row)?;
        let ret_kind = match declared_ret {
            Some(t) => RetKind::Known(t),
            None => RetKind::ClosureInfer,
        };
        let type_param_count = env.type_names.len() as u32;
        let effect_param_count = env.effect_names.len() as u32;
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
            saw_return: false,
            ret: ret_kind,
            self_class: None,
            ctor: None,
            env: env.clone(),
            declared_row: declared_row.clone(),
            collect_row: false,
        };
        let (body_h, body_ty) = match declared_ret {
            Some(t) => {
                let mode = if t == UNIT {
                    BlockMode::Stmt
                } else {
                    BlockMode::Value(t)
                };
                let (b, _, _) = child.check_block(ctx, body, mode, span)?;
                (b, t)
            }
            None => {
                let (b, ty, _) = child.check_block(ctx, body, BlockMode::Synth, span)?;
                let ty = if ty == NEVER { UNIT } else { ty };
                (b, ty)
            }
        };
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
        let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        let func = ctx.push_func(
            HirFunc {
                imported: false,
                name,
                type_params: type_param_count,
                effect_params: effect_param_count,
                params: ptys.clone(),
                param_muts: pmuts.clone(),
                ret: body_ty,
                row: declared_row.clone(),
                captures: capture_tys,
                locals,
                body: body_h,
            },
            FnSig {
                type_params: env.type_names.clone(),
                effect_params: env.effect_names.clone(),
                params: ptys.clone(),
                param_muts: pmuts.clone(),
                param_names,
                ret: body_ty,
                row: declared_row.clone(),
            },
        );
        let fn_ty = ctx.store.intern_fn(ptys, pmuts, body_ty, declared_row);
        Ok(HExpr {
            ty: fn_ty,
            mutable: true,
            kind: HExprKind::MakeClosure {
                func,
                captures: capture_inits,
            },
        })
    }

    /// Map a surface `sys` member name to its manifest group name.
    fn sys_group(name: &str) -> Option<&'static str> {
        sys_group_name(name)
    }

    /// True when the bare name `sys` means the ABI root object here.
    fn sys_in_scope(&mut self) -> Result<bool, Diagnostic> {
        Ok(self.resolve_name("sys")?.is_none())
    }

    /// Resolve a name to its `use` binding. Locals, module functions,
    /// and module types shadow a `use` binding, per the resolution
    /// order.
    fn use_binding(&mut self, ctx: &Ctx, name: &str) -> Result<Option<UseBinding>, Diagnostic> {
        if self.env.core_scope
            || self.resolve_name(name)?.is_some()
            || ctx.func_index.contains_key(name)
            || ctx.lookup_type(name, &self.env).is_some()
        {
            return Ok(None);
        }
        Ok(ctx.uses.get(name).cloned())
    }

    /// Read `sys.<group>` out of a receiver expression: the qualified
    /// form, or a `use`-bound group alias.
    fn sys_group_of(
        &mut self,
        ctx: &Ctx,
        recv: &ast::Expr,
    ) -> Result<Option<&'static str>, Diagnostic> {
        match &recv.kind {
            ExprKind::Field {
                recv: inner, name, ..
            } => {
                if matches!(inner.kind, ExprKind::Name(ref n) if n == "sys")
                    && self.sys_in_scope()?
                {
                    return Ok(Self::sys_group(name));
                }
            }
            ExprKind::Name(name) => {
                if let Some(UseBinding::SysGroup(group)) = self.use_binding(ctx, name)? {
                    return Ok(Some(group));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Convert one manifest type to a checker type.
    fn abi_type_id(ctx: &mut Ctx, t: lm_abi::AbiType) -> TypeId {
        match t {
            lm_abi::AbiType::Unit => UNIT,
            lm_abi::AbiType::Bool => BOOL,
            lm_abi::AbiType::Int => INT,
            lm_abi::AbiType::Str => STRING,
            lm_abi::AbiType::Bytes => lm_types::BYTES,
            lm_abi::AbiType::StringBuilder => Self::core_class(ctx, "StringBuilder"),
            lm_abi::AbiType::ByteBuffer => Self::core_class(ctx, "ByteBuffer"),
            lm_abi::AbiType::FileHandle => lm_types::FILE_HANDLE,
            lm_abi::AbiType::OpenOptions => Self::core_class(ctx, "OpenOptions"),
            lm_abi::AbiType::SeekFrom => Self::core_class(ctx, "SeekFrom"),
            lm_abi::AbiType::ResultOptionStrIoError => {
                let option = ctx.core_types["Option"];
                let result = ctx.core_types["Result"];
                let io_error = ctx.core_types["IoError"];
                let opt_str = ctx
                    .store
                    .intern(Type::Inst(lm_types::ClassId(option), vec![STRING]));
                let err = ctx.store.intern(Type::Class(lm_types::ClassId(io_error)));
                ctx.store
                    .intern(Type::Inst(lm_types::ClassId(result), vec![opt_str, err]))
            }
            lm_abi::AbiType::ResultSnapshotImageError => {
                let error = Self::core_class(ctx, "SnapshotError");
                Self::core_inst(ctx, "Result", vec![lm_types::SNAPSHOT_IMAGE, error])
            }
            lm_abi::AbiType::ResultFileHandleFsError => {
                let error = Self::core_class(ctx, "FsError");
                Self::core_inst(ctx, "Result", vec![lm_types::FILE_HANDLE, error])
            }
            lm_abi::AbiType::ResultBytesFsError => {
                let error = Self::core_class(ctx, "FsError");
                Self::core_inst(ctx, "Result", vec![lm_types::BYTES, error])
            }
            lm_abi::AbiType::ResultIntFsError => {
                let error = Self::core_class(ctx, "FsError");
                Self::core_inst(ctx, "Result", vec![INT, error])
            }
            lm_abi::AbiType::ResultUnitFsError => {
                let error = Self::core_class(ctx, "FsError");
                Self::core_inst(ctx, "Result", vec![UNIT, error])
            }
        }
    }

    /// The callable function type of one fixed operation.
    fn op_fn_type(ctx: &mut Ctx, op: u32) -> TypeId {
        let def = lm_abi::op(op);
        debug_assert_eq!(def.kind, lm_abi::OpKind::Fixed);
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        let ret = Self::abi_type_id(ctx, def.reply);
        ctx.store
            .intern_fn(params.clone(), vec![false; params.len()], ret, vec![])
    }

    /// The argument-view type of one fixed operation: `()` for a
    /// zero-parameter operation, a tuple otherwise.
    fn op_args_type(ctx: &mut Ctx, op: u32) -> TypeId {
        let def = lm_abi::op(op);
        if def.params.is_empty() {
            return UNIT;
        }
        let elems: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        ctx.store.intern(Type::Tuple(elems))
    }

    /// Charge one exact operation to the enclosing row.
    fn charge_op(&mut self, ctx: &mut Ctx, op: u32, span: Span) -> Result<(), Diagnostic> {
        let idx = ctx.store.intern_row_name(&lm_abi::op_name(op));
        let row = vec![lm_types::RowElem::Op(idx)];
        self.charge_row(ctx, &row, span)
    }

    /// The mailbox message type of the enclosing proc class, when the
    /// method belongs to a subclass of the core class `Proc`.
    fn proc_mailbox_type(&self, ctx: &Ctx) -> Option<TypeId> {
        let class = self.self_class?;
        let proc = *ctx.core_types.get("Proc")?;
        let arity = ctx.classes[class as usize].type_params.len();
        let own: Vec<TypeId> = (0..arity)
            .map(|i| {
                ctx.store
                    .find(&Type::Var(i as u32))
                    .expect("a class parameter type is interned")
            })
            .collect();
        let args =
            ctx.store
                .ancestor_args(lm_types::ClassId(class), &own, lm_types::ClassId(proc))?;
        args.first().copied()
    }

    /// An instance type of a core enum found by name.
    fn core_inst(ctx: &mut Ctx, name: &str, args: Vec<TypeId>) -> TypeId {
        let class = ctx.core_types[name];
        ctx.store.intern(Type::Inst(lm_types::ClassId(class), args))
    }

    /// The instance type of a core enum without type parameters.
    fn core_class(ctx: &mut Ctx, name: &str) -> TypeId {
        let class = ctx.core_types[name];
        ctx.store.intern(Type::Class(lm_types::ClassId(class)))
    }

    /// Reject arguments on a native method that takes none.
    fn expect_no_args(name: &str, args: &[ast::Expr], span: Span) -> Result<(), Diagnostic> {
        if args.is_empty() {
            return Ok(());
        }
        Err(Diagnostic::new(
            "E1006",
            format!("`{name}` expects 0 argument(s), found {}", args.len()),
            span,
        ))
    }

    /// Check a direct operation call `sys.<group>.<Member>(args)`.
    #[allow(clippy::too_many_arguments)]
    fn check_sys_call(
        &mut self,
        ctx: &mut Ctx,
        group: &str,
        member: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "an operation call does not take type arguments",
                name_span,
            ));
        }
        // `sys.vm.Vm()` creates an EmptyVm through `Vm.New`.
        if group == "Vm" && member == "Vm" {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    "`sys.vm.Vm` expects 0 argument(s)",
                    span,
                ));
            }
            self.charge_op(ctx, lm_abi::OP_VM_NEW, span)?;
            return Ok(HExpr {
                ty: lm_types::EMPTY_VM,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_NEW,
                    args: vec![],
                },
            });
        }
        // `sys.vm.snapshot_self()` performs `Vm.SnapshotSelf`. The
        // calling function cannot name the enclosing machine result
        // type, so the reply is an untyped `SnapshotImage`
        // (specification 17.1).
        if group == "Vm" && member == "snapshot_self" {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.vm.snapshot_self` expects 0 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_SELF, span)?;
            let error = Self::core_class(ctx, "SnapshotError");
            let ty = Self::core_inst(ctx, "Result", vec![lm_types::SNAPSHOT_IMAGE, error]);
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_SNAPSHOT_SELF,
                    args: vec![],
                },
            });
        }
        // `sys.proc.recv()` performs `Proc.Recv`. The mailbox type
        // comes from the enclosing proc class, so the call is valid
        // only inside a method of a subclass of `Proc[M]`.
        if group == "Proc" && matches!(member, "recv" | "recv_wait") {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.proc.{member}` expects 0 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            let mailbox = self.proc_mailbox_type(ctx).ok_or_else(|| {
                Diagnostic::new(
                    "E1051",
                    format!(
                        "`sys.proc.{member}` is only valid inside a method of a `Proc` subclass"
                    ),
                    name_span,
                )
            })?;
            // The performing proc is the receiver. Its class fixes the
            // mailbox type, so the verifier reads the class table
            // instead of a claim at the call site.
            let receiver = self.synth_self(ctx, span)?;
            let recv = Self::core_inst(ctx, "Recv", vec![mailbox]);
            let (op, ty) = if member == "recv" {
                (lm_abi::OP_PROC_RECV, recv)
            } else {
                (
                    lm_abi::OP_PROC_RECV_WAIT,
                    ctx.store.intern(Type::Wait(recv)),
                )
            };
            self.charge_op(ctx, op, span)?;
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op,
                    args: vec![receiver],
                },
            });
        }
        // `sys.proc.run(vm)` transfers one loaded machine to the
        // scheduler. The mailbox-bearing form comes from proc-class
        // lowering, so this surface chooses `M = Never`.
        if group == "Proc" && member == "run" {
            if args.len() != 1 {
                return Err(Diagnostic::new(
                    "E1006",
                    format!("`sys.proc.run` expects 1 argument(s), found {}", args.len()),
                    span,
                ));
            }
            let vm = self.synth_expr(ctx, &args[0])?;
            let Type::Vm(result) = ctx.store.get(vm.ty).clone() else {
                return Err(Diagnostic::new(
                    "E1004",
                    format!(
                        "`sys.proc.run` needs a loaded machine, found {}",
                        ctx.store.display(vm.ty)
                    ),
                    args[0].span,
                ));
            };
            self.charge_op(ctx, lm_abi::OP_PROC_RUN, span)?;
            let ty = ctx.store.intern(Type::Handle(NEVER, result));
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_PROC_RUN,
                    args: vec![vm],
                },
            });
        }
        let op = Self::resolve_sys_member(group, member, name_span)?;
        let def = lm_abi::op(op);
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        let muts = vec![false; params.len()];
        let checked = self.check_args_simple(ctx, args, &params, &muts, NO_NAMES, member, span)?;
        self.charge_op(ctx, op, span)?;
        let ret = Self::abi_type_id(ctx, def.reply);
        Ok(HExpr {
            ty: ret,
            mutable: true,
            kind: HExprKind::Perform { op, args: checked },
        })
    }

    /// Resolve one surface member name inside a group to its fixed
    /// operation slot. The surface form is snake_case; a capitalized
    /// spelling of a real operation gets the casing rule.
    fn resolve_sys_member(group: &str, member: &str, name_span: Span) -> Result<u32, Diagnostic> {
        let starts_upper = member
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
        if starts_upper {
            if lm_abi::fixed_member(group, member).is_some() {
                return Err(Diagnostic::new(
                    "E1051",
                    format!(
                        "callable `sys` members use snake_case; write `sys.{}.{}`",
                        group.to_ascii_lowercase(),
                        snake_member(member)
                    ),
                    name_span,
                ));
            }
            return Err(Diagnostic::new(
                "E1051",
                format!("the group `{group}` has no operation named `{member}`"),
                name_span,
            ));
        }
        lm_abi::fixed_member(group, &camel_member(member)).ok_or_else(|| {
            Diagnostic::new(
                "E1051",
                format!("the group `{group}` has no operation named `{member}`"),
                name_span,
            )
        })
    }

    /// Check a first-class operation value `sys.<group>.<member>`.
    fn check_sys_value(
        &mut self,
        ctx: &mut Ctx,
        group: &str,
        member: &str,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if group == "Vm" && member == "Vm" {
            return Err(Diagnostic::new(
                "E1051",
                "`sys.vm.Vm` is not a value; call `sys.vm.Vm()` to create a machine",
                span,
            ));
        }
        let op = Self::resolve_sys_member(group, member, span)?;
        let fn_ty = Self::op_fn_type(ctx, op);
        let ty = ctx.store.intern(Type::Op(op, fn_ty));
        Ok(HExpr {
            ty,
            mutable: true,
            kind: HExprKind::OpConst(op),
        })
    }

    /// Resolve a policy-target descriptor expression: a group name
    /// such as `Io`, or an exact name such as `Clock.Now`.
    fn resolve_descriptor(
        &self,
        expr: &ast::Expr,
    ) -> Result<(TargetKind, u32, String), Diagnostic> {
        self.resolve_descriptor_for(expr, "a policy target")
    }

    /// Resolve a descriptor expression with a context word for the
    /// shape diagnostic.
    fn resolve_descriptor_for(
        &self,
        expr: &ast::Expr,
        what: &str,
    ) -> Result<(TargetKind, u32, String), Diagnostic> {
        match &expr.kind {
            ExprKind::Name(name) => {
                if let Some(slot) = lm_abi::group_by_name(name) {
                    return Ok((TargetKind::Group, slot, name.clone()));
                }
                Err(Diagnostic::new(
                    "E1051",
                    format!("`{name}` is not a group in the operation manifest"),
                    expr.span,
                ))
            }
            ExprKind::Field { recv, name, .. } => {
                if let ExprKind::Name(group) = &recv.kind {
                    let full = format!("{group}.{name}");
                    if let Some(slot) = lm_abi::op_by_name(&full) {
                        return Ok((TargetKind::Exact, slot, full));
                    }
                    return Err(Diagnostic::new(
                        "E1051",
                        format!("`{full}` is not an operation in the operation manifest"),
                        expr.span,
                    ));
                }
                Err(Diagnostic::new(
                    "E1051",
                    format!("{what} must be a group name or an exact operation name"),
                    expr.span,
                ))
            }
            _ => Err(Diagnostic::new(
                "E1051",
                format!("{what} must be a group name or an exact operation name"),
                expr.span,
            )),
        }
    }

    /// Check a policy-table edit method.
    fn check_table_edit(
        &mut self,
        ctx: &mut Ctx,
        table: HExpr,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let action = match name {
            "pass" => TableAction::Pass,
            "block" => TableAction::Block,
            "mock" => TableAction::Mock,
            "clear" => TableAction::Clear,
            _ => {
                return Err(Diagnostic::new(
                    "E1026",
                    format!("`PolicyTable` has no method named `{name}`"),
                    name_span,
                ));
            }
        };
        let want_args = if action == TableAction::Mock { 2 } else { 1 };
        if args.len() != want_args {
            return Err(Diagnostic::new(
                "E1006",
                format!(
                    "`{name}` expects {want_args} argument(s), found {}",
                    args.len()
                ),
                span,
            ));
        }
        let (kind, slot, target_name) = self.resolve_descriptor(&args[0])?;
        let mock = if action == TableAction::Mock {
            if kind != TargetKind::Exact || lm_abi::op(slot).kind != lm_abi::OpKind::Fixed {
                return Err(Diagnostic::new(
                    "E1051",
                    "`mock` needs an exact host operation, for example `Clock.Now`",
                    args[0].span,
                ));
            }
            let handler_ty = Self::op_fn_type(ctx, slot);
            let handler = self.check_expr(ctx, &args[1], handler_ty)?;
            Some(Box::new(handler))
        } else {
            None
        };
        if action == TableAction::Pass {
            // The dependent grant rule: passing authority is charged
            // to the granter's row.
            let idx = ctx.store.intern_row_name(&target_name);
            let row = vec![lm_types::RowElem::Op(idx)];
            self.charge_row(ctx, &row, span)?;
        }
        Ok(HExpr {
            ty: UNIT,
            mutable: true,
            kind: HExprKind::TableEdit {
                action,
                kind,
                slot,
                table: Box::new(table),
                mock,
            },
        })
    }

    /// Check the native methods of the VM control surface: EmptyVm,
    /// Vm[T], resource controls, and the other VM control receivers.
    /// Return `None` when the receiver type has no such method.
    #[allow(clippy::too_many_arguments)]
    fn check_control_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<Option<HExpr>, Diagnostic> {
        let recv_ty = ctx.store.get(recv_h.ty).clone();
        if !matches!(
            recv_ty,
            Type::EmptyVm
                | Type::Vm(_)
                | Type::Wait(_)
                | Type::PolicyTable
                | Type::Request
                | Type::PendingCall(_, _)
                | Type::Handle(_, _)
                | Type::ResourceHandle
                | Type::Fault
        ) {
            return Ok(None);
        }
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "a native control method does not take type arguments",
                name_span,
            ));
        }
        let out = match (recv_ty, name) {
            (Type::EmptyVm, "from_fn") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`from_fn` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                // The type of the second parameter comes from the
                // first, so this method arranges the labels itself
                // instead of calling `check_args_simple`.
                let args = arrange_args(args, &["program", "args"], "from_fn")?;
                let program = self.synth_expr(ctx, args[0])?;
                let Type::Fn(params, _, ret, _) = ctx.store.get(program.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`from_fn` needs a function value, found {}",
                            ctx.store.display(program.ty)
                        ),
                        args[0].span,
                    ));
                };
                let want = if params.is_empty() {
                    UNIT
                } else {
                    ctx.store.intern(Type::Tuple(params))
                };
                let tuple = self.check_expr(ctx, args[1], want)?;
                self.charge_op(ctx, lm_abi::OP_VM_FROM_FN, span)?;
                let vm_ty = ctx.store.intern(Type::Vm(ret));
                HExpr {
                    ty: vm_ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_FROM_FN,
                        args: vec![recv_h, program, tuple],
                    },
                }
            }
            (Type::Vm(t), "snapshot_wait") => {
                // The held form. `Handle[M,R].snapshot_wait` waits on a
                // scheduler proc; this one advances a machine the
                // caller holds.
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`snapshot_wait` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let fuel = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_WAIT_HELD, span)?;
                let snapshot = ctx.store.intern(Type::Snapshot(t));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SNAPSHOT_WAIT_HELD,
                        args: vec![recv_h, fuel],
                    },
                }
            }
            (Type::Vm(t), "drive_for") => {
                // A bounded drive turn. `None` reports that the turn
                // spent its instructions and the machine can run again.
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`drive_for` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let count = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_VM_DRIVE_FOR, span)?;
                let event = Self::core_inst(ctx, "DriveEvent", vec![t]);
                let ty = Self::core_inst(ctx, "Option", vec![event]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DRIVE_FOR,
                        args: vec![recv_h, count],
                    },
                }
            }
            (Type::Vm(t), "run") | (Type::Vm(t), "step") | (Type::Vm(t), "drive") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`{name}` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let (op, event) = match name {
                    "run" => (lm_abi::OP_VM_RUN, "RunResult"),
                    "step" => (lm_abi::OP_VM_STEP, "StepEvent"),
                    _ => (lm_abi::OP_VM_DRIVE, "DriveEvent"),
                };
                self.charge_op(ctx, op, span)?;
                let ty = Self::core_inst(ctx, event, vec![t]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm(t), "drive_wait") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_DRIVE_WAIT, span)?;
                let event = Self::core_inst(ctx, "DriveEvent", vec![t]);
                let ty = ctx.store.intern(Type::Wait(event));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DRIVE_WAIT,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm(t), "snapshot") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_HELD, span)?;
                let snapshot = ctx.store.intern(Type::Snapshot(t));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SNAPSHOT_HELD,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::EmptyVm, "restore") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`restore` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let snapshot = self.synth_expr(ctx, &args[0])?;
                let Type::Snapshot(t) = ctx.store.get(snapshot.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`restore` needs a typed snapshot, found {}",
                            ctx.store.display(snapshot.ty)
                        ),
                        args[0].span,
                    ));
                };
                self.charge_op(ctx, lm_abi::OP_VM_RESTORE, span)?;
                let vm = ctx.store.intern(Type::Vm(t));
                let error = Self::core_class(ctx, "RestoreError");
                let ty = Self::core_inst(ctx, "Result", vec![vm, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESTORE,
                        args: vec![recv_h, snapshot],
                    },
                }
            }
            (Type::Vm(_), "table") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`table` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_TABLE, span)?;
                HExpr {
                    ty: lm_types::POLICY_TABLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_TABLE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm(_), "handles") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_HANDLES, span)?;
                let ty = ctx.store.intern(Type::List(lm_types::RESOURCE_HANDLE));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_HANDLES,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm(_), "resource") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`resource` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let handle = self.check_expr(ctx, &args[0], lm_types::FILE_HANDLE)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE,
                        args: vec![recv_h, handle],
                    },
                }
            }
            (Type::Vm(_), "serve_file") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`serve_file` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let call = self.synth_expr(ctx, &args[0])?;
                let want_args = Self::op_args_type(ctx, lm_abi::OP_FS_OPEN);
                let want_reply = Self::abi_type_id(ctx, lm_abi::op(lm_abi::OP_FS_OPEN).reply);
                if ctx.store.get(call.ty) != &Type::PendingCall(want_args, want_reply) {
                    return Err(Diagnostic::new(
                        "E1004",
                        "`serve_file` needs a current Fs.Open call",
                        args[0].span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_SERVE_FILE, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SERVE_FILE,
                        args: vec![recv_h, call],
                    },
                }
            }
            (Type::Vm(_), "answer") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`answer` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                // The reply type comes from the call token, so this
                // method arranges the labels itself.
                let args = arrange_args(args, &["call", "value"], "answer")?;
                let call = self.synth_expr(ctx, args[0])?;
                let Type::PendingCall(_, reply) = ctx.store.get(call.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`answer` needs a PendingCall token, found {}",
                            ctx.store.display(call.ty)
                        ),
                        args[0].span,
                    ));
                };
                let value = self.check_expr(ctx, args[1], reply)?;
                self.charge_op(ctx, lm_abi::OP_VM_ANSWER, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_ANSWER,
                        args: vec![recv_h, call, value],
                    },
                }
            }
            (Type::Vm(_), "reject") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`reject` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let args = arrange_args(args, &["request", "fault"], "reject")?;
                let request = self.check_expr(ctx, args[0], lm_types::REQUEST)?;
                let fault = self.check_expr(ctx, args[1], lm_types::FAULT)?;
                self.charge_op(ctx, lm_abi::OP_VM_REJECT, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_REJECT,
                        args: vec![recv_h, request, fault],
                    },
                }
            }
            (Type::Vm(_), "dispatch") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`dispatch` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let request = self.check_expr(ctx, &args[0], lm_types::REQUEST)?;
                self.charge_op(ctx, lm_abi::OP_VM_DISPATCH, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DISPATCH,
                        args: vec![recv_h, request],
                    },
                }
            }
            (Type::PolicyTable, _) => {
                return self
                    .check_table_edit(ctx, recv_h, name, name_span, args, span)
                    .map(Some);
            }
            (Type::PendingCall(a, _), "args") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`args` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                HExpr {
                    ty: a,
                    mutable: true,
                    kind: HExprKind::CallArgs {
                        call: Box::new(recv_h),
                    },
                }
            }
            (Type::Wait(t), "wait") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_WAIT_WAIT, span)?;
                HExpr {
                    ty: t,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_WAIT,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Wait(left), "choose") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`choose` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let right = self.synth_expr(ctx, &args[0])?;
                let Type::Wait(right_result) = ctx.store.get(right.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`choose` needs a wait, found {}",
                            ctx.store.display(right.ty)
                        ),
                        args[0].span,
                    ));
                };
                self.charge_op(ctx, lm_abi::OP_WAIT_CHOOSE, span)?;
                let choice = Self::core_inst(ctx, "Choice", vec![left, right_result]);
                let ty = ctx.store.intern(Type::Wait(choice));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_CHOOSE,
                        args: vec![recv_h, right],
                    },
                }
            }
            (Type::Wait(_), "cancel") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_WAIT_CANCEL, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_CANCEL,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Handle(m, _), "send") => {
                if m == NEVER {
                    return Err(Diagnostic::new(
                        "E1026",
                        "a proc with no mailbox has no `send` method",
                        name_span,
                    ));
                }
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`send` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let message = self.check_expr(ctx, &args[0], m)?;
                self.charge_op(ctx, lm_abi::OP_PROC_SEND, span)?;
                let ty = Self::core_class(ctx, "SendResult");
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_SEND,
                        args: vec![recv_h, message],
                    },
                }
            }
            (Type::Handle(_, _), "close") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_PROC_CLOSE, span)?;
                let ty = Self::core_class(ctx, "SendResult");
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_CLOSE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Handle(_, r), "done") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_PROC_DONE, span)?;
                let ty = Self::core_inst(ctx, "ProcResult", vec![r]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_DONE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Handle(_, r), "snapshot_wait") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`snapshot_wait` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let fuel = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_PROC_SNAPSHOT_WAIT, span)?;
                let snapshot = ctx.store.intern(Type::Snapshot(r));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_SNAPSHOT_WAIT,
                        args: vec![recv_h, fuel],
                    },
                }
            }
            (Type::Handle(_, r), "pause") | (Type::Handle(_, r), "resume") => {
                Self::expect_no_args(name, args, span)?;
                let (op, ok) = if name == "pause" {
                    (lm_abi::OP_PROC_PAUSE, ctx.store.intern(Type::Vm(r)))
                } else {
                    (lm_abi::OP_PROC_RESUME, UNIT)
                };
                self.charge_op(ctx, op, span)?;
                let error = Self::core_class(ctx, "ProcError");
                let ty = Self::core_inst(ctx, "Result", vec![ok, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "is_open") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_IS_OPEN, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_IS_OPEN,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "close") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_CLOSE, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_CLOSE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "kind") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_KIND, span)?;
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_KIND,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "same_resource") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "`same_resource` expects 1 argument(s), found {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let other = self.check_expr(ctx, &args[0], lm_types::RESOURCE_HANDLE)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_SAME, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_SAME,
                        args: vec![recv_h, other],
                    },
                }
            }
            // The erased inspection surface of a request. A wildcard
            // arm holds no operation identity, so this names the
            // operation as text for a report or a denial message. The
            // request must still be live: a continuation spends it.
            (Type::Request, "op_name") => {
                Self::expect_no_args(name, args, span)?;
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::RequestOpName {
                        request: Box::new(recv_h),
                    },
                }
            }
            (Type::Fault, "code") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`code` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::FaultCodeGet {
                        fault: Box::new(recv_h),
                    },
                }
            }
            (recv_ty, _) => {
                return Err(Diagnostic::new(
                    "E1026",
                    format!("the type {} has no method named `{name}`", {
                        let id = ctx.store.intern(recv_ty);
                        ctx.store.display(id)
                    }),
                    name_span,
                ));
            }
        };
        Ok(Some(out))
    }

    /// Build one direct call to a final primitive method.
    fn primitive_operator(ctx: &Ctx, class_name: &str, name: &str, args: Vec<HExpr>) -> HExpr {
        let class = *ctx
            .core_types
            .get(class_name)
            .expect("the core primitive class exists");
        let method = ctx.classes[class as usize]
            .methods
            .iter()
            .find(|method| method.name == name)
            .expect("the core primitive method exists");
        debug_assert_eq!(args.len(), method.params.len() + 1);
        HExpr {
            ty: method.ret,
            mutable: true,
            kind: HExprKind::Call {
                func: method.func,
                targs: vec![],
                rowargs: vec![],
                args,
            },
        }
    }

    /// Check one operator against a hook the receiver class declares.
    ///
    /// `a + b` is sugar for `a.__add__(b)`, and the sugar reads the
    /// hook from the class of `a`. The call takes the ordinary method
    /// path, so the declared parameter type checks the right operand,
    /// the declared result type is the result of the operator, the
    /// declared row is charged to the caller, and a class that is not
    /// `final` dispatches virtually.
    ///
    /// A class that declares no hook keeps the rule the caller had.
    ///
    /// The lookup is separate, because it needs the type alone. That
    /// keeps the receiver un-cloned: every arithmetic node of every
    /// program reaches this path, and a clone there costs the square
    /// of the expression size.
    fn find_operator_hook(ctx: &mut Ctx, ty: TypeId, hook: &str) -> Option<OperatorHook> {
        let (class, class_args) = class_of(ctx, ty)?;
        let found = ctx.find_method_owner(class, hook)?;
        Some((class, class_args, found))
    }

    #[allow(clippy::too_many_arguments)]
    fn operator_hook(
        &mut self,
        ctx: &mut Ctx,
        recv: HExpr,
        class: u32,
        class_args: Vec<TypeId>,
        found: (MethodSig, Vec<TypeId>, u32),
        hook: &str,
        operands: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if found.0.params.len() != operands.len() {
            return Err(Diagnostic::new(
                "E1006",
                format!(
                    "the operator hook `{hook}` of `{}` takes {} operand(s)",
                    ctx.classes[class as usize].name,
                    found.0.params.len()
                ),
                span,
            ));
        }
        self.check_declared_method(
            ctx,
            recv,
            class,
            class_args,
            found,
            hook,
            span,
            &[],
            operands,
            None,
            span,
        )
    }

    fn synth_binary(
        &mut self,
        ctx: &mut Ctx,
        op: BinOp,
        left: &ast::Expr,
        right: &ast::Expr,
    ) -> Result<HExpr, Diagnostic> {
        match op {
            BinOp::Add => {
                let l = self.synth_expr(ctx, left)?;
                if let Some((class, cargs, found)) = Self::find_operator_hook(ctx, l.ty, "__add__")
                {
                    return self.operator_hook(
                        ctx,
                        l,
                        class,
                        cargs,
                        found,
                        "__add__",
                        std::slice::from_ref(right),
                        left.span,
                    );
                }
                if l.ty == STRING {
                    let r = self.check_expr(ctx, right, STRING)?;
                    return Ok(Self::primitive_operator(
                        ctx,
                        "String",
                        "__add__",
                        vec![l, r],
                    ));
                }
                if l.ty == lm_types::BYTES {
                    let r = self.check_expr(ctx, right, lm_types::BYTES)?;
                    return Ok(Self::primitive_operator(
                        ctx,
                        "Bytes",
                        "__add__",
                        vec![l, r],
                    ));
                }
                let l = self.expect_compatible(ctx, INT, l, left.span)?;
                let r = self.check_expr(ctx, right, INT)?;
                Ok(Self::primitive_operator(ctx, "Int", "__add__", vec![l, r]))
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                let name = match op {
                    BinOp::Sub => "__sub__",
                    BinOp::Mul => "__mul__",
                    BinOp::Div => "__div__",
                    BinOp::Rem => "__rem__",
                    _ => unreachable!(),
                };
                let l = self.synth_expr(ctx, left)?;
                if let Some((class, cargs, found)) = Self::find_operator_hook(ctx, l.ty, name) {
                    return self.operator_hook(
                        ctx,
                        l,
                        class,
                        cargs,
                        found,
                        name,
                        std::slice::from_ref(right),
                        left.span,
                    );
                }
                let l = self.expect_compatible(ctx, INT, l, left.span)?;
                let r = self.check_expr(ctx, right, INT)?;
                Ok(Self::primitive_operator(ctx, "Int", name, vec![l, r]))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let name = match op {
                    BinOp::Lt => "__lt__",
                    BinOp::Le => "__le__",
                    BinOp::Gt => "__gt__",
                    BinOp::Ge => "__ge__",
                    _ => unreachable!(),
                };
                let l = self.synth_expr(ctx, left)?;
                if let Some((class, cargs, found)) = Self::find_operator_hook(ctx, l.ty, name) {
                    return self.operator_hook(
                        ctx,
                        l,
                        class,
                        cargs,
                        found,
                        name,
                        std::slice::from_ref(right),
                        left.span,
                    );
                }
                let l = self.expect_compatible(ctx, INT, l, left.span)?;
                let r = self.check_expr(ctx, right, INT)?;
                Ok(Self::primitive_operator(ctx, "Int", name, vec![l, r]))
            }
            BinOp::Eq | BinOp::Ne => {
                let l = self.synth_expr(ctx, left)?;
                let hook = if op == BinOp::Eq { "__eq__" } else { "__ne__" };
                if let Some((class, cargs, found)) = Self::find_operator_hook(ctx, l.ty, hook) {
                    return self.operator_hook(
                        ctx,
                        l,
                        class,
                        cargs,
                        found,
                        hook,
                        std::slice::from_ref(right),
                        left.span,
                    );
                }
                let r = self.synth_expr(ctx, right)?;
                let related = ctx.store.compatible(l.ty, r.ty) || ctx.store.compatible(r.ty, l.ty);
                if !related {
                    return Err(self.mismatch(ctx, l.ty, r.ty, right.span));
                }
                let operand_ty = if l.ty == NEVER { r.ty } else { l.ty };
                if matches!(ctx.store.get(operand_ty), Type::Tuple(_)) {
                    // Tuple equality is structural and needs equal
                    // static tuple types (specification 6.4).
                    if l.ty != r.ty && l.ty != NEVER && r.ty != NEVER {
                        return Err(Diagnostic::new(
                            "E1017",
                            format!(
                                "tuple equality needs equal static tuple types; \
                                 the sides are {} and {}",
                                ctx.store.display(l.ty),
                                ctx.store.display(r.ty)
                            ),
                            left.span,
                        ));
                    }
                    if !tuple_comparable(&ctx.store, operand_ty) {
                        return Err(Diagnostic::new(
                            "E1017",
                            format!(
                                "cannot compare {} values with `{}`; a tuple \
                                 element does not support equality",
                                ctx.store.display(operand_ty),
                                op.text()
                            ),
                            left.span,
                        ));
                    }
                    return Ok(HExpr {
                        ty: BOOL,
                        mutable: true,
                        kind: HExprKind::Binary {
                            op,
                            operand_ty,
                            left: Box::new(l),
                            right: Box::new(r),
                        },
                    });
                }
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
                if matches!(operand_ty, INT | BOOL | STRING) || operand_ty == lm_types::BYTES {
                    let class = match operand_ty {
                        BOOL => "Bool",
                        STRING => "String",
                        lm_types::BYTES => "Bytes",
                        _ => "Int",
                    };
                    let name = if op == BinOp::Eq { "__eq__" } else { "__ne__" };
                    return Ok(Self::primitive_operator(ctx, class, name, vec![l, r]));
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

    /// Merge forked constructor states from branches.
    fn merge_ctor_states(
        &mut self,
        entry: Option<CtorState>,
        branch_states: Vec<(CtorState, bool)>,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let (Some(c), Some(entry)) = (self.ctor.as_mut(), entry) else {
            return Ok(());
        };
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
        Ok(())
    }

    /// Extract a flow refinement from a checked condition:
    /// `local is Type` where the target narrows a named local.
    fn refinement_of(&self, ctx: &Ctx, cond: &HExpr) -> Option<(u32, String, TypeId)> {
        if let HExprKind::IsType { value, ty } = &cond.kind {
            if let HExprKind::Local(slot) = value.kind {
                let current = self.locals[slot as usize].0;
                if *ty != current && ctx.store.compatible(current, *ty) {
                    let name = self.name_of_slot(slot)?;
                    return Some((slot, name, *ty));
                }
            }
        }
        None
    }

    /// Find the innermost name bound to one local slot.
    fn name_of_slot(&self, slot: u32) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some((name, _)) = scope.iter().find(|(_, s)| **s == slot) {
                return Some(name.clone());
            }
        }
        None
    }

    /// Bind a refined shadow local inside the freshly pushed branch
    /// scope. The shadow holds the same value behind a checked cast,
    /// so the verifier sees the narrowed type. Return the statement
    /// that initializes the shadow.
    fn bind_refinement(&mut self, slot: u32, name: String, target: TypeId) -> HStmt {
        let (original, mutable) = self.locals[slot as usize];
        let shadow = self.locals.len() as u32;
        self.locals.push((target, mutable));
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .insert(name, shadow);
        HStmt::Assign {
            slot: shadow,
            value: HExpr {
                ty: target,
                mutable,
                kind: HExprKind::CastType {
                    value: Box::new(HExpr {
                        ty: original,
                        mutable,
                        kind: HExprKind::Local(slot),
                    }),
                    ty: target,
                },
            },
        }
    }

    /// Check one branch body inside a fresh scope with an optional
    /// flow refinement, after a constructor-state reset to the fork
    /// entry.
    fn check_branch_body(
        &mut self,
        ctx: &mut Ctx,
        body: &[ast::Stmt],
        mode: BlockMode,
        refinement: Option<(u32, String, TypeId)>,
        entry_state: &Option<CtorState>,
        span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool), Diagnostic> {
        if let (Some(c), Some(entry)) = (self.ctor.as_mut(), entry_state) {
            c.state = entry.clone();
        }
        self.scopes.push(HashMap::new());
        let shadow_init =
            refinement.map(|(slot, name, target)| self.bind_refinement(slot, name, target));
        let result = self.check_block(ctx, body, mode, span);
        self.scopes.pop();
        let (mut body_h, ty, mutable) = result?;
        if let Some(init) = shadow_init {
            body_h.insert(0, init);
        }
        Ok((body_h, ty, mutable))
    }

    /// The sibling hint for deferred branches: the family-widened
    /// join of the branch types that resolved alone. `None` when no
    /// branch resolved.
    fn branch_hint(
        &mut self,
        ctx: &mut Ctx,
        resolved: &[(TypeId, Span)],
    ) -> Result<Option<TypeId>, Diagnostic> {
        let mut hint: Option<TypeId> = None;
        for (ty, span) in resolved {
            if *ty == NEVER {
                continue;
            }
            let wide = deep_widen(ctx, *ty);
            hint = Some(match hint {
                None => wide,
                Some(prev) => ctx
                    .store
                    .join(prev, wide)
                    .ok_or_else(|| self.mismatch(ctx, prev, wide, *span))?,
            });
        }
        Ok(hint)
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
        let synth_join = matches!(branch_mode, BlockMode::Synth);
        // Check every condition first. A later condition runs only
        // when the earlier bodies were skipped, so it checks against
        // the fork entry state, not a branch state.
        let mut conds: Vec<HExpr> = Vec::new();
        let mut refinements: Vec<Option<(u32, String, TypeId)>> = Vec::new();
        let mut ctor_entry: Option<CtorState> = None;
        for (aidx, (cond, _)) in arms.iter().enumerate() {
            let cond = self.check_expr(ctx, cond, BOOL)?;
            if aidx == 0 {
                ctor_entry = self.ctor.as_ref().map(|c| c.state.clone());
            }
            refinements.push(self.refinement_of(ctx, &cond));
            conds.push(cond);
        }
        // The branch bodies in order; the `else` body is the last
        // entry when present.
        type BranchBody<'b> = (&'b Vec<ast::Stmt>, Option<(u32, String, TypeId)>);
        let mut bodies: Vec<BranchBody<'_>> = Vec::new();
        for ((_, body), refinement) in arms.iter().zip(refinements.into_iter()) {
            bodies.push((body, refinement));
        }
        if let Some(body) = else_body {
            bodies.push((body, None));
        }
        // Pass 1: check each body. In synthesis mode a branch with an
        // unresolved constructor (`E1045`) is deferred for the
        // sibling hint. Each result records the constructor state
        // after its body.
        type BranchOut = (Vec<HStmt>, TypeId, bool, Option<CtorState>);
        let mut results: Vec<Option<BranchOut>> = Vec::new();
        let mut gap: Option<Diagnostic> = None;
        for (body, refinement) in &bodies {
            match self.check_branch_body(
                ctx,
                body,
                branch_mode,
                refinement.clone(),
                &ctor_entry,
                span,
            ) {
                Ok((body_h, ty, mutable)) => {
                    let state = self.ctor.as_ref().map(|c| c.state.clone());
                    results.push(Some((body_h, ty, mutable, state)));
                }
                Err(d) if synth_join && is_inference_gap(&d) => {
                    if gap.is_none() {
                        gap = Some(d);
                    }
                    results.push(None);
                }
                Err(d) => return Err(d),
            }
        }
        // Pass 2: with a gap, every branch checks against the sibling
        // hint, so nested constructor arguments adopt one shared
        // instantiation.
        let mut hinted: Option<TypeId> = None;
        if let Some(gap) = gap {
            let resolved: Vec<(TypeId, Span)> = bodies
                .iter()
                .zip(results.iter())
                .filter_map(|((body, _), r)| {
                    r.as_ref()
                        .map(|(_, ty, _, _)| (*ty, body.last().map(|s| s.span).unwrap_or(span)))
                })
                .collect();
            let Some(hint) = self.branch_hint(ctx, &resolved)? else {
                return Err(gap);
            };
            results.clear();
            for (body, refinement) in &bodies {
                let (body_h, ty, mutable) = self.check_branch_body(
                    ctx,
                    body,
                    BlockMode::Value(hint),
                    refinement.clone(),
                    &ctor_entry,
                    span,
                )?;
                let state = self.ctor.as_ref().map(|c| c.state.clone());
                results.push(Some((body_h, ty, mutable, state)));
            }
            hinted = Some(hint);
        }
        // Collect the branch results, states, and types in order.
        let mut branch_types: Vec<(TypeId, bool, Span)> = Vec::new();
        let mut branch_states: Vec<(CtorState, bool)> = Vec::new();
        let mut final_bodies: Vec<Vec<HStmt>> = Vec::new();
        for ((body, _), out) in bodies.iter().zip(results.into_iter()) {
            let (body_h, ty, mutable, state) = out.expect("every branch resolved");
            let diverged = body_h.last().map(HStmt::diverges).unwrap_or(false);
            if let Some(state) = state {
                branch_states.push((state, diverged));
            }
            let branch_span = body.last().map(|s| s.span).unwrap_or(span);
            branch_types.push((ty, mutable, branch_span));
            final_bodies.push(body_h);
        }
        let else_h = if else_body.is_some() {
            final_bodies.pop()
        } else {
            // Without `else` the fork can skip every branch.
            if let Some(entry) = &ctor_entry {
                branch_states.push((entry.clone(), false));
            }
            None
        };
        let checked_arms: Vec<(HExpr, Vec<HStmt>)> = conds.into_iter().zip(final_bodies).collect();
        // Merge constructor states across the non-diverging branches.
        self.merge_ctor_states(ctor_entry, branch_states, span)?;
        let (ty, mutable) = match expected {
            Some(t) => {
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (t, mutable)
            }
            None => {
                if else_h.is_none() {
                    (UNIT, true)
                } else {
                    let ty = match hinted {
                        Some(hint) => self.join_branches(ctx, &branch_types).unwrap_or(hint),
                        None => self.join_branches(ctx, &branch_types)?,
                    };
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

    /// Check one `case` expression.
    fn check_case(
        &mut self,
        ctx: &mut Ctx,
        scrut: &ast::Expr,
        arms: &[ast::CaseArm],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let scrut_h = self.synth_expr(ctx, scrut)?;
        let scrut_ty = scrut_h.ty;
        let scrut_mut = scrut_h.mutable;
        // A hidden slot holds the scrutinee during the arm tests.
        let scrut_slot = self.locals.len() as u32;
        self.locals.push((scrut_ty, scrut_mut));
        let branch_mode = match expected {
            Some(t) => BlockMode::Value(t),
            None => BlockMode::Synth,
        };
        let synth_join = matches!(branch_mode, BlockMode::Synth);
        let ctor_entry = self.ctor.as_ref().map(|c| c.state.clone());
        // Pass 1: check each arm. In synthesis mode an arm body with
        // an unresolved constructor (`E1045`) is deferred for the
        // sibling hint.
        type ArmOut = (HPattern, Vec<HStmt>, TypeId, bool, Option<CtorState>);
        let mut results: Vec<Option<ArmOut>> = Vec::new();
        let mut gap: Option<Diagnostic> = None;
        for arm in arms {
            match self.check_case_arm(ctx, arm, scrut_ty, scrut_mut, branch_mode, &ctor_entry) {
                Ok(out) => results.push(Some(out)),
                Err(d) if synth_join && is_inference_gap(&d) => {
                    if gap.is_none() {
                        gap = Some(d);
                    }
                    results.push(None);
                }
                Err(d) => return Err(d),
            }
        }
        // Pass 2: with a gap, every arm checks against the sibling
        // hint.
        let mut hinted: Option<TypeId> = None;
        if let Some(gap) = gap {
            let resolved: Vec<(TypeId, Span)> = arms
                .iter()
                .zip(results.iter())
                .filter_map(|(arm, r)| r.as_ref().map(|(_, _, ty, _, _)| (*ty, arm.span)))
                .collect();
            let Some(hint) = self.branch_hint(ctx, &resolved)? else {
                return Err(gap);
            };
            results.clear();
            for arm in arms {
                let out = self.check_case_arm(
                    ctx,
                    arm,
                    scrut_ty,
                    scrut_mut,
                    BlockMode::Value(hint),
                    &ctor_entry,
                )?;
                results.push(Some(out));
            }
            hinted = Some(hint);
        }
        let mut checked_arms: Vec<HArm> = Vec::new();
        let mut branch_types: Vec<(TypeId, bool, Span)> = Vec::new();
        let mut branch_states: Vec<(CtorState, bool)> = Vec::new();
        for (arm, out) in arms.iter().zip(results.into_iter()) {
            let (hpat, body_h, ty, mutable, state) = out.expect("every arm resolved");
            let diverged = body_h.last().map(HStmt::diverges).unwrap_or(false);
            if let Some(state) = state {
                branch_states.push((state, diverged));
            }
            branch_types.push((ty, mutable, arm.span));
            checked_arms.push(HArm {
                pattern: hpat,
                body: body_h,
            });
        }
        self.analyze_arms(ctx, scrut_ty, arms, &checked_arms, span)?;
        self.merge_ctor_states(ctor_entry, branch_states, span)?;
        let (ty, mutable) = match expected {
            Some(t) => {
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (t, mutable)
            }
            None => {
                let ty = match hinted {
                    Some(hint) => self.join_branches(ctx, &branch_types).unwrap_or(hint),
                    None => self.join_branches(ctx, &branch_types)?,
                };
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (ty, mutable)
            }
        };
        Ok(HExpr {
            ty,
            mutable,
            kind: HExprKind::Case {
                scrut: Box::new(scrut_h),
                scrut_slot,
                arms: checked_arms,
            },
        })
    }

    /// Check one `case` arm: the pattern and the body share a fresh
    /// scope, after a constructor-state reset to the fork entry.
    #[allow(clippy::type_complexity)]
    fn check_case_arm(
        &mut self,
        ctx: &mut Ctx,
        arm: &ast::CaseArm,
        scrut_ty: TypeId,
        scrut_mut: bool,
        mode: BlockMode,
        entry_state: &Option<CtorState>,
    ) -> Result<(HPattern, Vec<HStmt>, TypeId, bool, Option<CtorState>), Diagnostic> {
        if let (Some(c), Some(entry)) = (self.ctor.as_mut(), entry_state) {
            c.state = entry.clone();
        }
        self.scopes.push(HashMap::new());
        let mut binds: Vec<String> = Vec::new();
        let pat = self.check_pattern(ctx, &arm.pattern, scrut_ty, scrut_mut, &mut binds);
        let result = pat.and_then(|hpat| {
            let (body_h, ty, mutable) = self.check_block(ctx, &arm.body, mode, arm.span)?;
            Ok((hpat, body_h, ty, mutable))
        });
        self.scopes.pop();
        let (hpat, body_h, ty, mutable) = result?;
        let state = self.ctor.as_ref().map(|c| c.state.clone());
        Ok((hpat, body_h, ty, mutable, state))
    }

    /// Prove exhaustiveness and reject unreachable arms.
    fn analyze_arms(
        &self,
        ctx: &Ctx,
        scrut_ty: TypeId,
        arms: &[ast::CaseArm],
        checked: &[HArm],
        span: Span,
    ) -> Result<(), Diagnostic> {
        struct Meta<'a> {
            ctx: &'a Ctx,
        }
        impl<'a> PatMeta for Meta<'a> {
            fn arm_arity(&self, class: u32) -> usize {
                self.ctx.classes[class as usize].field_tys.len()
            }
            fn family_arms(&self, class: u32) -> Vec<u32> {
                match self.ctx.family_of(class) {
                    Some(family) => self.ctx.classes[family as usize].arms.clone(),
                    None => vec![class],
                }
            }
        }
        let meta = Meta { ctx };
        let budget_err = |span| {
            Diagnostic::new(
                "E1049",
                "the case pattern analysis exceeded its work limit; \
                 simplify the patterns",
                span,
            )
        };
        let mut budget = PATTERN_BUDGET;
        let rows: Vec<Vec<APat>> = checked
            .iter()
            .map(|arm| vec![hpat_to_apat(&arm.pattern)])
            .collect();
        for (i, arm) in arms.iter().enumerate() {
            if i == 0 {
                continue;
            }
            let reachable = useful(&meta, &rows[..i], &rows[i], &mut budget)
                .map_err(|_| budget_err(arm.span))?;
            if !reachable {
                return Err(Diagnostic::new(
                    "E1043",
                    "this arm is unreachable; an earlier arm covers it",
                    arm.span,
                ));
            }
        }
        // Values outside the static scrutinee type cannot occur, so
        // they count as covered. The injection is recursive: an
        // arm-typed position at any depth excludes its sibling arms.
        let mut matrix = rows;
        for pat in impossible_patterns(ctx, scrut_ty) {
            matrix.push(vec![pat]);
        }
        let missing =
            useful(&meta, &matrix, &[APat::Wild], &mut budget).map_err(|_| budget_err(span))?;
        if missing {
            return Err(Diagnostic::new(
                "E1042",
                format!(
                    "the case does not cover every value of {}; add the missing \
                     arms or a `_` arm",
                    ctx.store.display(scrut_ty)
                ),
                span,
            ));
        }
        Ok(())
    }

    /// Check one pattern against a scrutinee type and bind its names.
    fn check_pattern(
        &mut self,
        ctx: &mut Ctx,
        pat: &ast::Pattern,
        scrut_ty: TypeId,
        scrut_mut: bool,
        binds: &mut Vec<String>,
    ) -> Result<HPattern, Diagnostic> {
        match &pat.kind {
            PatternKind::Wildcard => Ok(HPattern::Wildcard),
            // `Call(Op, call, args)` on a request: one operation
            // identity test that binds the call and its arguments.
            PatternKind::Ctor {
                qualifier: None,
                name,
                args,
                has_parens: true,
            } if name == "Call" && matches!(ctx.store.get(scrut_ty), Type::Request) => {
                if args.len() != 3 {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!(
                            "`Call` needs an operation, a call binding, and an \
                             argument pattern; found {} pattern(s)",
                            args.len()
                        ),
                        pat.span,
                    ));
                }
                let op = self.pattern_descriptor(&args[0])?;
                let args_ty = Self::op_args_type(ctx, op);
                let reply_ty = Self::abi_type_id(ctx, lm_abi::op(op).reply);
                let call_ty = ctx.store.intern(Type::PendingCall(args_ty, reply_ty));
                let option_ty = Self::core_inst(ctx, "Option", vec![call_ty]);
                let (option_class, option_args) =
                    class_of(ctx, option_ty).expect("the core declares Option");
                let family = ctx
                    .family_of(option_class)
                    .expect("Option is an enum family");
                let some = ctx
                    .find_arm(family, "Some")
                    .expect("Option declares the arm Some");
                let some_ty =
                    ctx.store
                        .substitute(ctx.classes[some as usize].self_ty, &option_args, &[]);
                let call_pat = self.check_pattern(ctx, &args[1], call_ty, scrut_mut, binds)?;
                let args_pat = self.check_pattern(ctx, &args[2], args_ty, scrut_mut, binds)?;
                // The call value serves twice: the binding takes it,
                // and the argument pattern reads its arguments.
                let both = HPattern::And(vec![
                    call_pat,
                    HPattern::Project {
                        projection: Projection::CallArgs,
                        ty: args_ty,
                        inner: Box::new(args_pat),
                    },
                ]);
                Ok(HPattern::Project {
                    projection: Projection::AsCall(op),
                    ty: option_ty,
                    inner: Box::new(HPattern::Ctor {
                        class: some,
                        ty: some_ty,
                        args: vec![both],
                        field_tys: vec![call_ty],
                    }),
                })
            }
            // `()` is the unit pattern. Unit holds one value, so the
            // pattern binds nothing and always matches.
            PatternKind::Tuple(elems) if elems.is_empty() => {
                if scrut_ty != UNIT {
                    return Err(self.pattern_mismatch(ctx, "a unit", scrut_ty, pat.span));
                }
                Ok(HPattern::Wildcard)
            }
            PatternKind::Tuple(elems) => {
                let Type::Tuple(elem_tys) = ctx.store.get(scrut_ty).clone() else {
                    return Err(self.pattern_mismatch(ctx, "a tuple", scrut_ty, pat.span));
                };
                if elem_tys.len() != elems.len() {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!(
                            "this tuple pattern has {} element(s), but {} has {}",
                            elems.len(),
                            ctx.store.display(scrut_ty),
                            elem_tys.len()
                        ),
                        pat.span,
                    ));
                }
                let mut out = Vec::with_capacity(elems.len());
                for (sub, ty) in elems.iter().zip(elem_tys.iter()) {
                    out.push(self.check_pattern(ctx, sub, *ty, scrut_mut, binds)?);
                }
                Ok(HPattern::Tuple {
                    elems: out,
                    elem_tys,
                })
            }
            PatternKind::Int(v) => {
                if scrut_ty != INT {
                    return Err(self.pattern_mismatch(
                        ctx,
                        "an integer literal",
                        scrut_ty,
                        pat.span,
                    ));
                }
                Ok(HPattern::Int(*v))
            }
            PatternKind::Bool(v) => {
                if scrut_ty != BOOL {
                    return Err(self.pattern_mismatch(ctx, "a Bool literal", scrut_ty, pat.span));
                }
                Ok(HPattern::Bool(*v))
            }
            PatternKind::Str(v) => {
                if scrut_ty != STRING {
                    return Err(self.pattern_mismatch(ctx, "a string literal", scrut_ty, pat.span));
                }
                Ok(HPattern::Str(v.clone()))
            }
            PatternKind::Name(name) => {
                // An arm name of the scrutinee enum is a constructor.
                if let Some((class, class_args)) = class_of(ctx, scrut_ty) {
                    if let Some(family) = ctx.family_of(class) {
                        if let Some(arm) = ctx.find_arm(family, name) {
                            return self.ctor_pattern(
                                ctx,
                                arm,
                                class,
                                &class_args,
                                &[],
                                false,
                                pat.span,
                                binds,
                                scrut_mut,
                            );
                        }
                    }
                }
                if binds.contains(name) {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!("the name `{name}` appears twice in this pattern"),
                        pat.span,
                    ));
                }
                binds.push(name.clone());
                let slot = self.locals.len() as u32;
                self.locals.push((scrut_ty, scrut_mut));
                self.scopes
                    .last_mut()
                    .expect("a scope is always open")
                    .insert(name.clone(), slot);
                Ok(HPattern::Bind(slot))
            }
            PatternKind::Ctor {
                qualifier,
                name,
                args,
                has_parens,
            } => {
                let Some((class, class_args)) = class_of(ctx, scrut_ty) else {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!(
                            "the scrutinee type {} has no constructors; use a \
                             binding or `_`",
                            ctx.store.display(scrut_ty)
                        ),
                        pat.span,
                    ));
                };
                let Some(family) = ctx.family_of(class) else {
                    // An ordinary class constructor pattern: it tests
                    // the scrutinee class and binds the full field
                    // layout in declaration order.
                    let named = ctx.lookup_type(name, &self.env);
                    if qualifier.is_some() || named != Some(class) {
                        return Err(Diagnostic::new(
                            "E1041",
                            format!(
                                "a class constructor pattern must name the \
                                 scrutinee class `{}`",
                                ctx.classes[class as usize].name
                            ),
                            pat.span,
                        ));
                    }
                    let field_tys = ctx.classes[class as usize].field_tys.clone();
                    if !*has_parens || args.len() != field_tys.len() {
                        return Err(Diagnostic::new(
                            "E1041",
                            format!(
                                "the class `{name}` has {} field(s), found {} \
                                 pattern(s)",
                                field_tys.len(),
                                args.len()
                            ),
                            pat.span,
                        ));
                    }
                    let mut sub = Vec::with_capacity(args.len());
                    let mut sub_tys = Vec::with_capacity(args.len());
                    for (arg, field_ty) in args.iter().zip(field_tys.iter()) {
                        let sub_ty = ctx.store.substitute(*field_ty, &class_args, &[]);
                        sub.push(self.check_pattern(ctx, arg, sub_ty, scrut_mut, binds)?);
                        sub_tys.push(sub_ty);
                    }
                    return Ok(HPattern::Ctor {
                        class,
                        ty: scrut_ty,
                        args: sub,
                        field_tys: sub_tys,
                    });
                };
                if let Some(q) = qualifier {
                    let named = ctx.lookup_type(q, &self.env);
                    if named != Some(family) {
                        return Err(Diagnostic::new(
                            "E1041",
                            format!(
                                "the qualifier `{q}` does not name the scrutinee \
                                 enum `{}`",
                                ctx.classes[family as usize].name
                            ),
                            pat.span,
                        ));
                    }
                }
                let Some(arm) = ctx.find_arm(family, name) else {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!(
                            "the enum `{}` has no arm named `{name}`",
                            ctx.classes[family as usize].name
                        ),
                        pat.span,
                    ));
                };
                self.ctor_pattern(
                    ctx,
                    arm,
                    class,
                    &class_args,
                    args,
                    *has_parens,
                    pat.span,
                    binds,
                    scrut_mut,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn ctor_pattern(
        &mut self,
        ctx: &mut Ctx,
        arm: u32,
        scrut_class: u32,
        class_args: &[TypeId],
        args: &[ast::Pattern],
        has_parens: bool,
        span: Span,
        binds: &mut Vec<String>,
        scrut_mut: bool,
    ) -> Result<HPattern, Diagnostic> {
        // A case-typed scrutinee only matches its own arm.
        if ctx.classes[scrut_class as usize].kind == ClassKind::EnumCase && arm != scrut_class {
            return Err(Diagnostic::new(
                "E1041",
                format!(
                    "the pattern `{}` can never match a value of `{}`",
                    ctx.classes[arm as usize].arm_short, ctx.classes[scrut_class as usize].name
                ),
                span,
            ));
        }
        let field_tys = ctx.classes[arm as usize].field_tys.clone();
        let arm_short = ctx.classes[arm as usize].arm_short.clone();
        if !has_parens && !field_tys.is_empty() {
            return Err(Diagnostic::new(
                "E1041",
                format!(
                    "the arm `{arm_short}` has {} field(s); write \
                     `{arm_short}(...)`",
                    field_tys.len()
                ),
                span,
            ));
        }
        if has_parens && args.len() != field_tys.len() {
            return Err(Diagnostic::new(
                "E1041",
                format!(
                    "the arm `{arm_short}` has {} field(s), found {} pattern(s)",
                    field_tys.len(),
                    args.len()
                ),
                span,
            ));
        }
        let mut sub = Vec::with_capacity(args.len());
        let mut sub_tys = Vec::with_capacity(args.len());
        for (arg, field_ty) in args.iter().zip(field_tys.iter()) {
            let sub_ty = ctx.store.substitute(*field_ty, class_args, &[]);
            sub.push(self.check_pattern(ctx, arg, sub_ty, scrut_mut, binds)?);
            sub_tys.push(sub_ty);
        }
        let arm_self = ctx.classes[arm as usize].self_ty;
        let arm_ty = ctx.store.substitute(arm_self, class_args, &[]);
        Ok(HPattern::Ctor {
            class: arm,
            ty: arm_ty,
            args: sub,
            field_tys: sub_tys,
        })
    }

    /// Resolve one exact operation named in pattern position, such as
    /// `Fs.Read`. The parser reads it as a qualified constructor with
    /// no arguments.
    fn pattern_descriptor(&self, pat: &ast::Pattern) -> Result<u32, Diagnostic> {
        let PatternKind::Ctor {
            qualifier: Some(group),
            name,
            args,
            has_parens: false,
        } = &pat.kind
        else {
            return Err(Diagnostic::new(
                "E1041",
                "`Call` needs an exact operation, for example `Fs.Read`",
                pat.span,
            ));
        };
        if !args.is_empty() {
            return Err(Diagnostic::new(
                "E1041",
                "`Call` needs an exact operation, for example `Fs.Read`",
                pat.span,
            ));
        }
        let full = format!("{group}.{name}");
        lm_abi::op_by_name(&full).ok_or_else(|| {
            Diagnostic::new(
                "E1051",
                format!("`{full}` is not an operation of the manifest"),
                pat.span,
            )
        })
    }

    fn pattern_mismatch(&self, ctx: &Ctx, what: &str, scrut_ty: TypeId, span: Span) -> Diagnostic {
        Diagnostic::new(
            "E1041",
            format!(
                "{what} pattern cannot match a scrutinee of type {}",
                ctx.store.display(scrut_ty)
            ),
            span,
        )
    }

    /// Synthesize sibling expressions of one collection position and
    /// join their types.
    ///
    /// An element whose constructor cannot resolve its type arguments
    /// alone (`E1045`, for example a bare `None`) adopts the unique
    /// solution from its siblings: the family-widened join of the
    /// elements that synthesize. Without such a sibling, the original
    /// error stands. The pass never invents a type and never searches.
    fn synth_join_elems(
        &mut self,
        ctx: &mut Ctx,
        items: &[&ast::Expr],
    ) -> Result<(Vec<HExpr>, TypeId), Diagnostic> {
        let mut first: Vec<Option<HExpr>> = Vec::with_capacity(items.len());
        let mut gap: Option<Diagnostic> = None;
        for item in items {
            match self.synth_expr(ctx, item) {
                Ok(h) => first.push(Some(h)),
                Err(d) if is_inference_gap(&d) => {
                    if gap.is_none() {
                        gap = Some(d);
                    }
                    first.push(None);
                }
                Err(d) => return Err(d),
            }
        }
        if let Some(gap) = gap {
            // The hint is the family-widened join of the elements
            // that resolved alone.
            let mut hint: Option<TypeId> = None;
            for (h, item) in first.iter().zip(items.iter()) {
                let Some(h) = h else { continue };
                let wide = deep_widen(ctx, h.ty);
                hint = Some(match hint {
                    None => wide,
                    Some(prev) => ctx
                        .store
                        .join(prev, wide)
                        .ok_or_else(|| self.mismatch(ctx, prev, wide, item.span))?,
                });
            }
            let Some(hint) = hint else {
                return Err(gap);
            };
            // Check every element against the sibling hint, so nested
            // constructor arguments adopt one shared instantiation.
            let mut checked = Vec::with_capacity(items.len());
            let mut joined: Option<TypeId> = None;
            for item in items {
                let h = self.check_expr(ctx, item, hint)?;
                joined = Some(match joined {
                    None => h.ty,
                    Some(prev) => ctx.store.join(prev, h.ty).unwrap_or(hint),
                });
                checked.push(h);
            }
            let ty = joined.unwrap_or(hint);
            return Ok((checked, ty));
        }
        let mut checked = Vec::with_capacity(items.len());
        let mut joined: Option<TypeId> = None;
        for (h, item) in first.into_iter().zip(items.iter()) {
            let h = h.expect("every element resolved");
            joined = Some(match joined {
                None => h.ty,
                Some(prev) => ctx
                    .store
                    .join(prev, h.ty)
                    .ok_or_else(|| self.mismatch(ctx, prev, h.ty, item.span))?,
            });
            checked.push(h);
        }
        let ty = joined.expect("the caller rejects empty literals");
        Ok((checked, ty))
    }

    /// Join branch types. `Never` branches do not contribute.
    fn join_branches(
        &self,
        ctx: &mut Ctx,
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
