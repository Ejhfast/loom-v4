//! Operator lowering through core hooks.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Build one direct call to a final primitive method.
    pub(super) fn primitive_operator(
        ctx: &Ctx,
        class_name: &str,
        name: &str,
        args: Vec<HExpr>,
    ) -> HExpr {
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
    pub(super) fn find_operator_hook(
        ctx: &mut Ctx,
        ty: TypeId,
        hook: &str,
    ) -> Option<OperatorHook> {
        let (class, class_args) = class_of(ctx, ty)?;
        let found = ctx.find_method_owner(class, hook)?;
        Some((class, class_args, found))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn operator_hook(
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

    pub(super) fn synth_binary(
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
                let r = if matches!(&right.kind, ExprKind::Name(name) if name == "None") {
                    self.check_expr(ctx, right, l.ty)?
                } else {
                    self.synth_expr(ctx, right)?
                };
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
                                ctx.display_type(&self.env, l.ty),
                                ctx.display_type(&self.env, r.ty)
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
                                ctx.display_type(&self.env, operand_ty),
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
                            ctx.display_type(&self.env, operand_ty),
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
}
