//! Fields, methods, super calls, indexing, and closures.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    pub(super) fn synth_field(
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
            return self.check_sys_value(ctx, &group, name, name_span);
        }
        // A bare `sys.<group>` is not a value.
        if matches!(recv.kind, ExprKind::Name(ref n) if n == "sys") && self.sys_in_scope()? {
            if Self::sys_group(ctx, name).is_some() {
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
                format!(
                    "the type {} has no fields",
                    ctx.display_type(&self.env, recv_h.ty)
                ),
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
    pub(super) fn synth_method_call(
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
        // `sys.args()` is the direct surface of `Args.Get`.
        if name == "args"
            && matches!(recv.kind, ExprKind::Name(ref value) if value == "sys")
            && self.sys_in_scope()?
        {
            return self.check_sys_call(ctx, "Args", "get", name_span, type_args, args, span);
        }
        // A direct operation call `sys.<group>.<Member>(args)`.
        if let Some(group) = self.sys_group_of(ctx, recv)? {
            return self.check_sys_call(ctx, &group, name, name_span, type_args, args, span);
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
        let code_control = match ctx.store.get(recv_ty) {
            Type::Class(class) => matches!(
                ctx.classes[class.0 as usize].native_repr,
                Some(
                    NativeRepr::Artifact
                        | NativeRepr::VerifiedModule
                        | NativeRepr::FunctionCode
                        | NativeRepr::ClassCode
                        | NativeRepr::SlotSpec
                        | NativeRepr::CodeInstance
                        | NativeRepr::Slot
                        | NativeRepr::ClassBinding
                )
            ),
            Type::Inst(class, _) => {
                matches!(
                    ctx.classes[class.0 as usize].native_repr,
                    Some(
                        NativeRepr::FunctionCode
                            | NativeRepr::FunctionDef
                            | NativeRepr::FunctionBinding
                    )
                )
            }
            _ => false,
        };
        if code_control
            || matches!(
                ctx.store.get(recv_ty),
                Type::Vm
                    | Type::Run(_)
                    | Type::Wait(_)
                    | Type::PolicyTable
                    | Type::Request
                    | Type::PendingCall(_, _)
                    | Type::Handle(_, _)
                    | Type::ResourceHandle
                    | Type::Fault
                    | Type::VmSnapshot
                    | Type::RunSnapshot(_)
            )
        {
            let out =
                self.check_control_method(ctx, recv_h, name, name_span, type_args, args, span)?;
            return Ok(out.expect("control receivers resolve or fail"));
        }
        // Text map queries accept every Text subtype.
        // The native path keeps one lookup probe.
        if let Type::Map(key, _) = ctx.store.get(recv_ty).clone() {
            let query = map_query_key_type(ctx, key);
            if query != key && matches!(name, "has" | "at" | "get") {
                if !type_args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1024",
                        "a map query does not take type arguments",
                        name_span,
                    ));
                }
                return self.check_native_method(ctx, recv_h, recv_ty, name, name_span, args, span);
            }
        }
        // Class and enum methods first, then the universal `freeze`.
        if let Some((class, class_args)) = class_of(ctx, recv_ty) {
            if let Some(found) = ctx.find_method_owner(class, name) {
                return self.check_declared_method(
                    ctx, recv_h, class, class_args, found, name, name_span, type_args, args,
                    expected, span,
                );
            }
            if name == "freeze"
                && ctx.store.is_heap(recv_ty)
                && args.is_empty()
                && type_args.is_empty()
            {
                return Ok(freeze_expr(recv_h));
            }
            if name == "digest"
                && ctx.store.is_heap(recv_ty)
                && args.is_empty()
                && type_args.is_empty()
            {
                return Ok(digest_expr(recv_h));
            }
            // A field can hold a function. `holder.step(1)` then reads
            // the field and calls that value. A class rejects a field
            // and a method of one name, so this name resolves once.
            if type_args.is_empty() {
                if let Some(fidx) = ctx.find_field(class, name) {
                    let declared = ctx.classes[class as usize].field_tys[fidx];
                    let field_ty = ctx.store.substitute(declared, &class_args, &[]);
                    if matches!(ctx.store.get(field_ty), Type::Fn(..) | Type::Callback(..)) {
                        let mutable = recv_h.mutable;
                        let callee = HExpr {
                            ty: field_ty,
                            mutable,
                            kind: HExprKind::FieldGet {
                                recv: Box::new(recv_h),
                                field: fidx as u32,
                            },
                        };
                        return self.synth_call_value(ctx, callee, args, name_span, span);
                    }
                }
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
        if let Some((interface, method, requirement)) =
            ctx.bound_method(&self.env, recv_ty, name, name_span)?
        {
            if !type_args.is_empty() {
                return Err(Diagnostic::new(
                    "E1024",
                    "an interface method does not take type arguments",
                    name_span,
                ));
            }
            if requirement.mut_self && !recv_h.mutable {
                return Err(Diagnostic::new(
                    "E1035",
                    format!("the method `{name}` needs a mutable receiver"),
                    name_span,
                ));
            }
            if requirement.mut_self {
                self.guard_iterated_mutation(&recv_h, name_span)?;
            }
            let checked = self.check_args_simple(
                ctx,
                args,
                &requirement.params,
                &requirement.param_muts,
                &requirement.param_names,
                name,
                span,
            )?;
            self.charge_row(ctx, &requirement.row, span)?;
            return Ok(HExpr {
                ty: requirement.ret,
                mutable: true,
                kind: HExprKind::InterfaceCall {
                    recv: Box::new(recv_h),
                    interface,
                    method,
                    selector: name.to_string(),
                    args: checked,
                },
            });
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
    pub(super) fn check_declared_method(
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
                if sig.mut_self {
                    self.guard_iterated_mutation(&recv_h, name_span)?;
                }
                if ctx.classes[class as usize].type_params.is_empty()
                    && owner_args.is_empty()
                    && sig.own_type_params.is_empty()
                    && sig.own_effect_params.is_empty()
                    && type_args.is_empty()
                {
                    let args = self.check_args_simple(
                        ctx,
                        args,
                        &sig.params,
                        &sig.param_muts,
                        &sig.param_names,
                        name,
                        span,
                    )?;
                    self.charge_row(ctx, &sig.row, span)?;
                    if ctx.classes[class as usize].is_final
                        || ctx.classes[class as usize].kind == ClassKind::EnumParent
                    {
                        let mut all_args = Vec::with_capacity(args.len() + 1);
                        all_args.push(recv_h);
                        all_args.extend(args);
                        return Ok(HExpr {
                            ty: sig.ret,
                            mutable: true,
                            kind: HExprKind::Call {
                                func: sig.func,
                                targs: Vec::new(),
                                rowargs: Vec::new(),
                                args: all_args,
                            },
                        });
                    }
                    return Ok(HExpr {
                        ty: sig.ret,
                        mutable: true,
                        kind: HExprKind::MethodCall {
                            recv: Box::new(recv_h),
                            selector: name.to_string(),
                            generic_owner: false,
                            own_targs: Vec::new(),
                            own_rowargs: Vec::new(),
                            args,
                        },
                    });
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
                    &{
                        let mut bounds = sig.class_type_bounds.clone();
                        bounds.extend(sig.own_type_bounds.clone());
                        bounds
                    },
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
                if ctx.classes[class as usize].is_final
                    || ctx.classes[class as usize].kind == ClassKind::EnumParent
                {
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
    pub(super) fn check_native_method(
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
            let def = ctx
                .bundle
                .op(op)
                .expect("the standard operation exists")
                .clone();
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
            (Type::Map(k, _), "has") => native(
                NativeOp::MapHas,
                vec![map_query_key_type(ctx, *k)],
                &["key"],
                BOOL,
                false,
            ),
            (Type::Map(k, v), "at") => native(
                NativeOp::MapAt,
                vec![map_query_key_type(ctx, *k)],
                &["key"],
                *v,
                false,
            ),
            (Type::Map(k, v), "put") => native(
                NativeOp::MapPut,
                vec![*k, *v],
                &["key", "value"],
                ctx.option_of(*v),
                true,
            ),
            (Type::Map(k, v), "get") => {
                let ret = ctx.option_of(*v);
                native(
                    NativeOp::MapGet,
                    vec![map_query_key_type(ctx, *k)],
                    &["key"],
                    ret,
                    false,
                )
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
                        ctx.display_type(&self.env, recv_ty)
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
        if needs_mut {
            self.guard_iterated_mutation(&recv_h, name_span)?;
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
    pub(super) fn check_fault_denied(
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
    pub(super) fn check_spawn(
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

    pub(super) fn synth_super_call(
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
        let (sig, owner_args, owner) = ctx
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
        if sig.mut_self {
            self.guard_iterated_mutation(&self_expr, name_span)?;
        }
        let class_names = ctx.classes[owner as usize].type_params.clone();
        let mut type_names = class_names.clone();
        type_names.extend(sig.own_type_params.iter().cloned());
        let mut pre_bound: Vec<Option<TypeId>> = owner_args.iter().map(|arg| Some(*arg)).collect();
        pre_bound.extend(vec![None; sig.own_type_params.len()]);
        let mut bounds = sig.class_type_bounds.clone();
        bounds.extend(sig.own_type_bounds.clone());
        let out = self.check_poly_call(
            ctx,
            name,
            span,
            &type_names,
            &bounds,
            sig.own_effect_params.len(),
            pre_bound,
            class_names.len(),
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
        let targs = out.targs;
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

    pub(super) fn synth_index(
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
                let key = self.check_expr(ctx, index, map_query_key_type(ctx, k))?;
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
                    ctx.display_type(&self.env, recv_h.ty)
                ),
                recv.span,
            )),
        }
    }

    /// Check `value is Type` and `value as Type`.
    pub(super) fn synth_is(
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
                    ctx.display_type(&self.env, v.ty)
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
                    ctx.display_type(&self.env, target)
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
                    ctx.display_type(&self.env, v.ty),
                    ctx.display_type(&self.env, target)
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
    pub(super) fn check_closure(
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
            ptys.push(resolve_param_type(ctx, &env, param)?);
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
            iterated_places: Vec::new(),
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
                source_span: (!env.core_scope).then_some(span),
                name,
                type_params: type_param_count,
                type_bounds: env
                    .type_bounds
                    .iter()
                    .map(|items| {
                        items
                            .iter()
                            .map(|application| HirInterfaceUse {
                                interface: application.interface,
                                types: application.type_args.clone(),
                                rows: application.row_args.clone(),
                            })
                            .collect()
                    })
                    .collect(),
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
                type_bounds: env.type_bounds.clone(),
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
}
