//! The sys surface, ABI types, descriptors, and control methods.
//!
//! One part of the `FnChecker` surface. `checkfn/mod.rs` holds the
//! state and the free helpers these methods use.

use super::*;

impl<'o> FnChecker<'o> {
    /// Map a surface `sys` member name to its manifest group name.
    pub(super) fn sys_group(ctx: &Ctx, name: &str) -> Option<String> {
        sys_group_name(ctx, name)
    }

    /// True when the bare name `sys` means the ABI root object here.
    pub(super) fn sys_in_scope(&mut self) -> Result<bool, Diagnostic> {
        Ok(self.resolve_name("sys")?.is_none())
    }

    /// Resolve a name to its `use` binding. Locals, module functions,
    /// and module types shadow a `use` binding, per the resolution
    /// order.
    pub(super) fn use_binding(
        &mut self,
        ctx: &Ctx,
        name: &str,
    ) -> Result<Option<UseBinding>, Diagnostic> {
        if self.env.core_scope
            || self.resolve_name(name)?.is_some()
            || ctx.func_index.contains_key(name)
            || ctx.lookup_type(name, &self.env).is_some()
        {
            return Ok(None);
        }
        Ok(ctx.uses.get(name).cloned())
    }

    /// Read `sys.<group>` out of a receiver expression: the qualified
    /// form, or a `use`-bound group alias.
    pub(super) fn sys_group_of(
        &mut self,
        ctx: &Ctx,
        recv: &ast::Expr,
    ) -> Result<Option<String>, Diagnostic> {
        match &recv.kind {
            ExprKind::Field {
                recv: inner, name, ..
            } => {
                if matches!(inner.kind, ExprKind::Name(ref n) if n == "sys")
                    && self.sys_in_scope()?
                {
                    return Ok(Self::sys_group(ctx, name));
                }
            }
            ExprKind::Name(name) => {
                if let Some(UseBinding::SysGroup(group)) = self.use_binding(ctx, name)? {
                    return Ok(Some(group));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Convert one manifest type to a checker type.
    pub(super) fn abi_type_id(ctx: &mut Ctx, t: lm_abi::AbiType) -> TypeId {
        Self::abi_type_id_with_vars(ctx, t, &[])
    }

    /// Convert one manifest type with intrinsic generic parameters.
    pub(super) fn abi_type_id_with_vars(
        ctx: &mut Ctx,
        t: lm_abi::AbiType,
        vars: &[TypeId],
    ) -> TypeId {
        match t {
            lm_abi::AbiType::Primitive(primitive) => match primitive {
                lm_abi::AbiPrimitive::Unit => UNIT,
                lm_abi::AbiPrimitive::Never => lm_types::NEVER,
                lm_abi::AbiPrimitive::Bool => BOOL,
                lm_abi::AbiPrimitive::Int => INT,
                lm_abi::AbiPrimitive::Float => lm_types::FLOAT,
                lm_abi::AbiPrimitive::String => STRING,
                lm_abi::AbiPrimitive::Bytes => lm_types::BYTES,
                lm_abi::AbiPrimitive::VmSnapshot => lm_types::VM_SNAPSHOT,
                lm_abi::AbiPrimitive::Fault => lm_types::FAULT,
            },
            lm_abi::AbiType::Core(core) => {
                let name = match core {
                    lm_abi::AbiCore::Text => "Text",
                    lm_abi::AbiCore::Substring => "Substring",
                    lm_abi::AbiCore::Char => "Char",
                    lm_abi::AbiCore::StringBuilder => "StringBuilder",
                    lm_abi::AbiCore::ByteBuffer => "ByteBuffer",
                    lm_abi::AbiCore::OpenOptions => "OpenOptions",
                    lm_abi::AbiCore::SeekFrom => "SeekFrom",
                    lm_abi::AbiCore::FileKind => "FileKind",
                    lm_abi::AbiCore::FileInfo => "FileInfo",
                    lm_abi::AbiCore::DirEntry => "DirEntry",
                    lm_abi::AbiCore::RenameMode => "RenameMode",
                    lm_abi::AbiCore::IoError => "IoError",
                    lm_abi::AbiCore::FsError => "FsError",
                    lm_abi::AbiCore::EnvError => "EnvError",
                    lm_abi::AbiCore::EntropyError => "EntropyError",
                    lm_abi::AbiCore::SnapshotError => "SnapshotError",
                    lm_abi::AbiCore::IpAddress => "IpAddress",
                    lm_abi::AbiCore::SocketAddress => "SocketAddress",
                    lm_abi::AbiCore::NetError => "NetError",
                    lm_abi::AbiCore::TcpRead => "TcpRead",
                    lm_abi::AbiCore::Shutdown => "Shutdown",
                    lm_abi::AbiCore::TlsError => "TlsError",
                    lm_abi::AbiCore::Artifact => "Artifact",
                    lm_abi::AbiCore::CompileEnv => "CompileEnv",
                    lm_abi::AbiCore::CompileOptions => "CompileOptions",
                    lm_abi::AbiCore::CompileErrors => "CompileErrors",
                    lm_abi::AbiCore::DynValue => "DynValue",
                    lm_abi::AbiCore::SyntaxTree => "SyntaxTree",
                    lm_abi::AbiCore::SyntaxElement => "SyntaxElement",
                    lm_abi::AbiCore::SyntaxNode => "SyntaxNode",
                    lm_abi::AbiCore::SyntaxToken => "SyntaxToken",
                    lm_abi::AbiCore::SyntaxTrivia => "SyntaxTrivia",
                    lm_abi::AbiCore::SyntaxBuilder => "SyntaxBuilder",
                    lm_abi::AbiCore::SyntaxParse => "SyntaxParse",
                    lm_abi::AbiCore::StdStream => "StdStream",
                    lm_abi::AbiCore::TtySize => "TtySize",
                    lm_abi::AbiCore::TtyError => "TtyError",
                    lm_abi::AbiCore::SignalKind => "SignalKind",
                    lm_abi::AbiCore::SignalError => "SignalError",
                    lm_abi::AbiCore::PipeError => "PipeError",
                    lm_abi::AbiCore::ChildInput => "ChildInput",
                    lm_abi::AbiCore::ChildOutput => "ChildOutput",
                    lm_abi::AbiCore::ChildEnv => "ChildEnv",
                    lm_abi::AbiCore::ExecSpec => "ExecSpec",
                    lm_abi::AbiCore::ChildStatus => "ChildStatus",
                    lm_abi::AbiCore::ExecError => "ExecError",
                    lm_abi::AbiCore::UdpDatagram => "UdpDatagram",
                };
                Self::core_class(ctx, name)
            }
            lm_abi::AbiType::Native(native) => match native {
                lm_abi::AbiNative::FileHandle => lm_types::FILE_HANDLE,
                lm_abi::AbiNative::TcpResource => Self::core_class(ctx, "TcpResource"),
                lm_abi::AbiNative::TcpStream => Self::core_class(ctx, "TcpStream"),
                lm_abi::AbiNative::TcpListener => Self::core_class(ctx, "TcpListener"),
                lm_abi::AbiNative::TlsStream => Self::core_class(ctx, "TlsStream"),
                lm_abi::AbiNative::RawMode => Self::core_class(ctx, "RawMode"),
                lm_abi::AbiNative::SignalStream => Self::core_class(ctx, "SignalStream"),
                lm_abi::AbiNative::PipeEnd => Self::core_class(ctx, "PipeEnd"),
                lm_abi::AbiNative::PipeReader => Self::core_class(ctx, "PipeReader"),
                lm_abi::AbiNative::PipeWriter => Self::core_class(ctx, "PipeWriter"),
                lm_abi::AbiNative::Child => Self::core_class(ctx, "Child"),
                lm_abi::AbiNative::UdpSocket => Self::core_class(ctx, "UdpSocket"),
            },
            lm_abi::AbiType::Var(index) => vars
                .get(index as usize)
                .copied()
                .expect("the intrinsic generic parameter is in scope"),
            lm_abi::AbiType::List(element) => {
                let element = Self::abi_type_id_with_vars(ctx, *element, vars);
                ctx.store.intern(Type::List(element))
            }
            lm_abi::AbiType::Map(key, value) => {
                let key = Self::abi_type_id_with_vars(ctx, *key, vars);
                let value = Self::abi_type_id_with_vars(ctx, *value, vars);
                ctx.store.intern(Type::Map(key, value))
            }
            lm_abi::AbiType::Tuple(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| Self::abi_type_id_with_vars(ctx, *element, vars))
                    .collect();
                ctx.store.intern(Type::Tuple(elements))
            }
            lm_abi::AbiType::Apply(constructor, arguments) => {
                let arguments = arguments
                    .iter()
                    .map(|argument| Self::abi_type_id_with_vars(ctx, *argument, vars))
                    .collect();
                Self::core_inst(ctx, constructor.text(), arguments)
            }
            lm_abi::AbiType::Resource(_) => lm_types::HOST_RESOURCE,
        }
    }

    /// The callable function type of one fixed operation.
    pub(super) fn op_fn_type(ctx: &mut Ctx, op: u32) -> TypeId {
        let def = ctx
            .bundle
            .op(op)
            .expect("the operation slot resolves")
            .clone();
        debug_assert_eq!(def.kind, lm_abi::OpKind::Fixed);
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        let ret = Self::abi_type_id(ctx, def.reply);
        ctx.store
            .intern_fn(params.clone(), vec![false; params.len()], ret, vec![])
    }

    /// The argument-view type of one fixed operation: `()` for a
    /// zero-parameter operation, a tuple otherwise.
    pub(super) fn op_args_type(ctx: &mut Ctx, op: u32) -> TypeId {
        let def = ctx
            .bundle
            .op(op)
            .expect("the operation slot resolves")
            .clone();
        if def.params.is_empty() {
            return UNIT;
        }
        let elems: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        ctx.store.intern(Type::Tuple(elems))
    }

    /// Charge one exact operation to the enclosing row.
    pub(super) fn charge_op(
        &mut self,
        ctx: &mut Ctx,
        op: u32,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let name = ctx.bundle.op_name(op).expect("the operation slot resolves");
        let idx = ctx.store.intern_row_name(name);
        let row = vec![lm_types::RowElem::Op(idx)];
        self.charge_row(ctx, &row, span)
    }

    /// The mailbox message type of the enclosing proc class, when the
    /// method belongs to a subclass of the core class `Proc`.
    pub(super) fn proc_mailbox_type(&self, ctx: &Ctx) -> Option<TypeId> {
        let class = self.self_class?;
        let proc = *ctx.core_types.get("Proc")?;
        let arity = ctx.classes[class as usize].type_params.len();
        let own: Vec<TypeId> = (0..arity)
            .map(|i| {
                ctx.store
                    .find(&Type::Var(i as u32))
                    .expect("a class parameter type is interned")
            })
            .collect();
        let args =
            ctx.store
                .ancestor_args(lm_types::ClassId(class), &own, lm_types::ClassId(proc))?;
        args.first().copied()
    }

    /// An instance type of a core enum found by name.
    pub(super) fn core_inst(ctx: &mut Ctx, name: &str, args: Vec<TypeId>) -> TypeId {
        let class = ctx.core_types[name];
        ctx.store.intern(Type::Inst(lm_types::ClassId(class), args))
    }

    /// The instance type of a core enum without type parameters.
    pub(super) fn core_class(ctx: &mut Ctx, name: &str) -> TypeId {
        let class = ctx.core_types[name];
        ctx.store.intern(Type::Class(lm_types::ClassId(class)))
    }

    /// Reject arguments on a native method that takes none.
    pub(super) fn expect_no_args(
        name: &str,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<(), Diagnostic> {
        if args.is_empty() {
            return Ok(());
        }
        Err(Diagnostic::new(
            "E1006",
            format!("`{name}` expects 0 argument(s), found {}", args.len()),
            span,
        ))
    }

    /// Check a direct operation call `sys.<group>.<Member>(args)`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_sys_call(
        &mut self,
        ctx: &mut Ctx,
        group: &str,
        member: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if !type_args.is_empty() {
            return Err(Diagnostic::new(
                "E1024",
                "an operation call does not take type arguments",
                name_span,
            ));
        }
        // `sys.vm.Vm()` creates a persistent VM through `Vm.New`.
        if group == "Vm" && member == "Vm" {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    "`sys.vm.Vm` expects 0 argument(s)",
                    span,
                ));
            }
            self.charge_op(ctx, lm_abi::OP_VM_NEW, span)?;
            return Ok(HExpr {
                ty: lm_types::VM,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_NEW,
                    args: vec![],
                },
            });
        }
        if group == "Vm" && member == "artifact" {
            if args.len() != 1 {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.vm.artifact` expects 1 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            let bytes = self.check_expr(ctx, &args[0], lm_types::BYTES)?;
            self.charge_op(ctx, lm_abi::OP_VM_ARTIFACT, span)?;
            return Ok(HExpr {
                ty: Self::core_class(ctx, "Artifact"),
                mutable: false,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_ARTIFACT,
                    args: vec![bytes],
                },
            });
        }
        // `sys.vm.snapshot_self()` performs `Vm.SnapshotSelf`. The
        // calling function cannot name the enclosing machine result
        // type, so the reply is an untyped `VmSnapshot`.
        // (specification 17.1).
        if group == "Vm" && member == "snapshot_self" {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.vm.snapshot_self` expects 0 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_SELF, span)?;
            let error = Self::core_class(ctx, "SnapshotError");
            let ty = Self::core_inst(ctx, "Result", vec![lm_types::VM_SNAPSHOT, error]);
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_SNAPSHOT_SELF,
                    args: vec![],
                },
            });
        }
        if group == "Vm" && member == "load_snapshot" {
            if args.len() != 1 {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.vm.load_snapshot` expects 1 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            let bytes = self.check_expr(ctx, &args[0], lm_types::BYTES)?;
            self.charge_op(ctx, lm_abi::OP_VM_LOAD_SNAPSHOT, span)?;
            let error = Self::core_class(ctx, "SnapshotError");
            let ty = Self::core_inst(ctx, "Result", vec![lm_types::VM_SNAPSHOT, error]);
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_LOAD_SNAPSHOT,
                    args: vec![bytes],
                },
            });
        }
        if group == "Vm" && member == "restore_vm" {
            if args.len() != 1 {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.vm.restore_vm` expects 1 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            let snapshot = self.check_expr(ctx, &args[0], lm_types::VM_SNAPSHOT)?;
            self.charge_op(ctx, lm_abi::OP_VM_RESTORE_VM, span)?;
            let error = Self::core_class(ctx, "RestoreError");
            let ty = Self::core_inst(ctx, "Result", vec![lm_types::VM, error]);
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op: lm_abi::OP_VM_RESTORE_VM,
                    args: vec![snapshot],
                },
            });
        }
        // `sys.proc.recv()` performs `Proc.Recv`. The mailbox type
        // comes from the enclosing proc class, so the call is valid
        // only inside a method of a subclass of `Proc[M]`.
        if group == "Proc" && matches!(member, "recv" | "recv_wait") {
            if !args.is_empty() {
                return Err(Diagnostic::new(
                    "E1006",
                    format!(
                        "`sys.proc.{member}` expects 0 argument(s), found {}",
                        args.len()
                    ),
                    span,
                ));
            }
            let mailbox = self.proc_mailbox_type(ctx).ok_or_else(|| {
                Diagnostic::new(
                    "E1051",
                    format!(
                        "`sys.proc.{member}` is only valid inside a method of a `Proc` subclass"
                    ),
                    name_span,
                )
            })?;
            // The performing proc is the receiver. Its class fixes the
            // mailbox type, so the verifier reads the class table
            // instead of a claim at the call site.
            let receiver = self.synth_self(ctx, span)?;
            let recv = Self::core_inst(ctx, "Recv", vec![mailbox]);
            let (op, ty) = if member == "recv" {
                (lm_abi::OP_PROC_RECV, recv)
            } else {
                (
                    lm_abi::OP_PROC_RECV_WAIT,
                    ctx.store.intern(Type::Wait(recv)),
                )
            };
            self.charge_op(ctx, op, span)?;
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op,
                    args: vec![receiver],
                },
            });
        }
        // `sys.proc.run(value)` transfers one active run or launches
        // one nullary closure. Both forms use `M = Never`.
        if group == "Proc" && member == "run" {
            if args.len() != 1 {
                return Err(Diagnostic::new(
                    "E1006",
                    format!("`sys.proc.run` expects 1 argument(s), found {}", args.len()),
                    span,
                ));
            }
            let run = self.synth_expr(ctx, &args[0])?;
            let (op, result, row) = match ctx.store.get(run.ty).clone() {
                Type::Run(result) => (lm_abi::OP_PROC_RUN, result, Vec::new()),
                Type::Fn(params, _, result, row) if params.is_empty() => {
                    (lm_abi::OP_PROC_RUN_CLOSURE, result, row)
                }
                _ => {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`sys.proc.run` needs an active run or a nullary closure, found {}",
                            ctx.display_type(&self.env, run.ty)
                        ),
                        args[0].span,
                    ));
                }
            };
            self.charge_op(ctx, op, span)?;
            self.charge_row(ctx, &row, span)?;
            let ty = ctx.store.intern(Type::Handle(NEVER, result));
            return Ok(HExpr {
                ty,
                mutable: true,
                kind: HExprKind::Perform {
                    op,
                    args: vec![run],
                },
            });
        }
        let op = Self::resolve_sys_member(ctx, group, member, name_span)?;
        let def = ctx
            .bundle
            .op(op)
            .expect("the operation slot resolves")
            .clone();
        let params: Vec<TypeId> = def
            .params
            .iter()
            .map(|p| Self::abi_type_id(ctx, *p))
            .collect();
        let muts = vec![false; params.len()];
        let checked = self.check_args_simple(ctx, args, &params, &muts, NO_NAMES, member, span)?;
        self.charge_op(ctx, op, span)?;
        let ret = Self::abi_type_id(ctx, def.reply);
        Ok(HExpr {
            ty: ret,
            mutable: true,
            kind: HExprKind::Perform { op, args: checked },
        })
    }

    /// Resolve one surface member name inside a group to its fixed
    /// operation slot. The surface form is snake_case; a capitalized
    /// spelling of a real operation gets the casing rule.
    pub(super) fn resolve_sys_member(
        ctx: &Ctx,
        group: &str,
        member: &str,
        name_span: Span,
    ) -> Result<u32, Diagnostic> {
        let starts_upper = member
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false);
        if starts_upper {
            if ctx.bundle.fixed_member(group, member).is_some() {
                return Err(Diagnostic::new(
                    "E1051",
                    format!(
                        "callable `sys` members use snake_case; write `sys.{}.{}`",
                        group.to_ascii_lowercase(),
                        snake_member(member)
                    ),
                    name_span,
                ));
            }
            return Err(Diagnostic::new(
                "E1051",
                format!("the group `{group}` has no operation named `{member}`"),
                name_span,
            ));
        }
        ctx.bundle
            .fixed_member(group, &camel_member(member))
            .ok_or_else(|| {
                Diagnostic::new(
                    "E1051",
                    format!("the group `{group}` has no operation named `{member}`"),
                    name_span,
                )
            })
    }

    /// Check a first-class operation value `sys.<group>.<member>`.
    pub(super) fn check_sys_value(
        &mut self,
        ctx: &mut Ctx,
        group: &str,
        member: &str,
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        if group == "Vm" && member == "Vm" {
            return Err(Diagnostic::new(
                "E1051",
                "`sys.vm.Vm` is not a value; call `sys.vm.Vm()` to create a machine",
                span,
            ));
        }
        let op = Self::resolve_sys_member(ctx, group, member, span)?;
        let fn_ty = Self::op_fn_type(ctx, op);
        let ty = ctx.store.intern(Type::Op(op, fn_ty));
        Ok(HExpr {
            ty,
            mutable: true,
            kind: HExprKind::OpConst(op),
        })
    }

    /// Resolve a policy-target descriptor expression: a group name
    /// such as `Io`, or an exact name such as `Clock.Now`.
    pub(super) fn resolve_descriptor(
        &self,
        ctx: &Ctx,
        expr: &ast::Expr,
    ) -> Result<(TargetKind, u32, String), Diagnostic> {
        self.resolve_descriptor_for(ctx, expr, "a policy target")
    }

    /// Resolve a descriptor expression with a context word for the
    /// shape diagnostic.
    pub(super) fn resolve_descriptor_for(
        &self,
        ctx: &Ctx,
        expr: &ast::Expr,
        what: &str,
    ) -> Result<(TargetKind, u32, String), Diagnostic> {
        match &expr.kind {
            ExprKind::Name(name) => {
                if let Some(slot) = ctx.bundle.group_by_name(name) {
                    return Ok((TargetKind::Group, slot, name.clone()));
                }
                Err(Diagnostic::new(
                    "E1051",
                    format!("`{name}` is not a group in the operation manifest"),
                    expr.span,
                ))
            }
            ExprKind::Field { recv, name, .. } => {
                if let ExprKind::Name(group) = &recv.kind {
                    let full = format!("{group}.{name}");
                    if let Some(slot) = ctx.bundle.op_by_name(&full) {
                        return Ok((TargetKind::Exact, slot, full));
                    }
                    if let Some(slot) = ctx.bundle.group_by_name(&full) {
                        return Ok((TargetKind::Group, slot, full));
                    }
                    return Err(Diagnostic::new(
                        "E1051",
                        format!(
                            "`{full}` is not an operation or effect set in the operation manifest"
                        ),
                        expr.span,
                    ));
                }
                Err(Diagnostic::new(
                    "E1051",
                    format!("{what} must be a group name or an exact operation name"),
                    expr.span,
                ))
            }
            _ => Err(Diagnostic::new(
                "E1051",
                format!("{what} must be a group name or an exact operation name"),
                expr.span,
            )),
        }
    }

    /// Check a policy-table edit method.
    pub(super) fn check_table_edit(
        &mut self,
        ctx: &mut Ctx,
        table: HExpr,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let action = match name {
            "pass" => TableAction::Pass,
            "block" => TableAction::Block,
            "mock" => TableAction::Mock,
            "clear" => TableAction::Clear,
            _ => {
                return Err(Diagnostic::new(
                    "E1026",
                    format!("`PolicyTable` has no method named `{name}`"),
                    name_span,
                ));
            }
        };
        let want_args = if action == TableAction::Mock { 2 } else { 1 };
        if args.len() != want_args {
            return Err(Diagnostic::new(
                "E1006",
                format!(
                    "`{name}` expects {want_args} argument(s), found {}",
                    args.len()
                ),
                span,
            ));
        }
        let (kind, slot, target_name) = self.resolve_descriptor(ctx, &args[0])?;
        let mock = if action == TableAction::Mock {
            if kind != TargetKind::Exact
                || ctx
                    .bundle
                    .op(slot)
                    .is_none_or(|op| op.kind != lm_abi::OpKind::Fixed)
            {
                return Err(Diagnostic::new(
                    "E1051",
                    "`mock` needs an exact host operation, for example `Clock.Now`",
                    args[0].span,
                ));
            }
            let handler_ty = Self::op_fn_type(ctx, slot);
            let handler = self.check_expr(ctx, &args[1], handler_ty)?;
            Some(Box::new(handler))
        } else {
            None
        };
        if action == TableAction::Pass {
            // The dependent grant rule: passing authority is charged
            // to the granter's row.
            let idx = ctx.store.intern_row_name(&target_name);
            let row = vec![lm_types::RowElem::Op(idx)];
            self.charge_row(ctx, &row, span)?;
        }
        Ok(HExpr {
            ty: UNIT,
            mutable: true,
            kind: HExprKind::TableEdit {
                action,
                kind,
                slot,
                table: Box::new(table),
                mock,
            },
        })
    }

    /// Check the native methods of the VM control surface.
    /// This surface includes VM images, runs, and resource controls.
    /// Return `None` when the receiver type has no such method.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_control_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<Option<HExpr>, Diagnostic> {
        let recv_ty = ctx.store.get(recv_h.ty).clone();
        let code_class = match &recv_ty {
            Type::Class(class) => [
                "Artifact",
                "VerifiedModule",
                "ClassCode",
                "SlotSpec",
                "Instance",
                "Slot",
                "ClassDef",
                "ClassBinding",
                "CodeError",
            ]
            .into_iter()
            .find(|name| ctx.core_types.get(*name) == Some(&class.0)),
            Type::Inst(class, _) if ctx.core_types.get("FunctionCode") == Some(&class.0) => {
                Some("FunctionCode")
            }
            Type::Inst(class, _) if ctx.core_types.get("FunctionDef") == Some(&class.0) => {
                Some("FunctionDef")
            }
            Type::Inst(class, _) if ctx.core_types.get("FunctionBinding") == Some(&class.0) => {
                Some("FunctionBinding")
            }
            _ => None,
        };
        if !matches!(
            recv_ty,
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
        ) && code_class.is_none()
        {
            return Ok(None);
        }
        let code_takes_types = matches!(
            (code_class, name),
            (
                Some("Instance"),
                "entry" | "function" | "entry_binding" | "function_binding"
            ) | (Some("VerifiedModule"), "entry_code" | "function_code")
        );
        if !type_args.is_empty() && !code_takes_types {
            return Err(Diagnostic::new(
                "E1024",
                "a native control method does not take type arguments",
                name_span,
            ));
        }
        let out = if let Some(class) = code_class {
            self.check_code_method(ctx, recv_h, class, name, name_span, type_args, args, span)?
        } else {
            match recv_ty {
                Type::Vm | Type::Run(_) | Type::VmSnapshot | Type::RunSnapshot(_) => {
                    self.check_machine_method(ctx, recv_h, recv_ty, name, name_span, args, span)?
                }
                Type::Wait(_) => {
                    self.check_wait_method(ctx, recv_h, recv_ty, name, name_span, args, span)?
                }
                Type::Handle(_, _) => self
                    .check_proc_handle_method(ctx, recv_h, recv_ty, name, name_span, args, span)?,
                Type::ResourceHandle => self.check_resource_handle_method(
                    ctx, recv_h, recv_ty, name, name_span, args, span,
                )?,
                _ => self.check_value_control_method(
                    ctx, recv_h, recv_ty, name, name_span, args, span,
                )?,
            }
        };
        Ok(Some(out))
    }

    /// Check one method of an opaque code value.
    #[allow(clippy::too_many_arguments)]
    fn check_code_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        class: &str,
        name: &str,
        name_span: Span,
        type_args: &[ast::TypeExpr],
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        let code_error = Self::core_class(ctx, "CodeError");
        let result = |ctx: &mut Ctx, ok| Self::core_inst(ctx, "Result", vec![ok, code_error]);
        match (class, name) {
            ("FunctionCode", "source") | ("ClassCode", "source") => {
                Self::expect_no_args(name, args, span)?;
                let element = Self::core_class(ctx, "DefinitionSource");
                let ty = Self::core_inst(ctx, "Option", vec![element]);
                Ok(HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::CodeSource {
                        code: Box::new(recv_h),
                        element,
                    },
                })
            }
            ("FunctionCode", "definition") | ("ClassCode", "definition") => {
                Self::expect_no_args(name, args, span)?;
                Ok(HExpr {
                    ty: Self::core_class(ctx, "DefinitionSpec"),
                    mutable: true,
                    kind: HExprKind::CodeDefinition {
                        code: Box::new(recv_h),
                    },
                })
            }
            ("Artifact", "verify") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_COMPILER_VERIFY, span)?;
                let verified = Self::core_class(ctx, "VerifiedModule");
                Ok(HExpr {
                    ty: result(ctx, verified),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_COMPILER_VERIFY,
                        args: vec![recv_h],
                    },
                })
            }
            ("VerifiedModule", "entry_code") | ("VerifiedModule", "function_code") => {
                if type_args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`{name}` needs argument and result type arguments"),
                        name_span,
                    ));
                }
                let env = self.env.clone();
                let input = resolve_type(ctx, &env, &type_args[0])?;
                let output = resolve_type(ctx, &env, &type_args[1])?;
                if !matches!(ctx.store.get(input), Type::Unit | Type::Tuple(_)) {
                    return Err(Diagnostic::new(
                        "E1004",
                        "a function code argument view must be () or a tuple",
                        type_args[0].span,
                    ));
                }
                let function = Self::core_inst(ctx, "FunctionCode", vec![input, output]);
                let (op, values) = if name == "entry_code" {
                    Self::expect_no_args(name, args, span)?;
                    (lm_abi::OP_VM_MODULE_ENTRY_CODE, vec![recv_h])
                } else {
                    if args.len() != 1 {
                        return Err(Diagnostic::new(
                            "E1006",
                            format!("`function_code` expects 1 argument, found {}", args.len()),
                            span,
                        ));
                    }
                    let binding = self.check_expr(ctx, &args[0], STRING)?;
                    (lm_abi::OP_VM_MODULE_FUNCTION_CODE, vec![recv_h, binding])
                };
                self.charge_op(ctx, op, span)?;
                Ok(HExpr {
                    ty: result(ctx, function),
                    mutable: true,
                    kind: HExprKind::Perform { op, args: values },
                })
            }
            ("VerifiedModule", "class_code") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`class_code` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let binding = self.check_expr(ctx, &args[0], STRING)?;
                self.charge_op(ctx, lm_abi::OP_VM_MODULE_CLASS_CODE, span)?;
                let class_code = Self::core_class(ctx, "ClassCode");
                Ok(HExpr {
                    ty: result(ctx, class_code),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_MODULE_CLASS_CODE,
                        args: vec![recv_h, binding],
                    },
                })
            }
            ("Instance", "dynamic_entry") => {
                Self::expect_no_args(name, args, span)?;
                let dynamic = Self::core_class(ctx, "DynValue");
                let function = Self::core_inst(ctx, "FunctionDef", vec![UNIT, dynamic]);
                self.charge_op(ctx, lm_abi::OP_VM_INSTANCE_ENTRY, span)?;
                Ok(HExpr {
                    ty: result(ctx, function),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_INSTANCE_ENTRY,
                        args: vec![recv_h],
                    },
                })
            }
            ("Instance", "entry")
            | ("Instance", "function")
            | ("Instance", "entry_binding")
            | ("Instance", "function_binding") => {
                if type_args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1024",
                        format!("`{name}` needs argument and result type arguments"),
                        name_span,
                    ));
                }
                let env = self.env.clone();
                let input = resolve_type(ctx, &env, &type_args[0])?;
                let output = resolve_type(ctx, &env, &type_args[1])?;
                if !matches!(ctx.store.get(input), Type::Unit | Type::Tuple(_)) {
                    return Err(Diagnostic::new(
                        "E1004",
                        "a function definition argument view must be () or a tuple",
                        type_args[0].span,
                    ));
                }
                let is_binding = matches!(name, "entry_binding" | "function_binding");
                let class = if is_binding {
                    "FunctionBinding"
                } else {
                    "FunctionDef"
                };
                let function = Self::core_inst(ctx, class, vec![input, output]);
                let entry = matches!(name, "entry" | "entry_binding");
                let (op, values) = if entry {
                    Self::expect_no_args(name, args, span)?;
                    (
                        if is_binding {
                            lm_abi::OP_VM_INSTANCE_ENTRY_BINDING
                        } else {
                            lm_abi::OP_VM_INSTANCE_ENTRY
                        },
                        vec![recv_h],
                    )
                } else {
                    if args.len() != 1 {
                        return Err(Diagnostic::new(
                            "E1006",
                            format!("`function` expects 1 argument(s), found {}", args.len()),
                            span,
                        ));
                    }
                    let name_value = self.check_expr(ctx, &args[0], STRING)?;
                    (
                        if is_binding {
                            lm_abi::OP_VM_INSTANCE_FUNCTION_BINDING
                        } else {
                            lm_abi::OP_VM_INSTANCE_FUNCTION
                        },
                        vec![recv_h, name_value],
                    )
                };
                self.charge_op(ctx, op, span)?;
                Ok(HExpr {
                    ty: result(ctx, function),
                    mutable: true,
                    kind: HExprKind::Perform { op, args: values },
                })
            }
            ("Instance", "class_def") | ("Instance", "class_binding") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`class_def` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let name_value = self.check_expr(ctx, &args[0], STRING)?;
                let is_binding = name == "class_binding";
                let op = if is_binding {
                    lm_abi::OP_VM_INSTANCE_CLASS_BINDING
                } else {
                    lm_abi::OP_VM_INSTANCE_CLASS
                };
                self.charge_op(ctx, op, span)?;
                let class_def = Self::core_class(
                    ctx,
                    if is_binding {
                        "ClassBinding"
                    } else {
                        "ClassDef"
                    },
                );
                Ok(HExpr {
                    ty: result(ctx, class_def),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h, name_value],
                    },
                })
            }
            ("Instance", "slot_for") | ("Instance", "slot_spec") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`{name}` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let (argument, op, value) = if name == "slot_for" {
                    let slot_spec = Self::core_class(ctx, "SlotSpec");
                    (
                        self.check_expr(ctx, &args[0], slot_spec)?,
                        lm_abi::OP_VM_INSTANCE_SLOT_FOR,
                        Self::core_class(ctx, "Slot"),
                    )
                } else {
                    (
                        self.check_expr(ctx, &args[0], STRING)?,
                        lm_abi::OP_VM_INSTANCE_SLOT_SPEC,
                        Self::core_class(ctx, "SlotSpec"),
                    )
                };
                self.charge_op(ctx, op, span)?;
                Ok(HExpr {
                    ty: result(ctx, value),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h, argument],
                    },
                })
            }
            ("FunctionBinding", "slot")
            | ("ClassBinding", "slot")
            | ("FunctionBinding", "spec")
            | ("ClassBinding", "spec")
            | ("FunctionBinding", "instance")
            | ("ClassBinding", "instance") => {
                Self::expect_no_args(name, args, span)?;
                let (op, value) = match name {
                    "slot" => (lm_abi::OP_VM_BINDING_SLOT, Self::core_class(ctx, "Slot")),
                    "spec" => (
                        lm_abi::OP_VM_BINDING_SPEC,
                        Self::core_class(ctx, "SlotSpec"),
                    ),
                    _ => (
                        lm_abi::OP_VM_BINDING_INSTANCE,
                        Self::core_class(ctx, "Instance"),
                    ),
                };
                self.charge_op(ctx, op, span)?;
                Ok(HExpr {
                    ty: result(ctx, value),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                })
            }
            ("FunctionBinding", "target") => {
                Self::expect_no_args(name, args, span)?;
                let Type::Inst(_, values) = ctx.store.get(recv_h.ty).clone() else {
                    unreachable!("a function binding is generic")
                };
                let target = Self::core_inst(ctx, "FunctionDef", values);
                self.charge_op(ctx, lm_abi::OP_VM_BINDING_FUNCTION_TARGET, span)?;
                Ok(HExpr {
                    ty: result(ctx, target),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_BINDING_FUNCTION_TARGET,
                        args: vec![recv_h],
                    },
                })
            }
            ("ClassBinding", "target") => {
                Self::expect_no_args(name, args, span)?;
                let target = Self::core_class(ctx, "ClassDef");
                self.charge_op(ctx, lm_abi::OP_VM_BINDING_CLASS_TARGET, span)?;
                Ok(HExpr {
                    ty: result(ctx, target),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_BINDING_CLASS_TARGET,
                        args: vec![recv_h],
                    },
                })
            }
            _ => {
                Err(self.no_control_method(ctx, ctx.store.get(recv_h.ty).clone(), name, name_span))
            }
        }
    }

    /// The one diagnostic every control receiver states for a name it
    /// does not answer.
    fn no_control_method(
        &self,
        ctx: &mut Ctx,
        recv_ty: Type,
        name: &str,
        name_span: Span,
    ) -> Diagnostic {
        let id = ctx.store.intern(recv_ty);
        Diagnostic::new(
            "E1026",
            format!(
                "the type {} has no method named `{name}`",
                ctx.display_type(&self.env, id)
            ),
            name_span,
        )
    }

    /// One method of a VM image or an active run.
    #[allow(clippy::too_many_arguments)]
    fn check_machine_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: Type,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        Ok(match (recv_ty, name) {
            (kind @ Type::RunSnapshot(_), "to_bytes") | (kind @ Type::VmSnapshot, "to_bytes") => {
                Self::expect_no_args(name, args, span)?;
                let op = if matches!(kind, Type::RunSnapshot(_)) {
                    lm_abi::OP_VM_RUN_SNAPSHOT_BYTES
                } else {
                    lm_abi::OP_VM_SNAPSHOT_BYTES
                };
                self.charge_op(ctx, op, span)?;
                let error = Self::core_class(ctx, "SnapshotError");
                HExpr {
                    ty: Self::core_inst(ctx, "Result", vec![lm_types::BYTES, error]),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm, "activate" | "activate_or_fault") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`{name}` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                // The type of the second parameter comes from the
                // first, so this method arranges the labels itself
                // instead of calling `check_args_simple`.
                let args = arrange_args(args, &["program", "args"], name)?;
                let program = self.synth_expr(ctx, args[0])?;
                let (want, ret, op) = match ctx.store.get(program.ty).clone() {
                    Type::Fn(params, _, ret, _) => {
                        let view = if params.is_empty() {
                            UNIT
                        } else {
                            ctx.store.intern(Type::Tuple(params))
                        };
                        let op = if name == "activate_or_fault" {
                            lm_abi::OP_VM_ACTIVATE_OR_FAULT
                        } else {
                            lm_abi::OP_VM_ACTIVATE
                        };
                        (view, ret, op)
                    }
                    Type::Inst(class, values)
                        if ctx.core_types.get("FunctionDef") == Some(&class.0)
                            && name == "activate"
                            && values.len() == 2 =>
                    {
                        (values[0], values[1], lm_abi::OP_VM_ACTIVATE_DEF)
                    }
                    Type::Inst(class, values)
                        if ctx.core_types.get("FunctionBinding") == Some(&class.0)
                            && name == "activate"
                            && values.len() == 2 =>
                    {
                        (values[0], values[1], lm_abi::OP_VM_ACTIVATE_DEF)
                    }
                    _ => {
                        let expected = if name == "activate_or_fault" {
                            "a function"
                        } else {
                            "a function, function definition, or function binding"
                        };
                        return Err(Diagnostic::new(
                            "E1004",
                            format!(
                                "`{name}` needs {expected}, found {}",
                                ctx.display_type(&self.env, program.ty)
                            ),
                            args[0].span,
                        ));
                    }
                };
                let tuple = self.check_expr(ctx, args[1], want)?;
                self.charge_op(ctx, op, span)?;
                let run_ty = ctx.store.intern(Type::Run(ret));
                let ty = if op == lm_abi::OP_VM_ACTIVATE_OR_FAULT {
                    run_ty
                } else {
                    let error = Self::core_class(ctx, "CodeError");
                    Self::core_inst(ctx, "Result", vec![run_ty, error])
                };
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h, program, tuple],
                    },
                }
            }
            (Type::Vm, "install") => {
                if args.is_empty() || args.len() > 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`install` expects 1 or 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let code = self.synth_expr(ctx, &args[0])?;
                if let HExprKind::MakeClosure { func, captures } = &code.kind {
                    if captures.is_empty() {
                        ctx.reified_functions.insert(*func);
                    }
                }
                let installed = match ctx.store.get(code.ty).clone() {
                    Type::Class(class)
                        if ctx.core_types.get("VerifiedModule") == Some(&class.0) =>
                    {
                        Self::core_class(ctx, "Instance")
                    }
                    Type::Inst(class, values)
                        if ctx.core_types.get("FunctionCode") == Some(&class.0)
                            && values.len() == 2 =>
                    {
                        Self::core_inst(ctx, "FunctionBinding", values)
                    }
                    Type::Class(class) if ctx.core_types.get("ClassCode") == Some(&class.0) => {
                        Self::core_class(ctx, "ClassBinding")
                    }
                    Type::Fn(params, muts, ret, _) if !muts.iter().any(|marker| *marker) => {
                        let input = if params.is_empty() {
                            UNIT
                        } else {
                            ctx.store.intern(Type::Tuple(params))
                        };
                        Self::core_inst(ctx, "FunctionBinding", vec![input, ret])
                    }
                    Type::Fn(_, _, _, _) => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`install` cannot install a function with a mut parameter",
                            args[0].span,
                        ));
                    }
                    _ => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`install` needs verified module, function code, class code, or function",
                            args[0].span,
                        ));
                    }
                };
                let op = if args.len() == 2 {
                    lm_abi::OP_VM_INSTALL_WITH
                } else {
                    lm_abi::OP_VM_INSTALL
                };
                let mut values = vec![recv_h, code];
                if let Some(links) = args.get(1) {
                    let links_ty = Self::core_class(ctx, "LinkEnv");
                    values.push(self.check_expr(ctx, links, links_ty)?);
                }
                self.charge_op(ctx, op, span)?;
                let error = Self::core_class(ctx, "CodeError");
                let ty = Self::core_inst(ctx, "Result", vec![installed, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform { op, args: values },
                }
            }
            (Type::Vm, "replace")
            | (Type::Vm, "replace_function")
            | (Type::Vm, "replace_class")
            | (Type::Vm, "replace_value")
            | (Type::Vm, "replace_process")
            | (Type::Vm, "change")
            | (Type::Vm, "change_function")
            | (Type::Vm, "change_class")
            | (Type::Vm, "change_value")
            | (Type::Vm, "change_process") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`replace` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let address = self.synth_expr(ctx, &args[0])?;
                let address_kind = match ctx.store.get(address.ty) {
                    Type::Class(class) if ctx.core_types.get("Slot") == Some(&class.0) => 0,
                    Type::Inst(class, values)
                        if ctx.core_types.get("FunctionBinding") == Some(&class.0)
                            && values.len() == 2 =>
                    {
                        1
                    }
                    Type::Class(class) if ctx.core_types.get("ClassBinding") == Some(&class.0) => 2,
                    _ => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`replace` needs a Slot or installed binding",
                            args[0].span,
                        ));
                    }
                };
                let target = self.synth_expr(ctx, &args[1])?;
                let is_function = matches!(
                    ctx.store.get(target.ty),
                    Type::Inst(class, values)
                        if (ctx.core_types.get("FunctionDef") == Some(&class.0)
                            || ctx.core_types.get("FunctionBinding") == Some(&class.0))
                            && values.len() == 2
                ) || matches!(ctx.store.get(target.ty), Type::Fn(_, muts, _, _) if !muts.iter().any(|marker| *marker));
                let is_class = matches!(
                    ctx.store.get(target.ty),
                    Type::Class(class)
                        if ctx.core_types.get("ClassDef") == Some(&class.0)
                            || ctx.core_types.get("ClassBinding") == Some(&class.0)
                );
                let is_process = matches!(ctx.store.get(target.ty), Type::Handle(_, _));
                let change = name.starts_with("change");
                let op = match name {
                    "replace" | "replace_function" | "change" | "change_function"
                        if is_function && address_kind != 2 =>
                    {
                        if change {
                            lm_abi::OP_VM_CHANGE_FUNCTION
                        } else {
                            lm_abi::OP_VM_REPLACE_FUNCTION
                        }
                    }
                    "replace" | "replace_class" | "change" | "change_class"
                        if is_class && address_kind != 1 =>
                    {
                        if change {
                            lm_abi::OP_VM_CHANGE_CLASS
                        } else {
                            lm_abi::OP_VM_REPLACE_CLASS
                        }
                    }
                    "replace_value" | "change_value" if address_kind == 0 => {
                        if change {
                            lm_abi::OP_VM_CHANGE_VALUE
                        } else {
                            lm_abi::OP_VM_REPLACE_VALUE
                        }
                    }
                    "replace_process" | "change_process" if is_process && address_kind == 0 => {
                        if change {
                            lm_abi::OP_VM_CHANGE_PROCESS
                        } else {
                            lm_abi::OP_VM_REPLACE_PROCESS
                        }
                    }
                    "replace" | "replace_function" | "change" | "change_function" => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`replace_function` needs a function target",
                            args[1].span,
                        ));
                    }
                    "replace_class" | "change_class" => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`replace_class` needs a class target",
                            args[1].span,
                        ));
                    }
                    _ => {
                        return Err(Diagnostic::new(
                            "E1004",
                            "`replace_process` needs a process handle target",
                            args[1].span,
                        ));
                    }
                };
                self.charge_op(ctx, op, span)?;
                let error = Self::core_class(ctx, "CodeError");
                let success = if change {
                    Self::core_class(ctx, "SlotChange")
                } else {
                    UNIT
                };
                let ty = Self::core_inst(ctx, "Result", vec![success, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h, address, target],
                    },
                }
            }
            (Type::Vm, "replace_all") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`replace_all` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let change = Self::core_class(ctx, "SlotChange");
                let changes = ctx.store.intern(Type::List(change));
                let changes = self.check_expr(ctx, &args[0], changes)?;
                self.charge_op(ctx, lm_abi::OP_VM_REPLACE_ALL, span)?;
                let error = Self::core_class(ctx, "CodeError");
                HExpr {
                    ty: Self::core_inst(ctx, "Result", vec![UNIT, error]),
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_REPLACE_ALL,
                        args: vec![recv_h, changes],
                    },
                }
            }
            (Type::Vm, "snapshot") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_VM, span)?;
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![lm_types::VM_SNAPSHOT, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SNAPSHOT_VM,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(t), "snapshot_wait") => {
                // The held form. `Handle[M,R].snapshot_wait` waits on a
                // scheduler proc; this one advances a machine the
                // caller holds.
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`snapshot_wait` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let fuel = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_WAIT_HELD, span)?;
                let snapshot = ctx.store.intern(Type::RunSnapshot(t));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SNAPSHOT_WAIT_HELD,
                        args: vec![recv_h, fuel],
                    },
                }
            }
            (Type::Run(t), "drive_for") => {
                // A bounded drive turn. `None` reports that the turn
                // spent its instructions and the machine can run again.
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`drive_for` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let count = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_VM_DRIVE_FOR, span)?;
                let event = Self::core_inst(ctx, "DriveEvent", vec![t]);
                let ty = Self::core_inst(ctx, "Option", vec![event]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DRIVE_FOR,
                        args: vec![recv_h, count],
                    },
                }
            }
            (Type::Run(t), "run") | (Type::Run(t), "step") | (Type::Run(t), "drive") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`{name}` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let op = match name {
                    "run" => lm_abi::OP_VM_RUN,
                    "step" => lm_abi::OP_VM_STEP,
                    _ => lm_abi::OP_VM_DRIVE,
                };
                self.charge_op(ctx, op, span)?;
                let ty = if name == "run" {
                    Self::core_inst(ctx, "Result", vec![t, lm_types::FAULT])
                } else {
                    let event = if name == "step" {
                        "StepEvent"
                    } else {
                        "DriveEvent"
                    };
                    Self::core_inst(ctx, event, vec![t])
                };
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(t), "drive_wait") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_DRIVE_WAIT, span)?;
                let event = Self::core_inst(ctx, "DriveEvent", vec![t]);
                let ty = ctx.store.intern(Type::Wait(event));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DRIVE_WAIT,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(t), "snapshot") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_SNAPSHOT_HELD, span)?;
                let snapshot = ctx.store.intern(Type::RunSnapshot(t));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SNAPSHOT_HELD,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(t), "branch") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_BRANCH, span)?;
                let run = ctx.store.intern(Type::Run(t));
                let error = Self::core_class(ctx, "BranchError");
                let ty = Self::core_inst(ctx, "Result", vec![run, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_BRANCH,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Vm, "restore") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`restore` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let snapshot = self.synth_expr(ctx, &args[0])?;
                let Type::RunSnapshot(t) = ctx.store.get(snapshot.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`restore` needs a typed snapshot, found {}",
                            ctx.display_type(&self.env, snapshot.ty)
                        ),
                        args[0].span,
                    ));
                };
                self.charge_op(ctx, lm_abi::OP_VM_RESTORE, span)?;
                let run = ctx.store.intern(Type::Run(t));
                let error = Self::core_class(ctx, "RestoreError");
                let ty = Self::core_inst(ctx, "Result", vec![run, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESTORE,
                        args: vec![recv_h, snapshot],
                    },
                }
            }
            (Type::Run(_), "table") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`table` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_TABLE, span)?;
                HExpr {
                    ty: lm_types::POLICY_TABLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_TABLE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(_), "handles") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_HANDLES, span)?;
                let ty = ctx.store.intern(Type::List(lm_types::RESOURCE_HANDLE));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_HANDLES,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Run(_), "resource") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`resource` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let handle = self.synth_expr(ctx, &args[0])?;
                let tcp_resource = Self::core_class(ctx, "TcpResource");
                let tls_stream = Self::core_class(ctx, "TlsStream");
                if handle.ty != lm_types::FILE_HANDLE
                    && !ctx.store.compatible(tcp_resource, handle.ty)
                    && handle.ty != tls_stream
                {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`resource` needs a file or stream resource, found {}",
                            ctx.display_type(&self.env, handle.ty)
                        ),
                        args[0].span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE,
                        args: vec![recv_h, handle],
                    },
                }
            }
            (Type::Run(_), "serve_file") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`serve_file` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let call = self.synth_expr(ctx, &args[0])?;
                let want_args = Self::op_args_type(ctx, lm_abi::OP_FS_OPEN);
                let reply = ctx
                    .bundle
                    .op(lm_abi::OP_FS_OPEN)
                    .expect("the standard operation exists")
                    .reply;
                let want_reply = Self::abi_type_id(ctx, reply);
                if ctx.store.get(call.ty) != &Type::PendingCall(want_args, want_reply) {
                    return Err(Diagnostic::new(
                        "E1004",
                        "`serve_file` needs a current Fs.Open call",
                        args[0].span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_SERVE_FILE, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SERVE_FILE,
                        args: vec![recv_h, call],
                    },
                }
            }
            (Type::Run(_), "serve_tcp_stream") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "`serve_tcp_stream` expects 2 argument(s), found {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let call = self.synth_expr(ctx, &args[0])?;
                let connect_args = Self::op_args_type(ctx, lm_abi::OP_TCP_CONNECT);
                let connect_reply = ctx
                    .bundle
                    .op(lm_abi::OP_TCP_CONNECT)
                    .expect("the standard operation exists")
                    .reply;
                let connect_reply = Self::abi_type_id(ctx, connect_reply);
                let accept_args = Self::op_args_type(ctx, lm_abi::OP_TCP_ACCEPT);
                let accept_reply = ctx
                    .bundle
                    .op(lm_abi::OP_TCP_ACCEPT)
                    .expect("the standard operation exists")
                    .reply;
                let accept_reply = Self::abi_type_id(ctx, accept_reply);
                let valid = ctx.store.get(call.ty)
                    == &Type::PendingCall(connect_args, connect_reply)
                    || ctx.store.get(call.ty) == &Type::PendingCall(accept_args, accept_reply);
                if !valid {
                    return Err(Diagnostic::new(
                        "E1004",
                        "`serve_tcp_stream` needs a current Tcp.Connect or Tcp.Accept call",
                        args[0].span,
                    ));
                }
                let address = Self::core_class(ctx, "SocketAddress");
                let peer = self.check_expr(ctx, &args[1], address)?;
                self.charge_op(ctx, lm_abi::OP_VM_SERVE_TCP_STREAM, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SERVE_TCP_STREAM,
                        args: vec![recv_h, call, peer],
                    },
                }
            }
            (Type::Run(_), "serve_tcp_listener") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "`serve_tcp_listener` expects 1 argument(s), found {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let call = self.synth_expr(ctx, &args[0])?;
                let want_args = Self::op_args_type(ctx, lm_abi::OP_TCP_LISTEN);
                let reply = ctx
                    .bundle
                    .op(lm_abi::OP_TCP_LISTEN)
                    .expect("the standard operation exists")
                    .reply;
                let want_reply = Self::abi_type_id(ctx, reply);
                if ctx.store.get(call.ty) != &Type::PendingCall(want_args, want_reply) {
                    return Err(Diagnostic::new(
                        "E1004",
                        "`serve_tcp_listener` needs a current Tcp.Listen call",
                        args[0].span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_SERVE_TCP_LISTENER, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SERVE_TCP_LISTENER,
                        args: vec![recv_h, call],
                    },
                }
            }
            (Type::Run(_), "serve_tls_stream") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "`serve_tls_stream` expects 1 argument(s), found {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let call = self.synth_expr(ctx, &args[0])?;
                let client_args = Self::op_args_type(ctx, lm_abi::OP_TLS_HANDSHAKE);
                let client_reply = ctx
                    .bundle
                    .op(lm_abi::OP_TLS_HANDSHAKE)
                    .expect("the standard operation exists")
                    .reply;
                let client_reply = Self::abi_type_id(ctx, client_reply);
                let server_args = Self::op_args_type(ctx, lm_abi::OP_TLS_SERVER_HANDSHAKE);
                let server_reply = ctx
                    .bundle
                    .op(lm_abi::OP_TLS_SERVER_HANDSHAKE)
                    .expect("the standard operation exists")
                    .reply;
                let server_reply = Self::abi_type_id(ctx, server_reply);
                let found = ctx.store.get(call.ty);
                if found != &Type::PendingCall(client_args, client_reply)
                    && found != &Type::PendingCall(server_args, server_reply)
                {
                    return Err(Diagnostic::new(
                        "E1004",
                        "`serve_tls_stream` needs a current TLS handshake call",
                        args[0].span,
                    ));
                }
                self.charge_op(ctx, lm_abi::OP_VM_SERVE_TLS_STREAM, span)?;
                HExpr {
                    ty: lm_types::RESOURCE_HANDLE,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_SERVE_TLS_STREAM,
                        args: vec![recv_h, call],
                    },
                }
            }
            (Type::Run(_), "answer") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`answer` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                // The reply type comes from the call token, so this
                // method arranges the labels itself.
                let args = arrange_args(args, &["call", "value"], "answer")?;
                let call = self.synth_expr(ctx, args[0])?;
                let Type::PendingCall(_, reply) = ctx.store.get(call.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`answer` needs a PendingCall token, found {}",
                            ctx.display_type(&self.env, call.ty)
                        ),
                        args[0].span,
                    ));
                };
                let value = self.check_expr(ctx, args[1], reply)?;
                self.charge_op(ctx, lm_abi::OP_VM_ANSWER, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_ANSWER,
                        args: vec![recv_h, call, value],
                    },
                }
            }
            (Type::Run(_), "reject") => {
                if args.len() != 2 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`reject` expects 2 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let args = arrange_args(args, &["request", "fault"], "reject")?;
                let request = self.check_expr(ctx, args[0], lm_types::REQUEST)?;
                let fault = self.check_expr(ctx, args[1], lm_types::FAULT)?;
                self.charge_op(ctx, lm_abi::OP_VM_REJECT, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_REJECT,
                        args: vec![recv_h, request, fault],
                    },
                }
            }
            (Type::Run(_), "dispatch") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`dispatch` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let request = self.check_expr(ctx, &args[0], lm_types::REQUEST)?;
                self.charge_op(ctx, lm_abi::OP_VM_DISPATCH, span)?;
                HExpr {
                    ty: UNIT,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_DISPATCH,
                        args: vec![recv_h, request],
                    },
                }
            }
            (recv_ty, _) => return Err(self.no_control_method(ctx, recv_ty, name, name_span)),
        })
    }

    /// One method of a selectable wait.
    #[allow(clippy::too_many_arguments)]
    fn check_wait_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: Type,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        Ok(match (recv_ty, name) {
            (Type::Wait(t), "wait") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_WAIT_WAIT, span)?;
                HExpr {
                    ty: t,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_WAIT,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Wait(left), "choose") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`choose` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let right = self.synth_expr(ctx, &args[0])?;
                let Type::Wait(right_result) = ctx.store.get(right.ty).clone() else {
                    return Err(Diagnostic::new(
                        "E1004",
                        format!(
                            "`choose` needs a wait, found {}",
                            ctx.display_type(&self.env, right.ty)
                        ),
                        args[0].span,
                    ));
                };
                self.charge_op(ctx, lm_abi::OP_WAIT_CHOOSE, span)?;
                let choice = Self::core_inst(ctx, "Choice", vec![left, right_result]);
                let ty = ctx.store.intern(Type::Wait(choice));
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_CHOOSE,
                        args: vec![recv_h, right],
                    },
                }
            }
            (Type::Wait(_), "cancel") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_WAIT_CANCEL, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_WAIT_CANCEL,
                        args: vec![recv_h],
                    },
                }
            }
            (recv_ty, _) => return Err(self.no_control_method(ctx, recv_ty, name, name_span)),
        })
    }

    /// One method of a proc handle.
    #[allow(clippy::too_many_arguments)]
    fn check_proc_handle_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: Type,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        Ok(match (recv_ty, name) {
            (Type::Handle(m, _), "send") => {
                if m == NEVER {
                    return Err(Diagnostic::new(
                        "E1026",
                        "a proc with no mailbox has no `send` method",
                        name_span,
                    ));
                }
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`send` expects 1 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                let message = self.check_expr(ctx, &args[0], m)?;
                self.charge_op(ctx, lm_abi::OP_PROC_SEND, span)?;
                let ty = Self::core_class(ctx, "SendResult");
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_SEND,
                        args: vec![recv_h, message],
                    },
                }
            }
            (Type::Handle(_, _), "close") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_PROC_CLOSE, span)?;
                let ty = Self::core_class(ctx, "SendResult");
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_CLOSE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Handle(_, r), "done") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_PROC_DONE, span)?;
                let ty = Self::core_inst(ctx, "Result", vec![r, lm_types::FAULT]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_DONE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::Handle(_, r), "snapshot_wait") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`snapshot_wait` expects 1 argument, found {}", args.len()),
                        span,
                    ));
                }
                let fuel = self.check_expr(ctx, &args[0], INT)?;
                self.charge_op(ctx, lm_abi::OP_PROC_SNAPSHOT_WAIT, span)?;
                let snapshot = ctx.store.intern(Type::RunSnapshot(r));
                let error = Self::core_class(ctx, "SnapshotError");
                let ty = Self::core_inst(ctx, "Result", vec![snapshot, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_PROC_SNAPSHOT_WAIT,
                        args: vec![recv_h, fuel],
                    },
                }
            }
            (Type::Handle(_, r), "pause") | (Type::Handle(_, r), "resume") => {
                Self::expect_no_args(name, args, span)?;
                let (op, ok) = if name == "pause" {
                    (lm_abi::OP_PROC_PAUSE, ctx.store.intern(Type::Run(r)))
                } else {
                    (lm_abi::OP_PROC_RESUME, UNIT)
                };
                self.charge_op(ctx, op, span)?;
                let error = Self::core_class(ctx, "ProcError");
                let ty = Self::core_inst(ctx, "Result", vec![ok, error]);
                HExpr {
                    ty,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op,
                        args: vec![recv_h],
                    },
                }
            }
            (recv_ty, _) => return Err(self.no_control_method(ctx, recv_ty, name, name_span)),
        })
    }

    /// One method of a resource control.
    #[allow(clippy::too_many_arguments)]
    fn check_resource_handle_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: Type,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        Ok(match (recv_ty, name) {
            (Type::ResourceHandle, "is_open") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_IS_OPEN, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_IS_OPEN,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "close") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_CLOSE, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_CLOSE,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "kind") => {
                Self::expect_no_args(name, args, span)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_KIND, span)?;
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_KIND,
                        args: vec![recv_h],
                    },
                }
            }
            (Type::ResourceHandle, "same_resource") => {
                if args.len() != 1 {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!(
                            "`same_resource` expects 1 argument(s), found {}",
                            args.len()
                        ),
                        span,
                    ));
                }
                let other = self.check_expr(ctx, &args[0], lm_types::RESOURCE_HANDLE)?;
                self.charge_op(ctx, lm_abi::OP_VM_RESOURCE_SAME, span)?;
                HExpr {
                    ty: BOOL,
                    mutable: true,
                    kind: HExprKind::Perform {
                        op: lm_abi::OP_VM_RESOURCE_SAME,
                        args: vec![recv_h, other],
                    },
                }
            }
            // The erased inspection surface of a request. A wildcard
            // arm holds no operation identity, so this names the
            // operation as text for a report or a denial message. The
            // request must still be live: a continuation spends it.
            (recv_ty, _) => return Err(self.no_control_method(ctx, recv_ty, name, name_span)),
        })
    }

    /// One method of a policy table, a call token, a request, or a fault.
    #[allow(clippy::too_many_arguments)]
    fn check_value_control_method(
        &mut self,
        ctx: &mut Ctx,
        recv_h: HExpr,
        recv_ty: Type,
        name: &str,
        name_span: Span,
        args: &[ast::Expr],
        span: Span,
    ) -> Result<HExpr, Diagnostic> {
        Ok(match (recv_ty, name) {
            (Type::PolicyTable, _) => {
                return self.check_table_edit(ctx, recv_h, name, name_span, args, span);
            }
            (Type::PendingCall(a, _), "args") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`args` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                HExpr {
                    ty: a,
                    mutable: true,
                    kind: HExprKind::CallArgs {
                        call: Box::new(recv_h),
                    },
                }
            }
            (Type::Request, "op_name") => {
                Self::expect_no_args(name, args, span)?;
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::RequestOpName {
                        request: Box::new(recv_h),
                    },
                }
            }
            (Type::Fault, "code") => {
                if !args.is_empty() {
                    return Err(Diagnostic::new(
                        "E1006",
                        format!("`code` expects 0 argument(s), found {}", args.len()),
                        span,
                    ));
                }
                HExpr {
                    ty: STRING,
                    mutable: true,
                    kind: HExprKind::FaultCodeGet {
                        fault: Box::new(recv_h),
                    },
                }
            }
            (Type::Fault, "site") => {
                Self::expect_no_args(name, args, span)?;
                let location = Self::core_class(ctx, "CodeLocation");
                HExpr {
                    ty: Self::core_inst(ctx, "Option", vec![location]),
                    mutable: true,
                    kind: HExprKind::FaultSiteGet {
                        fault: Box::new(recv_h),
                    },
                }
            }
            (Type::Fault, "trace") => {
                Self::expect_no_args(name, args, span)?;
                let location = Self::core_class(ctx, "CodeLocation");
                HExpr {
                    ty: ctx.store.intern(Type::List(location)),
                    mutable: true,
                    kind: HExprKind::FaultTraceGet {
                        fault: Box::new(recv_h),
                    },
                }
            }
            (recv_ty, _) => return Err(self.no_control_method(ctx, recv_ty, name, name_span)),
        })
    }
}
