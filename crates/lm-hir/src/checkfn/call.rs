//! Call resolution, arguments, and polymorphic inference.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Check a call of a plain name: a local closure, a top-level
    /// function, a class constructor, an enum constructor, or a
    /// native builder constructor.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn call_named(
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
                    &sig.type_bounds,
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
                if !self.env.core_scope
                    && ctx.classes[class as usize].name == "SocketAddress"
                    && ctx.core_types.get("SocketAddress") == Some(&class)
                {
                    return Err(Diagnostic::new(
                        "E1026",
                        "use `Tcp().address` to construct a SocketAddress",
                        name_span,
                    ));
                }
                let opaque_syntax = [
                    "SyntaxTree",
                    "SyntaxElement",
                    "SyntaxNode",
                    "SyntaxToken",
                    "SyntaxTrivia",
                ]
                .iter()
                .any(|name| ctx.core_types.get(*name) == Some(&class));
                if !self.env.core_scope && opaque_syntax {
                    return Err(Diagnostic::new(
                        "E1026",
                        format!("`{name}` values cannot be constructed directly"),
                        name_span,
                    ));
                }
                if matches!(
                    ctx.classes[class as usize].native_repr,
                    Some(
                        NativeRepr::Text
                            | NativeRepr::Substring
                            | NativeRepr::Char
                            | NativeRepr::TcpResource
                            | NativeRepr::TcpStream
                            | NativeRepr::TcpListener
                            | NativeRepr::TlsStream
                            | NativeRepr::Artifact
                            | NativeRepr::VerifiedModule
                            | NativeRepr::SlotSpec
                            | NativeRepr::CodeInstance
                            | NativeRepr::Slot
                            | NativeRepr::FunctionDef
                            | NativeRepr::ClassDef
                            | NativeRepr::DynValue
                    )
                ) {
                    return Err(Diagnostic::new(
                        "E1026",
                        format!("`{name}` values cannot be constructed directly"),
                        name_span,
                    ));
                }
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
                let type_bounds = info.type_bounds.clone();
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
                    &type_bounds,
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
                if ctx.classes[class as usize].native_repr == Some(NativeRepr::Map) {
                    let key =
                        out.targs.first().copied().ok_or_else(|| {
                            Diagnostic::new("E1024", "Map needs a key type", span)
                        })?;
                    let key_span = type_args.first().map(|arg| arg.span).unwrap_or(name_span);
                    check_key_type(ctx, key, key_span)?;
                }
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
    pub(super) fn call_intrinsic(
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
        let vars: Vec<TypeId> = (0..self.env.type_names.len())
            .map(|index| {
                ctx.store
                    .intern(Type::Var(self.env.type_offset + index as u32))
            })
            .collect();
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|param| Self::abi_type_id_with_vars(ctx, *param, &vars))
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
            ty: Self::abi_type_id_with_vars(ctx, def.reply, &vars),
            mutable: true,
            kind: HExprKind::Intrinsic {
                intrinsic,
                args: checked,
            },
        })
    }

    /// Resolve one called name to its meaning.
    pub(super) fn resolve_callee(
        &mut self,
        ctx: &mut Ctx,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<Callee, Diagnostic> {
        if let Some(res) = self.resolve_name(name)? {
            if matches!(res, NameRes::Capture(_, ty, _) if ctx.store.contains_callback(ty)) {
                return Err(Diagnostic::new(
                    "E1064",
                    "a closure cannot capture a nonescaping callback",
                    name_span,
                ));
            }
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
    pub(super) fn synth_call_value(
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
            Type::Fn(params, muts, ret, row) | Type::Callback(params, muts, ret, row) => {
                (params.clone(), muts.clone(), *ret, row.clone())
            }
            _ => {
                return Err(Diagnostic::new(
                    "E1032",
                    format!(
                        "cannot call a value of type {}; it is not a closure",
                        ctx.display_type(&self.env, callee.ty)
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
    pub(super) fn check_args_simple<N: AsRef<str>>(
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
            if *is_mut {
                self.guard_iterated_mutation(&h, arg.span)?;
            }
            checked.push(h);
        }
        Ok(checked)
    }

    /// True when an argument can be synthesized without an expected
    /// type. This gates the inference pass; a wrong `true` degrades to
    /// an inner diagnostic that asks for explicit arguments.
    pub(super) fn can_synth(&self, ctx: &Ctx, expr: &ast::Expr) -> bool {
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
    pub(super) fn check_poly_call(
        &mut self,
        ctx: &mut Ctx,
        what: &str,
        span: Span,
        type_names: &[String],
        type_bounds: &[Vec<InterfaceUse>],
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
        if targs.iter().any(|ty| ctx.store.contains_callback(*ty)) {
            return Err(Diagnostic::new(
                "E1064",
                "a type argument cannot contain a nonescaping callback",
                span,
            ));
        }
        for (index, bounds) in type_bounds.iter().enumerate() {
            for bound in bounds {
                let Some(found) = ctx.type_conformance(&self.env, targs[index], bound.interface)
                else {
                    continue;
                };
                if bound.row_args.len() != found.row_args.len() {
                    continue;
                }
                for (declared, actual) in bound.row_args.iter().zip(&found.row_args) {
                    infer_bound_row(ctx, declared, actual, &mut rowargs);
                }
            }
        }
        let rowargs: Vec<Row> = rowargs.into_iter().map(|r| r.unwrap_or_default()).collect();
        for (index, bounds) in type_bounds.iter().enumerate() {
            for bound in bounds {
                let required = ctx.substitute_interface_use(bound, &targs, &rowargs);
                if !ctx.type_conforms(&self.env, targs[index], &required) {
                    return Err(Diagnostic::new(
                        "E1053",
                        format!(
                            "the type argument `{}` does not conform to `{}`",
                            ctx.display_type(&self.env, targs[index]),
                            ctx.interfaces[required.interface as usize].name
                        ),
                        span,
                    ));
                }
            }
        }
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
                Some(h) => self.expect_compatible(ctx, want, h, arg.span)?,
                None => self.check_expr(ctx, arg, want)?,
            };
            if *is_mut && !h.mutable {
                return Err(Diagnostic::new(
                    "E1035",
                    "a `mut` parameter needs a mutable value",
                    arg.span,
                ));
            }
            if *is_mut {
                self.guard_iterated_mutation(&h, arg.span)?;
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
    pub(super) fn partial_substitute(
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
    pub(super) fn enum_qualifier(&self, ctx: &Ctx, recv: &ast::Expr) -> Option<u32> {
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
}
