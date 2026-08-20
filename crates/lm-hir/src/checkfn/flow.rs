//! Flow refinement, branches, and case analysis.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Merge forked constructor states from branches.
    pub(super) fn merge_ctor_states(
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
    pub(super) fn refinement_of(&self, ctx: &Ctx, cond: &HExpr) -> Option<(u32, String, TypeId)> {
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
    pub(super) fn name_of_slot(&self, slot: u32) -> Option<String> {
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
    pub(super) fn bind_refinement(&mut self, slot: u32, name: String, target: TypeId) -> HStmt {
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
    pub(super) fn check_branch_body(
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
    pub(super) fn branch_hint(
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
    pub(super) fn check_if(
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
    pub(super) fn check_case(
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
    pub(super) fn check_case_arm(
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
    pub(super) fn analyze_arms(
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
                    ctx.display_type(&self.env, scrut_ty)
                ),
                span,
            ));
        }
        Ok(())
    }
}
