//! Expression synthesis and constructor resolution.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Synthesize an expression type.
    pub(super) fn synth_expr(
        &mut self,
        ctx: &mut Ctx,
        expr: &ast::Expr,
    ) -> Result<HExpr, Diagnostic> {
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
                    if matches!(res, NameRes::Capture(_, ty, _) if ctx.store.contains_callback(ty))
                    {
                        return Err(Diagnostic::new(
                            "E1064",
                            "a closure cannot capture a nonescaping callback",
                            expr.span,
                        ));
                    }
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
            ExprKind::Propagate(value) => self.check_propagate(ctx, value, expr.span),
            ExprKind::TupleLit(items) => {
                let mut checked = Vec::new();
                let mut tys = Vec::new();
                for item in items {
                    let h = self.synth_expr(ctx, item)?;
                    tys.push(h.ty);
                    checked.push(h);
                }
                let ty = ctx.store.intern(Type::Tuple(tys));
                if ctx.store.contains_callback(ty) {
                    return Err(Diagnostic::new(
                        "E1064",
                        "a tuple cannot store a nonescaping callback",
                        expr.span,
                    ));
                }
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
                if ctx.store.contains_callback(ty) {
                    return Err(Diagnostic::new(
                        "E1064",
                        "a list cannot store a nonescaping callback",
                        expr.span,
                    ));
                }
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
                if ctx.store.contains_callback(ty) {
                    return Err(Diagnostic::new(
                        "E1064",
                        "a map cannot store a nonescaping callback",
                        expr.span,
                    ));
                }
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

    /// Check postfix Result propagation.
    fn check_propagate(
        &mut self,
        ctx: &mut Ctx,
        value: &ast::Expr,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let value_h = self.synth_expr(ctx, value)?;
        let result_class = *ctx
            .core_types
            .get("Result")
            .expect("the core declares Result");
        let Type::Inst(found, arguments) = ctx.store.get(value_h.ty).clone() else {
            return Err(Diagnostic::new(
                "E1066",
                format!(
                    "`?` needs Result[T, E], found {}",
                    ctx.display_type(&self.env, value_h.ty)
                ),
                span,
            ));
        };
        if found.0 != result_class || arguments.len() != 2 {
            return Err(Diagnostic::new(
                "E1066",
                format!(
                    "`?` needs Result[T, E], found {}",
                    ctx.display_type(&self.env, value_h.ty)
                ),
                span,
            ));
        }
        let ok_ty = arguments[0];
        let error_ty = arguments[1];
        let ret = match self.ret {
            RetKind::Known(ret) => ret,
            RetKind::Entry => {
                return Err(Diagnostic::new(
                    "E1066",
                    "`?` is not valid at the top level of a module",
                    span,
                ));
            }
            RetKind::ClosureInfer => {
                return Err(Diagnostic::new(
                    "E1066",
                    "`?` needs a declared Result type on the closure",
                    span,
                ));
            }
        };
        let Type::Inst(ret_class, ret_arguments) = ctx.store.get(ret).clone() else {
            return Err(Diagnostic::new(
                "E1066",
                format!(
                    "`?` needs an enclosing Result return type, found {}",
                    ctx.display_type(&self.env, ret)
                ),
                span,
            ));
        };
        if ret_class.0 != result_class || ret_arguments.len() != 2 {
            return Err(Diagnostic::new(
                "E1066",
                format!(
                    "`?` needs an enclosing Result return type, found {}",
                    ctx.display_type(&self.env, ret)
                ),
                span,
            ));
        }
        if ret_arguments[1] != error_ty {
            return Err(Diagnostic::new(
                "E1066",
                format!(
                    "`?` cannot propagate {} through a Result error type of {}",
                    ctx.display_type(&self.env, error_ty),
                    ctx.display_type(&self.env, ret_arguments[1])
                ),
                span,
            ));
        }

        let value_name = "__loom_propagated_value".to_string();
        let error_name = "__loom_propagated_error".to_string();
        let binding = |name: String| ast::Pattern {
            kind: PatternKind::Name(name),
            span,
        };
        let constructor = |name: &str, argument: ast::Pattern| ast::Pattern {
            kind: PatternKind::Ctor {
                qualifier: Some("Result".to_string()),
                name: name.to_string(),
                args: vec![argument],
                has_parens: true,
            },
            span,
        };
        let ok_arm = ast::CaseArm {
            pattern: constructor("Ok", binding(value_name.clone())),
            body: vec![ast::Stmt {
                kind: StmtKind::Expr(ast::Expr {
                    kind: ExprKind::Name(value_name),
                    span,
                }),
                span,
            }],
            span,
        };
        let error_value = ast::Expr {
            kind: ExprKind::Name(error_name.clone()),
            span,
        };
        let err_arm = ast::CaseArm {
            pattern: constructor("Err", binding(error_name)),
            body: vec![ast::Stmt {
                kind: StmtKind::Return {
                    value: Some(ast::Expr {
                        kind: ExprKind::Call {
                            name: "Err".to_string(),
                            name_span: span,
                            type_args: Vec::new(),
                            args: vec![error_value],
                        },
                        span,
                    }),
                },
                span,
            }],
            span,
        };
        self.check_case_value(ctx, value_h, &[ok_arm, err_arm], Some(ok_ty), span)
    }

    /// Lower `select` to one wait and one case expression.
    pub(super) fn check_select(
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

    pub(super) fn synth_self(&mut self, ctx: &mut Ctx, span: Span) -> Result<HExpr, Diagnostic> {
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

    pub(super) fn synth_interp(
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
                    // Every Text form appends through the same builder
                    // path, so a Substring interpolates without a copy.
                    let text = Self::core_class(ctx, "Text");
                    let interpolable =
                        matches!(h.ty, INT | BOOL | STRING) || ctx.store.compatible(text, h.ty);
                    if !interpolable {
                        return Err(Diagnostic::new(
                            "E1034",
                            format!(
                                "cannot interpolate a value of type {}; this slice \
                                 interpolates Int, Bool, and Text",
                                ctx.display_type(&self.env, h.ty)
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
    pub(super) fn try_ctor_name(
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
    pub(super) fn resolve_ctor(
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
    pub(super) fn construct_arm(
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
        let type_bounds = info.type_bounds.clone();
        let ret = info.self_ty;
        let short = info.arm_short.clone();
        let muts = vec![false; field_tys.len()];
        let out = self.check_poly_call(
            ctx,
            &short,
            span,
            &type_names,
            &type_bounds,
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
        // A constructor builds a value of the enum, not of the one arm
        // it names. Narrowing to an arm is what a `case` does, so a
        // local that holds a constructor result still matches every
        // arm. An expected type already fixed the result, so only the
        // free position widens.
        let family = ctx.classes[arm as usize].family;
        let ty = match family {
            Some(parent) if expected.is_none() => {
                if out.targs.is_empty() {
                    ctx.store.intern(Type::Class(lm_types::ClassId(parent)))
                } else {
                    ctx.store
                        .intern(Type::Inst(lm_types::ClassId(parent), out.targs.clone()))
                }
            }
            _ => out.ret,
        };
        Ok(HExpr {
            ty,
            mutable: true,
            kind: HExprKind::Construct {
                class: arm,
                targs: out.targs,
                args: out.args,
            },
        })
    }
}
