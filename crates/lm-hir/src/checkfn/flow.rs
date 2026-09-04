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
    /// so the verifier sees the narrowed type. Return the lowering step
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
                flow: Flow::Normal,
                ty: target,
                mutable,
                kind: HExprKind::CastType {
                    value: Box::new(HExpr {
                        flow: Flow::Normal,
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
        body: &[ast::Expr],
        mode: BlockMode,
        refinement: Option<(u32, String, TypeId)>,
        entry_state: &Option<CtorState>,
        span: Span,
    ) -> Result<(Vec<HStmt>, TypeId, bool), Diagnostic> {
        if let (Some(c), Some(entry)) = (self.ctor.as_mut(), entry_state) {
            c.state = entry.clone();
        }
        self.scopes.push(Scope::default());
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

    /// Check an `if` expression in the supplied block mode.
    pub(super) fn check_if(
        &mut self,
        ctx: &mut Ctx,
        arms: &[(ast::Expr, Vec<ast::Expr>)],
        else_body: &Option<Vec<ast::Expr>>,
        mode: BlockMode,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if let BlockMode::Value(expected) = mode {
            if else_body.is_none() && expected != UNIT {
                return Err(self.mismatch(ctx, expected, UNIT, span));
            }
        }
        let branch_mode = match (mode, else_body) {
            (BlockMode::Discard, _) => BlockMode::Discard,
            (BlockMode::Value(t), _) => BlockMode::Value(t),
            (BlockMode::Synth, Some(_)) => BlockMode::Synth,
            // A value-position `if` without `else` gives unit. Each
            // branch must therefore produce unit.
            (BlockMode::Synth, None) => BlockMode::Value(UNIT),
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
        type BranchBody<'b> = (&'b Vec<ast::Expr>, Option<(u32, String, TypeId)>);
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
        let (ty, mutable) = match mode {
            BlockMode::Discard => (UNIT, true),
            BlockMode::Value(t) => {
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (t, mutable)
            }
            BlockMode::Synth => {
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
            flow: Flow::Normal,
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
        mode: BlockMode,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let scrut_h = self.synth_expr(ctx, scrut)?;
        self.check_case_value(ctx, scrut_h, arms, mode, span)
    }

    /// Check a case expression with an existing scrutinee value.
    pub(super) fn check_case_value(
        &mut self,
        ctx: &mut Ctx,
        scrut_h: HExpr,
        arms: &[ast::CaseArm],
        mode: BlockMode,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if arms
            .iter()
            .any(|arm| matches!(arm.pattern.kind, PatternKind::Reflect { .. }))
        {
            return self.check_reflect_case_value(ctx, scrut_h, arms, mode, span);
        }
        let scrut_ty = scrut_h.ty;
        let scrut_mut = scrut_h.mutable;
        // A hidden slot holds the scrutinee during the arm tests.
        let scrut_slot = self.locals.len() as u32;
        self.locals.push((scrut_ty, scrut_mut));
        let branch_mode = mode;
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
        let (ty, mutable) = match mode {
            BlockMode::Discard => (UNIT, true),
            BlockMode::Value(t) => {
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (t, mutable)
            }
            BlockMode::Synth => {
                let ty = match hinted {
                    Some(hint) => self.join_branches(ctx, &branch_types).unwrap_or(hint),
                    None => self.join_branches(ctx, &branch_types)?,
                };
                let mutable = branch_types.iter().all(|(_, m, _)| *m);
                (ty, mutable)
            }
        };
        Ok(HExpr {
            flow: Flow::Normal,
            ty,
            mutable,
            kind: HExprKind::Case {
                scrut: Box::new(scrut_h),
                scrut_slot,
                arms: checked_arms,
            },
        })
    }

    /// Check one descriptor case with scoped generic parameters.
    fn check_reflect_case_value(
        &mut self,
        ctx: &mut Ctx,
        scrut: HExpr,
        arms: &[ast::CaseArm],
        mode: BlockMode,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if self.ctor.is_some() {
            return Err(Diagnostic::new(
                "E1026",
                "a reflection case is not valid during initialization",
                span,
            ));
        }
        let Some((fallback_arm, refined_arms)) = arms.split_last() else {
            return Err(Diagnostic::new(
                "E1049",
                "a reflection case needs a final wildcard arm",
                span,
            ));
        };
        if !matches!(fallback_arm.pattern.kind, PatternKind::Wildcard) {
            return Err(Diagnostic::new(
                "E1049",
                "a reflection case needs a final wildcard arm",
                fallback_arm.pattern.span,
            ));
        }
        if refined_arms.is_empty()
            || refined_arms
                .iter()
                .any(|arm| !matches!(arm.pattern.kind, PatternKind::Reflect { .. }))
        {
            return Err(Diagnostic::new(
                "E1049",
                "a reflection case accepts refinement arms and one wildcard arm",
                span,
            ));
        }

        self.scopes.push(Scope::default());
        let fallback_result = self.check_block(ctx, &fallback_arm.body, mode, fallback_arm.span);
        self.scopes.pop();
        let (fallback, fallback_ty, fallback_mutable) = fallback_result?;
        let result_ty = match mode {
            BlockMode::Discard => UNIT,
            BlockMode::Value(expected) => expected,
            BlockMode::Synth if fallback_ty != NEVER => fallback_ty,
            BlockMode::Synth => {
                return Err(Diagnostic::new(
                    "E1004",
                    "the wildcard arm must provide the reflection case type",
                    fallback_arm.span,
                ));
            }
        };
        let arm_mode = if mode == BlockMode::Discard {
            BlockMode::Discard
        } else {
            BlockMode::Value(result_ty)
        };
        let mut checked = Vec::with_capacity(refined_arms.len());
        let mut mutable = fallback_mutable;
        for arm in refined_arms {
            let PatternKind::Reflect {
                kind,
                generics,
                signature,
                binding,
            } = &arm.pattern.kind
            else {
                unreachable!("the reflection case shape was checked");
            };
            let (checked_arm, arm_mutable) = self.check_reflect_arm(
                ctx, scrut.ty, kind, generics, signature, binding, &arm.body, arm_mode, arm.span,
            )?;
            checked.push(checked_arm);
            mutable &= arm_mutable;
        }
        let scrut_slot = self.locals.len() as u32;
        self.locals.push((scrut.ty, scrut.mutable));
        Ok(HExpr {
            flow: Flow::Normal,
            ty: result_ty,
            mutable,
            kind: HExprKind::ReflectCase {
                scrut: Box::new(scrut),
                scrut_slot,
                arms: checked,
                fallback,
            },
        })
    }

    /// Check one reflection arm in the enclosing function.
    #[allow(clippy::too_many_arguments)]
    fn check_reflect_arm(
        &mut self,
        ctx: &mut Ctx,
        descriptor_ty: TypeId,
        kind_name: &str,
        generics: &[ast::GenericParam],
        signature: &Option<ast::TypeExpr>,
        binding: &ast::Pattern,
        body: &[ast::Expr],
        mode: BlockMode,
        span: Span,
    ) -> Result<(HReflectArm, bool), Diagnostic> {
        let descriptor_name = match kind_name {
            "Class" | "Def" | "Const" | "Method" | "Code" => "OpenCode",
            _ => {
                return Err(Diagnostic::new(
                    "E1041",
                    format!("unknown reflection pattern `{kind_name}`"),
                    span,
                ));
            }
        };
        let required_descriptor = Self::core_class(ctx, descriptor_name);
        if descriptor_ty != required_descriptor {
            return Err(self.mismatch(ctx, required_descriptor, descriptor_ty, span));
        }

        let parent_env = self.env.clone();
        let mut env = parent_env.clone();
        let first_type = env.type_bounds.len() as u32;
        let first_effect = env.effect_names.len() as u32;
        if generics.iter().filter(|generic| generic.is_effect).count() > 1 {
            return Err(Diagnostic::new(
                "E1041",
                "a refinement introduces at most one effect parameter",
                span,
            ));
        }
        for generic in generics {
            let names = if generic.is_effect {
                &mut env.effect_names
            } else {
                &mut env.type_names
            };
            if names.contains(&generic.name) {
                return Err(Diagnostic::new(
                    "E1014",
                    format!("duplicate generic parameter name `{}`", generic.name),
                    generic.span,
                ));
            }
            names.push(generic.name.clone());
            if !generic.is_effect {
                env.type_bounds.push(Vec::new());
            }
        }
        let fresh_bounds = resolve_generic_bounds_from(ctx, &env, generics, first_type)?;
        env.type_bounds[first_type as usize..].clone_from_slice(&fresh_bounds);
        let (kind, match_ty, binding_ty) = match signature {
            Some(signature) => {
                let kind = match kind_name {
                    "Class" => ReflectKind::Class,
                    "Def" => ReflectKind::Function,
                    "Method" => ReflectKind::Method,
                    "Const" => ReflectKind::Constant,
                    "Code" => ReflectKind::Code,
                    _ => unreachable!("the reflection kind was checked"),
                };
                let refined_ty = resolve_type(ctx, &env, signature)?;
                if kind != ReflectKind::Constant
                    && !matches!(ctx.store.get(refined_ty), Type::Fn(..))
                {
                    return Err(Diagnostic::new(
                        "E1004",
                        "a callable refinement needs a function type",
                        signature.span,
                    ));
                }
                let binding_ty = if kind == ReflectKind::Code {
                    Self::core_inst(ctx, "FunctionCode", vec![refined_ty])
                } else {
                    refined_ty
                };
                (kind, refined_ty, binding_ty)
            }
            None => {
                if kind_name != "Class" {
                    return Err(Diagnostic::new(
                        "E1041",
                        "only a class refinement can omit its callable signature",
                        span,
                    ));
                }
                if generics.len() != 1 || generics[0].is_effect {
                    return Err(Diagnostic::new(
                        "E1041",
                        "a class descriptor refinement needs one type parameter",
                        span,
                    ));
                }
                (
                    ReflectKind::ClassDescriptor,
                    ctx.store.intern(Type::Var(first_type)),
                    Self::core_class(ctx, "DeclarationCode"),
                )
            }
        };
        let binding_name = match &binding.kind {
            PatternKind::Name(name) if name != "_" => Some(name.clone()),
            PatternKind::Wildcard => None,
            _ => {
                return Err(Diagnostic::new(
                    "E1041",
                    "a reflection refinement binds one name or `_`",
                    binding.span,
                ));
            }
        };

        let pattern_name = format!("<reflection pattern {}>", ctx.funcs.len());
        let pattern = ctx.push_func(
            HirFunc {
                core: env.core_scope,
                imported: false,
                source_span: None,
                name: pattern_name,
                type_params: env.type_bounds.len() as u32,
                type_bounds: hir_bounds(&env.type_bounds),
                effect_params: env.effect_names.len() as u32,
                params: vec![match_ty],
                param_muts: vec![false],
                param_names: vec!["value".to_string()],
                ret: UNIT,
                row: Vec::new(),
                captures: Vec::new(),
                locals: vec![match_ty],
                body: Vec::new(),
            },
            FnSig {
                type_params: env.type_names.clone(),
                type_bounds: env.type_bounds.clone(),
                effect_params: env.effect_names.clone(),
                params: vec![match_ty],
                param_muts: vec![false],
                param_names: vec!["value".to_string()],
                ret: UNIT,
                row: Vec::new(),
            },
        );

        let binding_slot = binding_name.as_ref().map(|_| {
            let slot = self.locals.len() as u32;
            self.locals.push((binding_ty, false));
            slot
        });
        let mut scope = Scope::default();
        if let (Some(name), Some(slot)) = (binding_name, binding_slot) {
            scope.insert(name, slot);
        }
        self.env = env;
        self.scopes.push(scope);
        let body_result = self.check_block(ctx, body, mode, span);
        self.scopes.pop();
        self.env = parent_env;
        let (body, _, mutable) = body_result?;
        debug_assert_eq!(first_effect as usize, self.env.effect_names.len());
        Ok((
            HReflectArm {
                kind,
                pattern,
                type_base: first_type,
                effect_base: first_effect,
                binding: binding_slot,
                body,
            },
            mutable,
        ))
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
        self.scopes.push(Scope::default());
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
