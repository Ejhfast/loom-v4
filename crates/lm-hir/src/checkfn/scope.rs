//! Slots, names, rows, and type compatibility.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    pub(crate) fn top_level(ret: RetKind, env: TyEnv, declared_row: Row) -> FnChecker<'static> {
        FnChecker {
            outer: None,
            locals: Vec::new(),
            scopes: vec![Scope::default()],
            captures: Vec::new(),
            is_closure: false,
            loops: Vec::new(),
            iterated_places: Vec::new(),
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

    pub(crate) fn reserve_parameters(&mut self, count: usize) {
        self.locals.reserve(count);
        self.scopes[0].bindings.reserve(count);
    }

    pub(super) fn lookup_slot(&self, name: &str) -> Option<u32> {
        for scope in self.scopes.iter().rev() {
            if let Some(slot) = scope.get(name) {
                return Some(*slot);
            }
        }
        None
    }

    /// Find one user module function in a user body.
    pub(super) fn module_func(&self, ctx: &Ctx, name: &str) -> Option<u32> {
        if self.env.core_scope {
            return ctx.core_func_index.get(name).copied();
        }
        ctx.func_index.get(name).copied().or_else(|| {
            ctx.prelude
                .then(|| ctx.core_func_index.get(name).copied())
                .flatten()
        })
    }

    /// Resolve a name to a local or a capture, registering transitive
    /// captures on demand.
    pub(super) fn resolve_name(&mut self, name: &str) -> Result<Option<NameRes>, Diagnostic> {
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

    pub(super) fn mismatch(
        &self,
        ctx: &Ctx,
        expected: TypeId,
        found: TypeId,
        span: Span,
    ) -> Diagnostic {
        Diagnostic::new(
            "E1004",
            format!(
                "expected {}, found {}",
                ctx.display_type(&self.env, expected),
                ctx.display_type(&self.env, found)
            ),
            span,
        )
    }

    /// Reject a direct mutation of an active `for` source.
    pub(super) fn guard_iterated_mutation(
        &self,
        value: &HExpr,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let Some(place) = iterated_place(value) else {
            return Ok(());
        };
        if self.iterated_places.contains(&place) {
            return Err(Diagnostic::new(
                "E1065",
                "cannot mutate a value during its `for` traversal",
                span,
            ));
        }
        Ok(())
    }

    /// Require the row of a call to be inside the declared row. The
    /// entry checker collects the union instead.
    pub(super) fn charge_row(
        &mut self,
        ctx: &Ctx,
        row: &Row,
        span: Span,
    ) -> Result<(), Diagnostic> {
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
            format!("`{}`", ctx.display_row(&self.env, &self.declared_row))
        };
        Err(Diagnostic::new(
            "E1046",
            format!(
                "this call needs effect row `{}`, but the enclosing callable declares \
                 {declared}. Add the row to the enclosing callable's `with` clause",
                ctx.display_row(&self.env, row)
            ),
            span,
        ))
    }

    /// Check a full callable body and package the result.
    pub(crate) fn check_callable(
        mut self,
        ctx: &mut Ctx,
        exprs: &[ast::Expr],
        ret: TypeId,
        span: Span,
    ) -> Result<CheckedBody, Diagnostic> {
        let mode = if ret == UNIT {
            BlockMode::Discard
        } else {
            BlockMode::Value(ret)
        };
        let (body, _, _) = self.check_block(ctx, exprs, mode, span)?;
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
                    ctx.display_type(&self.env, ret)
                ),
                span,
            ));
        }
        Ok(CheckedBody {
            body,
            locals: self.locals.iter().map(|(t, _)| *t).collect(),
            type_bounds: self.env.type_bounds,
            diverges,
            ctor: self.ctor,
        })
    }

    /// Check the module entry block and synthesize its type. The last
    /// result element is the collected entry row.
    pub(crate) fn check_entry(
        mut self,
        ctx: &mut Ctx,
        exprs: &[ast::Expr],
        span: Span,
    ) -> Result<CheckedEntry, Diagnostic> {
        let (body, ty, mutable) = self.check_block(ctx, exprs, BlockMode::Synth, span)?;
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
        Ok(self.check_expr_inner(ctx, expr, expected)?.finish_flow())
    }

    fn check_expr_inner(
        &mut self,
        ctx: &mut Ctx,
        expr: &ast::Expr,
        expected: TypeId,
    ) -> Result<HExpr, Diagnostic> {
        match &expr.kind {
            ExprKind::If { arms, else_body } => {
                self.check_if(ctx, arms, else_body, BlockMode::Value(expected), expr.span)
            }
            ExprKind::Case { scrut, arms } => {
                self.check_case(ctx, scrut, arms, BlockMode::Value(expected), expr.span)
            }
            ExprKind::Select { arms } => {
                self.check_select(ctx, arms, BlockMode::Value(expected), expr.span)
            }
            ExprKind::Loop { body } => {
                self.check_loop(ctx, body, BlockMode::Value(expected), expr.span)
            }
            ExprKind::TupleLit(items) => {
                if let Type::Tuple(elems) = ctx.store.get(expected).clone() {
                    if elems.len() == items.len() {
                        if ctx.store.contains_callback(expected) {
                            return Err(Diagnostic::new(
                                "E1064",
                                "a tuple cannot store a nonescaping callback",
                                expr.span,
                            ));
                        }
                        let mut checked = Vec::new();
                        for (item, elem) in items.iter().zip(elems.iter()) {
                            checked.push(self.check_expr(ctx, item, *elem)?);
                        }
                        return Ok(HExpr {
                            flow: Flow::Normal,
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
                    if ctx.store.contains_callback(expected) {
                        return Err(Diagnostic::new(
                            "E1064",
                            "a list cannot store a nonescaping callback",
                            expr.span,
                        ));
                    }
                    let elem = *elem;
                    let mut checked = Vec::new();
                    for item in items {
                        checked.push(self.check_expr(ctx, item, elem)?);
                    }
                    return Ok(HExpr {
                        flow: Flow::Normal,
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
                    if ctx.store.contains_callback(expected) {
                        return Err(Diagnostic::new(
                            "E1064",
                            "a map cannot store a nonescaping callback",
                            expr.span,
                        ));
                    }
                    let (k, v) = (*k, *v);
                    let mut checked = Vec::new();
                    for (key, value) in entries {
                        let key = self.check_expr(ctx, key, k)?;
                        let value = self.check_expr(ctx, value, v)?;
                        checked.push((key, value));
                    }
                    return Ok(HExpr {
                        flow: Flow::Normal,
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
                row_explicit: _,
                body,
            } => {
                let expected_ret = match (ret, ctx.store.get(expected)) {
                    (None, Type::Fn(_, _, r, _) | Type::Callback(_, _, r, _)) => Some(*r),
                    _ => None,
                };
                let found = self.check_closure(
                    ctx,
                    params,
                    ret,
                    row,
                    expected_ret,
                    false,
                    body,
                    expr.span,
                )?;
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

    pub(super) fn expect_compatible(
        &self,
        ctx: &Ctx,
        expected: TypeId,
        mut found: HExpr,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if found.flow == Flow::Never {
            return Ok(found);
        }
        if ctx.store.contains_callback(found.ty)
            && !matches!(ctx.store.get(expected), Type::Callback(..))
        {
            return Err(Diagnostic::new(
                "E1064",
                "a nonescaping callback cannot enter an escaping value",
                span,
            ));
        }
        if callback_accepts(ctx, expected, found.ty) {
            found.kind = match found.kind {
                HExprKind::MakeClosure { func, captures } => {
                    HExprKind::MakeCallback { func, captures }
                }
                other => HExprKind::AsCallback(Box::new(HExpr {
                    flow: Flow::Normal,
                    ty: found.ty,
                    mutable: found.mutable,
                    kind: other,
                })),
            };
            found.ty = expected;
            return Ok(found);
        }
        if !ctx.store.compatible(expected, found.ty) {
            return Err(self.mismatch(ctx, expected, found.ty, span));
        }
        Ok(found)
    }
}
