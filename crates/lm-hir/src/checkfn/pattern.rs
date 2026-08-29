//! Patterns and branch joins.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Check one pattern against a scrutinee type and bind its names.
    pub(super) fn check_pattern(
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
                let op = self.pattern_descriptor(ctx, &args[0])?;
                let args_ty = Self::op_args_type(ctx, op);
                let reply = ctx
                    .bundle
                    .op(op)
                    .expect("the operation slot resolves")
                    .reply;
                let reply_ty = Self::abi_type_id(ctx, reply);
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
                            ctx.display_type(&self.env, scrut_ty),
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
            PatternKind::Char(value) => {
                let char_ty = Self::core_class(ctx, "Char");
                if scrut_ty != char_ty {
                    return Err(self.pattern_mismatch(
                        ctx,
                        "a character literal",
                        scrut_ty,
                        pat.span,
                    ));
                }
                Ok(HPattern::Char(*value))
            }
            PatternKind::Str(v) => {
                if scrut_ty != STRING {
                    return Err(self.pattern_mismatch(ctx, "a string literal", scrut_ty, pat.span));
                }
                Ok(HPattern::Str(v.clone()))
            }
            PatternKind::Name(name) => {
                if ctx.constant_names.contains(name) {
                    return Err(Diagnostic::new(
                        "E1041",
                        format!(
                            "`{name}` is a constant and cannot bind a pattern. \
                             Write its literal value."
                        ),
                        pat.span,
                    ));
                }
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
                            ctx.display_type(&self.env, scrut_ty)
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
    pub(super) fn ctor_pattern(
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
    pub(super) fn pattern_descriptor(
        &self,
        ctx: &Ctx,
        pat: &ast::Pattern,
    ) -> Result<u32, Diagnostic> {
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
        ctx.bundle.op_by_name(&full).ok_or_else(|| {
            Diagnostic::new(
                "E1051",
                format!("`{full}` is not an operation of the manifest"),
                pat.span,
            )
        })
    }

    pub(super) fn pattern_mismatch(
        &self,
        ctx: &Ctx,
        what: &str,
        scrut_ty: TypeId,
        span: Span,
    ) -> Diagnostic {
        Diagnostic::new(
            "E1041",
            format!(
                "{what} pattern cannot match a scrutinee of type {}",
                ctx.display_type(&self.env, scrut_ty)
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
    pub(super) fn synth_join_elems(
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
    pub(super) fn join_branches(
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
