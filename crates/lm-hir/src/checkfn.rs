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

use crate::check::{check_key_type, resolve_row, resolve_type, Ctx, FnSig, TyEnv};
use crate::exhaust::{useful, APat, PatMeta};
use crate::hir::*;
use lm_source::ast::{self, BinOp, ExprKind, PatternKind, StmtKind};
use lm_source::diag::Diagnostic;
use lm_source::span::Span;
use lm_types::{
    ClassId, ClassKind, Row, Type, TypeId, BOOL, BYTE_BUFFER, INT, NEVER, STRING, STRING_BUILDER,
    UNIT,
};
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
    NativeSb,
    NativeBb,
    /// `List[T]()` with explicit arguments.
    ListCtor(TypeId),
    /// `Map[K, V]()` with explicit arguments.
    MapCtor(TypeId, TypeId),
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
    ret: RetKind,
    pub(crate) self_class: Option<u32>,
    pub(crate) ctor: Option<CtorCtx>,
    pub(crate) env: TyEnv,
    declared_row: Row,
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

/// Extract the nominal class and arguments of one instance type.
fn class_of(ctx: &Ctx, ty: TypeId) -> Option<(u32, Vec<TypeId>)> {
    match ctx.store.get(ty) {
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
        Type::Fn(params, ret, _) => {
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
        (Type::Fn(dp, dr, drow), Type::Fn(ap, ar, arow)) => {
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
            ret,
            self_class: None,
            ctor: None,
            env,
            declared_row,
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

    /// Require the row of a call to be inside the declared row.
    fn charge_row(&self, ctx: &Ctx, row: &Row, span: Span) -> Result<(), Diagnostic> {
        if ctx.store.row_included(row, &self.declared_row) {
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
        Ok(CheckedBody {
            body,
            locals: self.locals.iter().map(|(t, _)| *t).collect(),
            diverges,
            ctor: self.ctor,
        })
    }

    /// Check the module entry block and synthesize its type.
    pub(crate) fn check_entry(
        mut self,
        ctx: &mut Ctx,
        stmts: &[ast::Stmt],
        span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool, Vec<TypeId>), Diagnostic> {
        let (body, ty, mutable) = self.check_block(ctx, stmts, BlockMode::Synth, span)?;
        let locals = self.locals.iter().map(|(t, _)| *t).collect();
        Ok((body, ty, mutable, locals))
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
                    (None, Type::Fn(_, r, _)) => Some(*r),
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
                if ctx.func_index.contains_key(name) || ctx.lookup_type(name, &self.env).is_some() {
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
                if ctx.func_index.contains_key(name) {
                    return Err(Diagnostic::new(
                        "E1018",
                        format!("the function `{name}` is not a value in this language slice"),
                        expr.span,
                    ));
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
                let info = &ctx.classes[class as usize];
                let type_names = info.type_params.clone();
                let ret = info.self_ty;
                let (params, muts, row) = match &info.init {
                    Some(init) => (
                        init.params.clone(),
                        init.param_muts.clone(),
                        init.row.clone(),
                    ),
                    None => (vec![], vec![], vec![]),
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
            Callee::NativeSb => {
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
            Callee::NativeBb => {
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
        }
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
        if let Some(func) = ctx.func_index.get(name).copied() {
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
            "StringBuilder" => return Ok(Callee::NativeSb),
            "ByteBuffer" => return Ok(Callee::NativeBb),
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
        let (params, ret, row) = match ctx.store.get(callee.ty) {
            Type::Fn(params, ret, row) => (params.clone(), *ret, row.clone()),
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
        let muts = vec![false; params.len()];
        let args = self.check_args_simple(ctx, args, &params, &muts, "closure", span)?;
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
    fn check_args_simple(
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

    /// True when an argument can be synthesized without an expected
    /// type. This gates the inference pass; a wrong `true` degrades to
    /// an inner diagnostic that asks for explicit arguments.
    fn can_synth(&self, ctx: &Ctx, expr: &ast::Expr) -> bool {
        match &expr.kind {
            ExprKind::ListLit(items) => !items.is_empty(),
            ExprKind::MapLit(entries) => !entries.is_empty(),
            ExprKind::TupleLit(items) => items.iter().all(|i| self.can_synth(ctx, i)),
            ExprKind::Name(name) => {
                if self.lookup_slot(name).is_some() || ctx.func_index.contains_key(name) {
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
                    || ctx.func_index.contains_key(name)
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
        // still contains an unresolved variable.
        let mut pre: Vec<Option<HExpr>> = Vec::with_capacity(args.len());
        for (arg, decl) in args.iter().zip(decl_params.iter()) {
            let part = self.partial_substitute(ctx, *decl, &targs, &rowargs);
            if ctx.store.contains_var(part) && self.can_synth(ctx, arg) {
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
            if self.lookup_slot(name).is_none() && !ctx.func_index.contains_key(name) {
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
        let recv_h = self.synth_expr(ctx, recv)?;
        let recv_ty = recv_h.ty;
        // Class and enum methods first, then the universal `freeze`.
        if let Some((class, class_args)) = class_of(ctx, recv_ty) {
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
                    sig.ret,
                    &sig.row,
                    args,
                    expected,
                )?;
                let own_targs = out.targs[own_start..].to_vec();
                return Ok(HExpr {
                    ty: out.ret,
                    mutable: true,
                    kind: HExprKind::MethodCall {
                        recv: Box::new(recv_h),
                        selector: name.to_string(),
                        own_targs,
                        own_rowargs: out.rowargs,
                        args: out.args,
                    },
                });
            }
            if name == "freeze" && args.is_empty() && type_args.is_empty() {
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
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "a native method does not take type arguments",
                name_span,
            ));
        }
        // Native methods on collections and builders.
        let store_ty = ctx.store.get(recv_ty).clone();
        let native = |op: NativeOp, params: Vec<TypeId>, ret: TypeId, needs_mut: bool| {
            (op, params, ret, needs_mut)
        };
        let (op, params, ret, needs_mut) = match (&store_ty, name) {
            (Type::List(_), "len") => native(NativeOp::ListLen, vec![], INT, false),
            (Type::List(e), "at") => native(NativeOp::ListAt, vec![INT], *e, false),
            (Type::List(e), "push") => native(NativeOp::ListPush, vec![*e], UNIT, true),
            (Type::List(e), "get") => {
                let ret = ctx.option_of(*e);
                native(NativeOp::ListGet, vec![INT], ret, false)
            }
            (Type::Map(_, _), "len") => native(NativeOp::MapLen, vec![], INT, false),
            (Type::Map(k, _), "has") => native(NativeOp::MapHas, vec![*k], BOOL, false),
            (Type::Map(k, v), "at") => native(NativeOp::MapAt, vec![*k], *v, false),
            (Type::Map(k, v), "put") => native(NativeOp::MapPut, vec![*k, *v], UNIT, true),
            (Type::Map(k, v), "get") => {
                let ret = ctx.option_of(*v);
                native(NativeOp::MapGet, vec![*k], ret, false)
            }
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
        let checked = self.check_args_simple(ctx, args, &params, &muts, name, span)?;
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
            self.charge_row(ctx, &parent_init.row, span)?;
            let checked = self.check_args_simple(
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
            let mut all_args = vec![self.self_value()];
            all_args.extend(checked);
            return Ok(HExpr {
                ty: UNIT,
                mutable: true,
                kind: HExprKind::Call {
                    func: parent_init.func,
                    targs: vec![],
                    rowargs: vec![],
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
            sig.ret,
            &sig.row,
            args,
            None,
        )?;
        let mut all_args = vec![self_expr];
        all_args.extend(out.args);
        Ok(HExpr {
            ty: out.ret,
            mutable: true,
            kind: HExprKind::Call {
                func: sig.func,
                targs: out.targs,
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
            ret: ret_kind,
            self_class: None,
            ctor: None,
            env: env.clone(),
            declared_row: declared_row.clone(),
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
        let func = ctx.push_func(
            HirFunc {
                name,
                type_params: type_param_count,
                effect_params: effect_param_count,
                params: ptys.clone(),
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
                param_muts: pmuts,
                ret: body_ty,
                row: declared_row.clone(),
            },
        );
        let fn_ty = ctx.store.intern_fn(ptys, body_ty, declared_row);
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
                if matches!(ctx.store.get(operand_ty), Type::Tuple(_)) {
                    return Err(Diagnostic::new(
                        "E1017",
                        format!(
                            "cannot compare {} values with `{}`; tuple equality \
                             is not defined",
                            ctx.store.display(operand_ty),
                            op.text()
                        ),
                        left.span,
                    ));
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
            // Apply the flow refinement of `local is Type` inside the
            // true branch only: the branch scope shadows the name
            // with a narrowed local behind a checked cast.
            let refinement = self.refinement_of(ctx, &cond);
            self.scopes.push(HashMap::new());
            let shadow_init =
                refinement.map(|(slot, name, target)| self.bind_refinement(slot, name, target));
            let result = self.check_block(ctx, body, branch_mode, span);
            self.scopes.pop();
            let (mut body_h, ty, mutable) = result?;
            if let Some(init) = shadow_init {
                body_h.insert(0, init);
            }
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
        let mut checked_arms: Vec<HArm> = Vec::new();
        let mut branch_types: Vec<(TypeId, bool, Span)> = Vec::new();
        let mut branch_states: Vec<(CtorState, bool)> = Vec::new();
        let ctor_entry = self.ctor.as_ref().map(|c| c.state.clone());
        for arm in arms {
            if let (Some(c), Some(entry)) = (self.ctor.as_mut(), &ctor_entry) {
                c.state = entry.clone();
            }
            self.scopes.push(HashMap::new());
            let mut binds: Vec<String> = Vec::new();
            let pat = self.check_pattern(ctx, &arm.pattern, scrut_ty, scrut_mut, &mut binds);
            let result = pat.and_then(|hpat| {
                let (body_h, ty, mutable) =
                    self.check_block(ctx, &arm.body, branch_mode, arm.span)?;
                Ok((hpat, body_h, ty, mutable))
            });
            self.scopes.pop();
            let (hpat, body_h, ty, mutable) = result?;
            let diverged = body_h.last().map(HStmt::diverges).unwrap_or(false);
            if let Some(c) = &self.ctor {
                branch_states.push((c.state.clone(), diverged));
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
                let ty = self.join_branches(ctx, &branch_types)?;
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
        // For an arm-typed scrutinee the sibling arms cannot occur, so
        // they count as covered.
        let mut matrix = rows;
        if let Some((class, _)) = class_of(ctx, scrut_ty) {
            if ctx.classes[class as usize].kind == ClassKind::EnumCase {
                let family = ctx.classes[class as usize].family.expect("case has family");
                for sibling in &ctx.classes[family as usize].arms {
                    if *sibling != class {
                        let arity = ctx.classes[*sibling as usize].field_tys.len();
                        matrix.push(vec![APat::Ctor(*sibling, vec![APat::Wild; arity])]);
                    }
                }
            }
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
                    return Err(Diagnostic::new(
                        "E1041",
                        "class constructor patterns are not supported in this \
                         slice; use a binding or `_`",
                        pat.span,
                    ));
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
        for (arg, field_ty) in args.iter().zip(field_tys.iter()) {
            let sub_ty = ctx.store.substitute(*field_ty, class_args, &[]);
            sub.push(self.check_pattern(ctx, arg, sub_ty, scrut_mut, binds)?);
        }
        let arm_self = ctx.classes[arm as usize].self_ty;
        let arm_ty = ctx.store.substitute(arm_self, class_args, &[]);
        Ok(HPattern::Ctor {
            class: arm,
            ty: arm_ty,
            args: sub,
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

fn hpat_to_apat(pat: &HPattern) -> APat {
    match pat {
        HPattern::Wildcard | HPattern::Bind(_) => APat::Wild,
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

/// Replace an empty diagnostic span with a real one.
fn reposition(mut d: Diagnostic, span: Span) -> Diagnostic {
    if d.span.lo == 0 && d.span.hi == 0 {
        d.span = span;
    }
    d
}
