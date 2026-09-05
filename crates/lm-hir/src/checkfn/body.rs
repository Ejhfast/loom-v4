//! Expression bodies, assignment, and loops.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Check an expression list. Return the lowering steps, the block type,
    /// and the block-value capability.
    pub(super) fn check_block(
        &mut self,
        ctx: &mut Ctx,
        exprs: &[ast::Expr],
        mode: BlockMode,
        block_span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool), Diagnostic> {
        let mut out = Vec::with_capacity(exprs.len());
        for (idx, expr) in exprs.iter().enumerate() {
            if let Some(prev) = out.last() {
                let prev: &HStmt = prev;
                if prev.diverges() {
                    return Err(Diagnostic::new(
                        "E1021",
                        "this expression is unreachable",
                        expr.span,
                    ));
                }
            }
            let is_last = idx + 1 == exprs.len();
            if is_last {
                match mode {
                    BlockMode::Value(expected) => {
                        let value = self.check_expr(ctx, expr, expected)?;
                        let mutable = value.flow == Flow::Never || value.mutable;
                        out.push(HStmt::Expr(value));
                        return Ok((out, expected, mutable));
                    }
                    BlockMode::Synth => {
                        let value = self.synth_expr(ctx, expr)?;
                        let ty = if value.flow == Flow::Never {
                            NEVER
                        } else {
                            value.ty
                        };
                        let mutable = value.flow == Flow::Never || value.mutable;
                        out.push(HStmt::Expr(value));
                        return Ok((out, ty, mutable));
                    }
                    BlockMode::Discard => {}
                }
            }
            out.push(self.check_discarded(ctx, expr)?);
        }
        let (block_ty, mutable) = match mode {
            BlockMode::Discard => (UNIT, true),
            BlockMode::Value(expected) => {
                // The list is empty, so the block value is `()`.
                if expected != UNIT {
                    return Err(self.mismatch(ctx, expected, UNIT, block_span));
                }
                (UNIT, true)
            }
            BlockMode::Synth => match out.last() {
                Some(HStmt::Expr(e)) if e.flow == Flow::Never => (NEVER, true),
                Some(HStmt::Expr(e)) => (e.ty, e.mutable),
                Some(stmt) if stmt.diverges() => (NEVER, true),
                _ => (UNIT, true),
            },
        };
        Ok((out, block_ty, mutable))
    }

    /// Check one expression whose value is discarded.
    pub(super) fn check_discarded(
        &mut self,
        ctx: &mut Ctx,
        expr: &ast::Expr,
    ) -> Result<HStmt, Diagnostic> {
        match &expr.kind {
            ExprKind::Assign {
                name,
                name_span,
                ty,
                value,
            } => self.check_assign(ctx, name, *name_span, ty, value),
            ExprKind::Destructure { pattern, value } => self.check_destructure(ctx, pattern, value),
            ExprKind::AssignField {
                recv,
                field,
                field_span,
                value,
            } => self.check_assign_field(ctx, recv, field, *field_span, value),
            ExprKind::While { cond, body } => self.check_while(ctx, cond, body, expr.span),
            ExprKind::For {
                bindings,
                value,
                body,
            } => self.check_for(ctx, bindings, value, body, expr.span),
            ExprKind::Loop { body } => Ok(HStmt::Expr(
                self.check_loop(ctx, body, BlockMode::Discard, expr.span)?
                    .finish_flow(),
            )),
            ExprKind::Return { value } => self.check_return(ctx, value.as_deref(), expr.span),
            ExprKind::Break { value } => self.check_break(ctx, value.as_deref(), expr.span),
            ExprKind::Continue => {
                if self.loops.is_empty() {
                    return Err(Diagnostic::new(
                        "E1008",
                        "`continue` is only valid inside a loop",
                        expr.span,
                    ));
                }
                Ok(HStmt::Continue)
            }
            ExprKind::If { arms, else_body } => Ok(HStmt::Expr(
                self.check_if(ctx, arms, else_body, BlockMode::Discard, expr.span)?
                    .finish_flow(),
            )),
            ExprKind::Case { scrut, arms } => Ok(HStmt::Expr(
                self.check_case(ctx, scrut, arms, BlockMode::Discard, expr.span)?
                    .finish_flow(),
            )),
            ExprKind::Select { arms } => Ok(HStmt::Expr(
                self.check_select(ctx, arms, BlockMode::Discard, expr.span)?
                    .finish_flow(),
            )),
            _ => Ok(HStmt::Expr(self.synth_expr(ctx, expr)?)),
        }
    }

    pub(super) fn check_while(
        &mut self,
        ctx: &mut Ctx,
        cond: &ast::Expr,
        body: &[ast::Expr],
        span: Span,
    ) -> Result<HStmt, Diagnostic> {
        let before = self.ctor.as_ref().map(|item| item.state.clone());
        let cond = self.check_expr(ctx, cond, BOOL)?;
        self.ctor_guard_loop(&before, span)?;
        let snapshot = self.ctor.as_ref().map(|item| item.state.clone());
        self.scopes.push(Scope::default());
        self.loops.push(LoopContext {
            mode: LoopMode::UnitOnly,
            breaks: Vec::new(),
            inference_gap: None,
        });
        let result = self.check_block(ctx, body, BlockMode::Discard, span);
        self.loops.pop();
        self.scopes.pop();
        let (body, _, _) = result?;
        self.ctor_guard_loop(&snapshot, span)?;
        if let (Some(ctor), Some(snapshot)) = (self.ctor.as_mut(), snapshot) {
            ctor.state = snapshot;
        }
        Ok(HStmt::While { cond, body })
    }

    pub(super) fn check_return(
        &mut self,
        ctx: &mut Ctx,
        source: Option<&ast::Expr>,
        span: Span,
    ) -> Result<HStmt, Diagnostic> {
        let ret = match self.ret {
            RetKind::Known(ty) => ty,
            RetKind::Entry => {
                return Err(Diagnostic::new(
                    "E1016",
                    "`return` is not valid at the top level of a module",
                    span,
                ));
            }
            RetKind::ClosureInfer => {
                return Err(Diagnostic::new(
                    "E1016",
                    "`return` needs a declared closure result type",
                    span,
                ));
            }
        };
        let value = match source {
            Some(source) => Some(self.check_expr(ctx, source, ret)?),
            None if ret == UNIT => None,
            None => return Err(self.mismatch(ctx, ret, UNIT, span)),
        };
        if value
            .as_ref()
            .is_some_and(|value| ctx.store.contains_callback(value.ty))
        {
            return Err(Diagnostic::new(
                "E1064",
                "a function cannot return a nonescaping callback",
                span,
            ));
        }
        if let Some(ctor) = &self.ctor {
            require_complete(ctx, ctor.class, ctor, span)?;
        }
        self.saw_return = true;
        Ok(HStmt::Return { value })
    }

    pub(super) fn check_break(
        &mut self,
        ctx: &mut Ctx,
        source: Option<&ast::Expr>,
        span: Span,
    ) -> Result<HStmt, Diagnostic> {
        let Some(mode) = self.loops.last().map(|context| context.mode) else {
            return Err(Diagnostic::new(
                "E1008",
                "`break` is only valid inside a loop",
                span,
            ));
        };
        if source.is_some() && matches!(mode, LoopMode::UnitOnly) {
            return Err(Diagnostic::new(
                "E1008",
                "a `break` in `while` or `for` cannot carry a value",
                span,
            ));
        }
        let value = match (mode, source) {
            (LoopMode::Value(expected), Some(source)) => {
                Some(self.check_expr(ctx, source, expected)?)
            }
            (LoopMode::Synth, Some(source)) => match self.synth_expr(ctx, source) {
                Ok(value) => Some(value),
                Err(diagnostic) if is_inference_gap(&diagnostic) => {
                    let context = self.loops.last_mut().expect("the loop context exists");
                    if context.inference_gap.is_none() {
                        context.inference_gap = Some(diagnostic);
                    }
                    None
                }
                Err(diagnostic) => return Err(diagnostic),
            },
            (_, Some(source)) => Some(self.synth_expr(ctx, source)?),
            (LoopMode::Value(expected), None) if expected != UNIT => {
                return Err(self.mismatch(ctx, expected, UNIT, span));
            }
            _ => None,
        };
        if source.is_some() && value.is_none() {
            return Ok(HStmt::Break { value });
        }
        let break_ty = value.as_ref().map_or(UNIT, |value| value.ty);
        let break_flow = value.as_ref().map_or(Flow::Normal, |value| value.flow);
        let break_mutable = value.as_ref().is_none_or(|value| value.mutable);
        if break_flow == Flow::Normal && !matches!(mode, LoopMode::UnitOnly) {
            self.loops
                .last_mut()
                .expect("the loop context exists")
                .breaks
                .push((break_ty, break_mutable, span));
        }
        Ok(HStmt::Break { value })
    }

    pub(super) fn check_loop(
        &mut self,
        ctx: &mut Ctx,
        body: &[ast::Expr],
        mode: BlockMode,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let loop_mode = match mode {
            BlockMode::Discard => LoopMode::Discard,
            BlockMode::Value(expected) => LoopMode::Value(expected),
            BlockMode::Synth => LoopMode::Synth,
        };
        let before = self.ctor.as_ref().map(|item| item.state.clone());
        let local_count = self.locals.len();
        let func_count = ctx.funcs.len();
        let sig_count = ctx.sigs.len();
        let (mut checked_body, mut context) = self.check_loop_body(ctx, body, loop_mode, span)?;
        let mut inferred_hint = None;
        if let Some(gap) = context.inference_gap.take() {
            let resolved: Vec<(TypeId, Span)> = context
                .breaks
                .iter()
                .map(|(ty, _, span)| (*ty, *span))
                .collect();
            let Some(hint) = self.branch_hint(ctx, &resolved)? else {
                return Err(gap);
            };

            // Discard the incomplete pass before checking every break
            // against the shared sibling hint.
            self.locals.truncate(local_count);
            ctx.funcs.truncate(func_count);
            ctx.sigs.truncate(sig_count);
            if let (Some(ctor), Some(before)) = (self.ctor.as_mut(), before.as_ref()) {
                ctor.state = before.clone();
            }
            (checked_body, context) =
                self.check_loop_body(ctx, body, LoopMode::Value(hint), span)?;
            inferred_hint = Some(hint);
        }
        self.ctor_guard_loop(&before, span)?;
        if let (Some(ctor), Some(before)) = (self.ctor.as_mut(), before) {
            ctor.state = before;
        }

        let ty = if context.breaks.is_empty() {
            NEVER
        } else {
            match loop_mode {
                LoopMode::Discard => UNIT,
                LoopMode::Value(expected) => expected,
                LoopMode::Synth => match inferred_hint {
                    Some(hint) => hint,
                    None => {
                        let resolved: Vec<(TypeId, Span)> = context
                            .breaks
                            .iter()
                            .map(|(ty, _, span)| (*ty, *span))
                            .collect();
                        self.branch_hint(ctx, &resolved)?
                            .expect("a break supplies a type")
                    }
                },
                LoopMode::UnitOnly => unreachable!("a value loop has a value mode"),
            }
        };
        let mutable = if ty == UNIT || ty == NEVER {
            true
        } else {
            context.breaks.iter().all(|(_, mutable, _)| *mutable)
        };
        let result_slot = (ty != UNIT && ty != NEVER).then(|| self.hidden_local(ty, mutable));
        Ok(HExpr {
            flow: Flow::Normal,
            ty,
            mutable,
            kind: HExprKind::Loop {
                body: checked_body,
                result_slot,
            },
        })
    }

    fn check_loop_body(
        &mut self,
        ctx: &mut Ctx,
        body: &[ast::Expr],
        mode: LoopMode,
        span: Span,
    ) -> Result<(Vec<HStmt>, LoopContext), Diagnostic> {
        self.scopes.push(Scope::default());
        self.loops.push(LoopContext {
            mode,
            breaks: Vec::new(),
            inference_gap: None,
        });
        let result = self.check_block(ctx, body, BlockMode::Discard, span);
        let context = self.loops.pop().expect("the loop context exists");
        self.scopes.pop();
        let (body, _, _) = result?;
        Ok((body, context))
    }

    pub(super) fn hidden_local(&mut self, ty: TypeId, mutable: bool) -> u32 {
        let slot = self.locals.len() as u32;
        self.locals.push((ty, mutable));
        slot
    }

    pub(super) fn local_expr(&self, slot: u32) -> HExpr {
        let (ty, mutable) = self.locals[slot as usize];
        HExpr {
            flow: Flow::Normal,
            ty,
            mutable,
            kind: HExprKind::Local(slot),
        }
    }

    pub(super) fn interface_associated(
        &mut self,
        ctx: &mut Ctx,
        ty: TypeId,
        interface: u32,
        assoc: u32,
    ) -> Option<TypeId> {
        match ctx.store.get(ty).clone() {
            Type::Var(index) if index >= self.env.type_offset => {
                let bounds = self
                    .env
                    .type_bounds
                    .get((index - self.env.type_offset) as usize)?;
                bounds.iter().find(|item| item.interface == interface)?;
                Some(
                    ctx.store
                        .project(ty, lm_types::InterfaceId(interface), assoc),
                )
            }
            Type::Projection {
                interface: owner,
                assoc: owner_assoc,
                ..
            } => {
                ctx.interfaces[owner.0 as usize]
                    .associated
                    .get(owner_assoc as usize)?
                    .bounds
                    .iter()
                    .find(|bound| bound.interface == interface)?;
                Some(
                    ctx.store
                        .project(ty, lm_types::InterfaceId(interface), assoc),
                )
            }
            _ => ctx.conformance_associated(&self.env, ty, interface, assoc),
        }
    }

    pub(super) fn call_zero_method(
        &mut self,
        ctx: &mut Ctx,
        recv: HExpr,
        name: &str,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if let Some((class, class_args)) = class_of(ctx, recv.ty) {
            let found = ctx.find_method_owner(class, name).ok_or_else(|| {
                Diagnostic::new(
                    "E1026",
                    format!(
                        "the class `{}` has no method named `{name}`",
                        ctx.classes[class as usize].name
                    ),
                    span,
                )
            })?;
            return self.check_declared_method(
                ctx,
                recv,
                class,
                class_args,
                found,
                name,
                span,
                &[],
                &[],
                None,
                span,
            );
        }
        let Some((application, interface, method, requirement)) =
            ctx.bound_method(&self.env, recv.ty, name, span)?
        else {
            return Err(Diagnostic::new(
                "E1053",
                format!(
                    "the type {} has no interface method named `{name}`",
                    ctx.display_type(&self.env, recv.ty)
                ),
                span,
            ));
        };
        let requirement = ctx.instantiate_interface_method(recv.ty, &application, &requirement);
        if requirement.mut_self && !recv.mutable {
            return Err(Diagnostic::new(
                "E1035",
                format!("the method `{name}` needs a mutable receiver"),
                span,
            ));
        }
        if requirement.mut_self {
            self.guard_iterated_mutation(&recv, span)?;
        }
        if !requirement.params.is_empty() {
            return Err(Diagnostic::new(
                "E1053",
                format!("the interface method `{name}` must take no arguments"),
                span,
            ));
        }
        self.charge_row(ctx, &requirement.row, span)?;
        let ret = ctx.normalize_associated(&self.env, requirement.ret);
        Ok(HExpr {
            flow: Flow::Normal,
            ty: ret,
            mutable: true,
            kind: HExprKind::InterfaceCall {
                recv: Box::new(recv),
                interface,
                method,
                selector: name.to_string(),
                own_targs: Vec::new(),
                own_rowargs: Vec::new(),
                args: Vec::new(),
            },
        })
    }

    pub(super) fn for_bindings(
        &mut self,
        bindings: &[(String, Span)],
        types: &[TypeId],
        mutable: bool,
    ) -> Result<(Vec<u32>, Scope), Diagnostic> {
        if bindings.len() != types.len() {
            let found = bindings.len();
            let expected = types.len();
            return Err(Diagnostic::new(
                "E1054",
                format!("this loop needs {expected} binding(s), found {found}"),
                bindings
                    .first()
                    .map(|item| item.1)
                    .unwrap_or(Span::new(0, 0)),
            ));
        }
        let mut slots = Vec::with_capacity(bindings.len());
        let mut scope = Scope::default();
        for ((name, span), ty) in bindings.iter().zip(types) {
            if name != "_" && scope.contains_key(name) {
                return Err(Diagnostic::new(
                    "E1010",
                    format!("the loop binding `{name}` occurs more than once"),
                    *span,
                ));
            }
            let slot = self.hidden_local(*ty, mutable);
            if name != "_" {
                scope.insert(name.clone(), slot);
            }
            slots.push(slot);
        }
        Ok((slots, scope))
    }

    pub(super) fn check_for(
        &mut self,
        ctx: &mut Ctx,
        bindings: &[(String, Span)],
        value: &ast::Expr,
        body: &[ast::Expr],
        span: Span,
    ) -> Result<HStmt, Diagnostic> {
        let before = self.ctor.as_ref().map(|item| item.state.clone());
        let source = self.synth_expr(ctx, value)?;
        let source_ty = source.ty;
        let source_mut = source.mutable;
        let iterated = iterated_place(&source);

        let (kind, binding_types, binding_mut) = match ctx.store.get(source_ty).clone() {
            Type::List(element) => {
                let source_slot = self.hidden_local(source_ty, source_mut);
                let index_slot = self.hidden_local(INT, true);
                let epoch_slot = self.hidden_local(INT, true);
                (
                    HForKind::List {
                        source_slot,
                        index_slot,
                        epoch_slot,
                        element,
                    },
                    vec![element],
                    source_mut,
                )
            }
            Type::Map(key, map_value) => {
                let source_slot = self.hidden_local(source_ty, source_mut);
                let index_slot = self.hidden_local(INT, true);
                let epoch_slot = self.hidden_local(INT, true);
                let pair = ctx.store.intern(Type::Tuple(vec![key, map_value]));
                let binding_types = match bindings.len() {
                    1 => vec![pair],
                    2 => vec![key, map_value],
                    _ => vec![pair],
                };
                (
                    HForKind::Map {
                        source_slot,
                        index_slot,
                        epoch_slot,
                        key,
                        value: map_value,
                        pair,
                    },
                    binding_types,
                    source_mut,
                )
            }
            _ => {
                let nominal = class_of(ctx, source_ty).map(|item| item.0);
                let text = ctx.core_types.get("Text").copied();
                let is_text = nominal.zip(text).is_some_and(|(class, text)| {
                    ctx.store.class_extends(ClassId(class), ClassId(text))
                });
                let range = ctx.core_types.get("Range").copied();
                if is_text {
                    let item = ctx
                        .core_types
                        .get("Char")
                        .and_then(|class| ctx.classes.get(*class as usize))
                        .map(|class| class.self_ty)
                        .unwrap_or_else(|| ctx.omit_core_type("Char"));
                    let source_slot = self.hidden_local(source_ty, source_mut);
                    let cursor_slot = self.hidden_local(INT, true);
                    (
                        HForKind::Text {
                            source_slot,
                            cursor_slot,
                            item,
                        },
                        vec![item],
                        true,
                    )
                } else if nominal == range {
                    let source_slot = self.hidden_local(source_ty, source_mut);
                    let cursor_slot = self.hidden_local(INT, true);
                    let stop_slot = self.hidden_local(INT, true);
                    (
                        HForKind::Range {
                            source_slot,
                            cursor_slot,
                            stop_slot,
                        },
                        vec![INT],
                        true,
                    )
                } else {
                    let iterable = ctx.core_interface("Iterable", span)?;
                    let item_index = ctx.interfaces[iterable as usize]
                        .associated
                        .iter()
                        .position(|item| item.name == "Item")
                        .expect("the core Iterable interface declares Item")
                        as u32;
                    let expected_item = self
                        .interface_associated(ctx, source_ty, iterable, item_index)
                        .ok_or_else(|| {
                            Diagnostic::new(
                                "E1053",
                                format!(
                                    "the type {} does not conform to Iterable",
                                    ctx.display_type(&self.env, source_ty)
                                ),
                                value.span,
                            )
                        })?;
                    let source_slot = self.hidden_local(source_ty, source_mut);
                    let iterator =
                        self.call_zero_method(ctx, self.local_expr(source_slot), "iterator", span)?;
                    let iterator_slot = self.hidden_local(iterator.ty, true);
                    let next =
                        self.call_zero_method(ctx, self.local_expr(iterator_slot), "next", span)?;
                    let Some((option, args)) = ctx.store.nominal_class(next.ty) else {
                        return Err(Diagnostic::new(
                            "E1053",
                            "Iterator.next must return Option[Item]",
                            span,
                        ));
                    };
                    if option.0 != ctx.core.option_class || args.len() != 1 {
                        return Err(Diagnostic::new(
                            "E1053",
                            "Iterator.next must return Option[Item]",
                            span,
                        ));
                    }
                    let item = args[0];
                    let item_matches = ctx.store.compatible(item, expected_item)
                        && ctx.store.compatible(expected_item, item);
                    if !(ctx.store.contains_var(source_ty) || item_matches) {
                        return Err(Diagnostic::new(
                            "E1053",
                            "Iterable.Item must equal Iterable.Iter.Item",
                            span,
                        ));
                    }
                    let some_ty = ctx.store.substitute(
                        ctx.classes[ctx.core.some_class as usize].self_ty,
                        &[item],
                        &[],
                    );
                    let option_slot = self.hidden_local(next.ty, true);
                    let (binding_types, item_slot) = if bindings.len() == 2 {
                        let Type::Tuple(items) = ctx.store.get(item).clone() else {
                            return Err(Diagnostic::new(
                                "E1054",
                                "two loop bindings need a two-element item tuple",
                                span,
                            ));
                        };
                        if items.len() != 2 {
                            return Err(Diagnostic::new(
                                "E1054",
                                "two loop bindings need a two-element item tuple",
                                span,
                            ));
                        }
                        let item_slot = self.hidden_local(item, true);
                        (items, Some(item_slot))
                    } else {
                        (vec![item], None)
                    };
                    (
                        HForKind::Generic {
                            source_slot,
                            iterator_slot,
                            option_slot,
                            item_slot,
                            iterator,
                            next: Box::new(next),
                            some_ty,
                            item,
                        },
                        binding_types,
                        true,
                    )
                }
            }
        };

        self.ctor_guard_loop(&before, span)?;
        let snapshot = self.ctor.as_ref().map(|item| item.state.clone());
        let (bindings, scope) = self.for_bindings(bindings, &binding_types, binding_mut)?;
        self.scopes.push(scope);
        self.loops.push(LoopContext {
            mode: LoopMode::UnitOnly,
            breaks: Vec::new(),
            inference_gap: None,
        });
        let tracks_iterated = iterated.is_some();
        if let Some(place) = iterated {
            self.iterated_places.push(place);
        }
        let result = self.check_block(ctx, body, BlockMode::Discard, span);
        if tracks_iterated {
            self.iterated_places.pop();
        }
        self.loops.pop();
        self.scopes.pop();
        let (body, _, _) = result?;
        self.ctor_guard_loop(&snapshot, span)?;
        if let (Some(ctor), Some(snapshot)) = (self.ctor.as_mut(), snapshot) {
            ctor.state = snapshot;
        }
        Ok(HStmt::For {
            source,
            bindings,
            kind,
            body,
        })
    }

    /// Reject a `super.init` call inside a loop condition or body.
    pub(super) fn ctor_guard_loop(
        &self,
        before: &Option<CtorState>,
        span: Span,
    ) -> Result<(), Diagnostic> {
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

    pub(super) fn check_assign(
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
                if ctx.store.contains_callback(expected) {
                    return Err(Diagnostic::new(
                        "E1064",
                        "a nonescaping callback cannot be rebound",
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
                if self.module_func(ctx, name).is_some()
                    || ctx.lookup_type(name, &self.env).is_some()
                    || ctx.constant_names.contains(name)
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
                        if ctx.store.contains_callback(ty) {
                            return Err(Diagnostic::new(
                                "E1064",
                                "a nonescaping callback cannot be stored in a local",
                                name_span,
                            ));
                        }
                        (value, ty)
                    }
                };
                if ctx.store.contains_callback(local_ty) {
                    return Err(Diagnostic::new(
                        "E1064",
                        "a nonescaping callback cannot be stored in a local",
                        name_span,
                    ));
                }
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

    pub(super) fn check_assign_field(
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
            if ctx.store.contains_callback(value.ty) {
                return Err(Diagnostic::new(
                    "E1064",
                    "a field cannot store a nonescaping callback",
                    field_span,
                ));
            }
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
                format!(
                    "the type {} has no fields",
                    ctx.display_type(&self.env, recv_h.ty)
                ),
                recv.span,
            ));
        };
        if matches!(
            ctx.classes[class as usize].native_repr,
            Some(
                NativeRepr::ModuleCode
                    | NativeRepr::DeclarationCode
                    | NativeRepr::MemberCode
                    | NativeRepr::OpenCode
            )
        ) {
            return Err(unknown_field(ctx, class, field, field_span));
        }
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
        self.guard_iterated_mutation(&recv_h, field_span)?;
        let declared = ctx.classes[class as usize].field_tys[fidx];
        let field_ty = ctx.store.substitute(declared, &class_args, &[]);
        let value = self.check_expr(ctx, value, field_ty)?;
        if ctx.store.contains_callback(value.ty) {
            return Err(Diagnostic::new(
                "E1064",
                "a field cannot store a nonescaping callback",
                field_span,
            ));
        }
        Ok(HStmt::AssignField {
            recv: recv_h,
            field: fidx as u32,
            value,
        })
    }

    /// Build the `self` expression for a method body.
    pub(super) fn self_value(&self) -> HExpr {
        let (ty, mutable) = self.locals[0];
        debug_assert_eq!(self.lookup_slot("self"), Some(0));
        HExpr {
            flow: Flow::Normal,
            ty,
            mutable,
            kind: HExprKind::Local(0),
        }
    }
}
