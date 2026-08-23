//! Host boundary values and the resource lifecycle.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

/// The error family one resource kind states.
///
/// A closed handle and a failed service both answer `Result.Err`. The
/// family decides which error value that arm holds. A TCP stream and
/// a TCP listener state the same family, so the resource kind alone
/// is not the axis this dispatch turns on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ResourceErrors {
    Fs,
    Net,
    Tls,
    Tty,
    Signal,
    Pipe,
    Exec,
}

/// The error family of one operation that takes a handle first.
///
/// Every operation named here reads its resource from the first
/// pending argument, so one lookup answers both the closed-handle
/// check and the error value that check must build. An operation that
/// creates a resource takes no handle and answers `None`.
pub(super) fn handle_op_errors(op: u32) -> Option<ResourceErrors> {
    match op {
        lm_abi::OP_FS_READ
        | lm_abi::OP_FS_WRITE
        | lm_abi::OP_FS_SEEK
        | lm_abi::OP_FS_FLUSH
        | lm_abi::OP_FS_CLOSE => Some(ResourceErrors::Fs),
        lm_abi::OP_TCP_ACCEPT
        | lm_abi::OP_TCP_READ
        | lm_abi::OP_TCP_WRITE
        | lm_abi::OP_TCP_SHUTDOWN
        | lm_abi::OP_TCP_LOCAL_ADDRESS
        | lm_abi::OP_TCP_PEER_ADDRESS
        | lm_abi::OP_TCP_CLOSE => Some(ResourceErrors::Net),
        lm_abi::OP_TLS_READ
        | lm_abi::OP_TLS_WRITE
        | lm_abi::OP_TLS_SHUTDOWN
        | lm_abi::OP_TLS_LOCAL_ADDRESS
        | lm_abi::OP_TLS_PEER_ADDRESS
        | lm_abi::OP_TLS_CLOSE => Some(ResourceErrors::Tls),
        lm_abi::OP_TTY_EXIT_RAW => Some(ResourceErrors::Tty),
        lm_abi::OP_SIGNAL_NEXT | lm_abi::OP_SIGNAL_CLOSE => Some(ResourceErrors::Signal),
        lm_abi::OP_PIPE_READ | lm_abi::OP_PIPE_WRITE | lm_abi::OP_PIPE_CLOSE => {
            Some(ResourceErrors::Pipe)
        }
        lm_abi::OP_EXEC_WAIT
        | lm_abi::OP_EXEC_TERMINATE
        | lm_abi::OP_EXEC_KILL
        | lm_abi::OP_EXEC_CLOSE => Some(ResourceErrors::Exec),
        _ => None,
    }
}

/// The error family one resource object belongs to.
fn object_errors(object: &Object) -> Option<ResourceErrors> {
    match object {
        Object::NativeFileHandle { .. } => Some(ResourceErrors::Fs),
        Object::NativeTcpStream { .. } | Object::NativeTcpListener { .. } => {
            Some(ResourceErrors::Net)
        }
        Object::NativeTlsStream { .. } => Some(ResourceErrors::Tls),
        Object::NativeRawMode { .. } => Some(ResourceErrors::Tty),
        Object::NativeSignalStream { .. } => Some(ResourceErrors::Signal),
        Object::NativePipeReader { .. } | Object::NativePipeWriter { .. } => {
            Some(ResourceErrors::Pipe)
        }
        Object::NativeChild { .. } => Some(ResourceErrors::Exec),
        _ => None,
    }
}

/// The resource identifier one resource object names.
fn object_resource(object: &Object) -> Option<u64> {
    match object {
        Object::NativeFileHandle { resource }
        | Object::NativeTcpStream { resource }
        | Object::NativeTcpListener { resource }
        | Object::NativeTlsStream { resource }
        | Object::NativeRawMode { resource }
        | Object::NativeSignalStream { resource }
        | Object::NativePipeReader { resource }
        | Object::NativePipeWriter { resource }
        | Object::NativeChild { resource }
        | Object::NativeHostResource { resource, .. } => Some(*resource),
        _ => None,
    }
}

impl World {
    pub(super) fn build_host_value(
        &mut self,
        vm: VmId,
        value: &HostValue,
        expected: ClosedTypeId,
    ) -> Result<Value, FaultCode> {
        match value {
            HostValue::Unit if matches!(self.envs.ty(expected), Some(ClosedType::Unit)) => {
                Ok(Value::Unit)
            }
            HostValue::Bool(v) if matches!(self.envs.ty(expected), Some(ClosedType::Bool)) => {
                Ok(Value::Bool(*v))
            }
            HostValue::Int(v) if matches!(self.envs.ty(expected), Some(ClosedType::Int)) => {
                Ok(Value::Int(*v))
            }
            HostValue::Float(bits) if matches!(self.envs.ty(expected), Some(ClosedType::Float)) => {
                Ok(Value::Float(lm_value::canonical_float_bits(*bits)))
            }
            HostValue::Str(s) if matches!(self.envs.ty(expected), Some(ClosedType::Str)) => {
                self.machines[vm as usize].alloc(Object::Str(s.clone()))
            }
            HostValue::Bytes(bytes) => {
                if !matches!(self.envs.ty(expected), Some(ClosedType::Bytes)) {
                    return Err(FaultCode::TypeMismatch);
                }
                self.machines[vm as usize].alloc(Object::Bytes(bytes.clone()))
            }
            HostValue::File(token) => self.build_host_file(vm, *token),
            HostValue::List(values) => {
                let element = match self.envs.ty(expected).cloned() {
                    Some(ClosedType::List(element)) => element,
                    Some(ClosedType::Inst(class, args))
                        if Some(class) == self.core.list && args.len() == 1 =>
                    {
                        args[0]
                    }
                    _ => return Err(FaultCode::TypeMismatch),
                };
                let mut items = Vec::with_capacity(values.len());
                for value in values {
                    items.push(self.build_host_value(vm, value, element)?);
                }
                self.machines[vm as usize].alloc(Object::List {
                    items,
                    epoch: StructuralEpoch::default(),
                })
            }
            HostValue::Tuple(values) => {
                let elements = match self.envs.ty(expected).cloned() {
                    Some(ClosedType::Tuple(elements)) if elements.len() == values.len() => elements,
                    _ => return Err(FaultCode::TypeMismatch),
                };
                let mut items = Vec::with_capacity(values.len());
                for (value, element) in values.iter().zip(elements) {
                    items.push(self.build_host_value(vm, value, element)?);
                }
                self.machines[vm as usize].alloc(Object::Tuple { items })
            }
            HostValue::SocketAddress(address) => self.build_host_address(vm, *address),
            HostValue::TcpStream(token) => {
                self.build_host_tcp(vm, *token, crate::ResourceKind::TcpStream)
            }
            HostValue::TcpListener(token) => {
                self.build_host_tcp(vm, *token, crate::ResourceKind::TcpListener)
            }
            HostValue::TlsStream(token) => self.build_host_tls(vm, *token),
            HostValue::RawMode(token) => self.build_host_standard_resource(
                vm,
                *token,
                crate::ResourceKind::RawMode,
                lm_abi::OP_TTY_ENTER_RAW,
            ),
            HostValue::SignalStream(token) => self.build_host_standard_resource(
                vm,
                *token,
                crate::ResourceKind::SignalStream,
                lm_abi::OP_SIGNAL_OPEN,
            ),
            HostValue::PipeReader(token) => self.build_host_standard_resource(
                vm,
                *token,
                crate::ResourceKind::PipeReader,
                lm_abi::OP_PIPE_OPEN,
            ),
            HostValue::PipeWriter(token) => self.build_host_standard_resource(
                vm,
                *token,
                crate::ResourceKind::PipeWriter,
                lm_abi::OP_PIPE_OPEN,
            ),
            HostValue::Child(token) => self.build_host_standard_resource(
                vm,
                *token,
                crate::ResourceKind::Child,
                lm_abi::OP_EXEC_SPAWN,
            ),
            HostValue::Resource(resource) => self.build_host_resource(vm, *resource),
            HostValue::Artifact { module, interface } => {
                let valid = matches!(
                    self.envs.ty(expected),
                    Some(ClosedType::Class(class)) if Some(*class) == self.core.artifact
                );
                if !valid {
                    return Err(FaultCode::TypeMismatch);
                }
                self.machines[vm as usize].alloc(Object::NativeCode(Box::new(
                    lm_heap::PortableCode {
                        kind: lm_heap::PortableCodeKind::Artifact,
                        bytes: module.clone(),
                        interface: Some(interface.clone()),
                        index: u32::MAX,
                        origin: None,
                    },
                )))
            }
            HostValue::SyntaxParse {
                source,
                records,
                status,
                diagnostics,
            } => self.build_syntax_parse(
                vm,
                source.clone(),
                records.clone(),
                *status,
                diagnostics,
                expected,
            ),
            HostValue::Ctor(ctor, parts) => {
                let class = match ctor {
                    CoreCtor::Some => self.core.option_some,
                    CoreCtor::None => self.core.option_none,
                    CoreCtor::Ok => self.core.result_ok,
                    CoreCtor::Err => self.core.result_err,
                    CoreCtor::IoErrorBrokenPipe => self.core.io_error_broken_pipe,
                    CoreCtor::IoErrorInvalidInput => self.core.io_error_invalid_input,
                    CoreCtor::IoErrorLimitExceeded => self.core.io_error_limit_exceeded,
                    CoreCtor::IoErrorUnsupported => self.core.io_error_unsupported,
                    CoreCtor::IoErrorFailed => self.core.io_error_failed,
                    CoreCtor::EnvInvalidName => self.core.env_error_invalid_name,
                    CoreCtor::EnvInvalidEncoding => self.core.env_error_invalid_encoding,
                    CoreCtor::EnvPermissionDenied => self.core.env_error_permission_denied,
                    CoreCtor::EnvFailed => self.core.env_error_failed,
                    CoreCtor::EntropyInvalidInput => self.core.entropy_error_invalid_input,
                    CoreCtor::EntropyLimitExceeded => self.core.entropy_error_limit_exceeded,
                    CoreCtor::EntropyUnavailable => self.core.entropy_error_unavailable,
                    CoreCtor::EntropyFailed => self.core.entropy_error_failed,
                    CoreCtor::FsErrorClosed => self.core.fs_error_closed,
                    CoreCtor::FsErrorInvalidInput => self.core.fs_error_invalid_input,
                    CoreCtor::FsErrorInvalidEncoding => self.core.fs_error_invalid_encoding,
                    CoreCtor::FsErrorLimitExceeded => self.core.fs_error_limit_exceeded,
                    CoreCtor::FsErrorNotFound => self.core.fs_error_not_found,
                    CoreCtor::FsErrorAlreadyExists => self.core.fs_error_already_exists,
                    CoreCtor::FsErrorPermissionDenied => self.core.fs_error_permission_denied,
                    CoreCtor::FsErrorNotDirectory => self.core.fs_error_not_directory,
                    CoreCtor::FsErrorIsDirectory => self.core.fs_error_is_directory,
                    CoreCtor::FsErrorDirectoryNotEmpty => self.core.fs_error_directory_not_empty,
                    CoreCtor::FsErrorCrossDevice => self.core.fs_error_cross_device,
                    CoreCtor::FsErrorUnsupported => self.core.fs_error_unsupported,
                    CoreCtor::FsErrorFailed => self.core.fs_error_failed,
                    CoreCtor::FileKindFile => self.core.file_kind_file,
                    CoreCtor::FileKindDirectory => self.core.file_kind_directory,
                    CoreCtor::FileKindSymlink => self.core.file_kind_symlink,
                    CoreCtor::FileKindOther => self.core.file_kind_other,
                    CoreCtor::FileInfo => self.core.file_info,
                    CoreCtor::DirEntry => self.core.dir_entry,
                    CoreCtor::NetInvalidInput => self.core.net_invalid_input,
                    CoreCtor::NetNameNotFound => self.core.net_name_not_found,
                    CoreCtor::NetUnavailable => self.core.net_unavailable,
                    CoreCtor::NetPermissionDenied => self.core.net_permission_denied,
                    CoreCtor::NetAddressInUse => self.core.net_address_in_use,
                    CoreCtor::NetConnectionRefused => self.core.net_connection_refused,
                    CoreCtor::NetConnectionReset => self.core.net_connection_reset,
                    CoreCtor::NetNotConnected => self.core.net_not_connected,
                    CoreCtor::NetTimedOut => self.core.net_timed_out,
                    CoreCtor::NetClosed => self.core.net_closed,
                    CoreCtor::NetLimitExceeded => self.core.net_limit_exceeded,
                    CoreCtor::NetUnsupported => self.core.net_unsupported,
                    CoreCtor::NetFailed => self.core.net_failed,
                    CoreCtor::TcpReadData => self.core.tcp_read_data,
                    CoreCtor::TcpReadEnd => self.core.tcp_read_end,
                    CoreCtor::TlsInvalidConfig => self.core.tls_invalid_config,
                    CoreCtor::TlsHandshake => self.core.tls_handshake,
                    CoreCtor::TlsCertificate => self.core.tls_certificate,
                    CoreCtor::TlsProtocol => self.core.tls_protocol,
                    CoreCtor::TlsNetwork => self.core.tls_network,
                    CoreCtor::TlsClosed => self.core.tls_closed,
                    CoreCtor::TlsLimitExceeded => self.core.tls_limit_exceeded,
                    CoreCtor::TtySize => self.core.tty_size,
                    CoreCtor::TtyClosed => self.core.tty_error_closed,
                    CoreCtor::TtyNotTerminal => self.core.tty_error_not_terminal,
                    CoreCtor::TtyBusy => self.core.tty_error_busy,
                    CoreCtor::TtyPermissionDenied => self.core.tty_error_permission_denied,
                    CoreCtor::TtyUnsupported => self.core.tty_error_unsupported,
                    CoreCtor::TtyFailed => self.core.tty_error_failed,
                    CoreCtor::SignalInterrupt => self.core.signal_interrupt,
                    CoreCtor::SignalTerminate => self.core.signal_terminate,
                    CoreCtor::SignalClosed => self.core.signal_error_closed,
                    CoreCtor::SignalInvalidInput => self.core.signal_error_invalid_input,
                    CoreCtor::SignalBusy => self.core.signal_error_busy,
                    CoreCtor::SignalUnsupported => self.core.signal_error_unsupported,
                    CoreCtor::SignalLimitExceeded => self.core.signal_error_limit_exceeded,
                    CoreCtor::SignalFailed => self.core.signal_error_failed,
                    CoreCtor::PipeErrorClosed => self.core.pipe_error_closed,
                    CoreCtor::PipeErrorBrokenPipe => self.core.pipe_error_broken_pipe,
                    CoreCtor::PipeErrorInvalidInput => self.core.pipe_error_invalid_input,
                    CoreCtor::PipeErrorLimitExceeded => self.core.pipe_error_limit_exceeded,
                    CoreCtor::PipeErrorUnsupported => self.core.pipe_error_unsupported,
                    CoreCtor::PipeErrorFailed => self.core.pipe_error_failed,
                    CoreCtor::ChildStatusExited => self.core.child_status_exited,
                    CoreCtor::ChildStatusTerminated => self.core.child_status_terminated,
                    CoreCtor::ExecErrorClosed => self.core.exec_error_closed,
                    CoreCtor::ExecErrorInvalidInput => self.core.exec_error_invalid_input,
                    CoreCtor::ExecErrorLimitExceeded => self.core.exec_error_limit_exceeded,
                    CoreCtor::ExecErrorNotFound => self.core.exec_error_not_found,
                    CoreCtor::ExecErrorPermissionDenied => self.core.exec_error_permission_denied,
                    CoreCtor::ExecErrorUnsupported => self.core.exec_error_unsupported,
                    CoreCtor::ExecErrorFailed => self.core.exec_error_failed,
                    CoreCtor::CompileErrors => self.core.compile_errors,
                };
                if matches!(ctor, CoreCtor::Some | CoreCtor::None) {
                    let (class, args) = match self.envs.ty(expected).cloned() {
                        Some(ClosedType::Inst(class, args)) => (class, args),
                        _ => return Err(FaultCode::TypeMismatch),
                    };
                    let option = self.core.option.ok_or(FaultCode::MalformedState)?;
                    let some = self.core.option_some.ok_or(FaultCode::MalformedState)?;
                    let none = self.core.option_none.ok_or(FaultCode::MalformedState)?;
                    if args.len() != 1 || (class != option && class != some && class != none) {
                        return Err(FaultCode::TypeMismatch);
                    }
                    if matches!(ctor, CoreCtor::Some) {
                        if parts.len() != 1 {
                            return Err(FaultCode::TypeMismatch);
                        }
                        return self.build_host_value(vm, &parts[0], args[0]);
                    }
                    if !parts.is_empty() {
                        return Err(FaultCode::TypeMismatch);
                    }
                    let family = self
                        .envs
                        .intern(ClosedType::Inst(option, args))
                        .map_err(|_| FaultCode::BoundaryLimit)?;
                    return Ok(Value::EmptyCase { ty: family, arm: 1 });
                }
                let class = class.ok_or(FaultCode::MalformedState)?;
                let args = match self.envs.ty(expected).cloned() {
                    Some(ClosedType::Inst(_, args)) => args,
                    Some(ClosedType::Class(_)) => Vec::new(),
                    _ => return Err(FaultCode::TypeMismatch),
                };
                let env = self
                    .envs
                    .env_of(args, Vec::new())
                    .map_err(|_| FaultCode::BoundaryLimit)?;
                let templates: Vec<u32> = self.module.classes[class as usize]
                    .fields
                    .iter()
                    .map(|(_, ty)| *ty)
                    .collect();
                if templates.len() != parts.len() {
                    return Err(FaultCode::TypeMismatch);
                }
                let mut fields = Vec::with_capacity(parts.len());
                for (part, template) in parts.iter().zip(templates) {
                    let field = self
                        .envs
                        .close(&self.module, template, env)
                        .map_err(|_| FaultCode::BoundaryLimit)?;
                    fields.push(self.build_host_value(vm, part, field)?);
                }
                self.make_instance(vm, Some(class), fields)
            }
            _ => Err(FaultCode::TypeMismatch),
        }
    }

    fn build_syntax_parse(
        &mut self,
        vm: VmId,
        source: SharedText,
        records: SharedBytes,
        status: HostParseStatus,
        diagnostics: &[HostSyntaxDiagnostic],
        expected: ClosedTypeId,
    ) -> Result<Value, FaultCode> {
        let expected_ok = matches!(
            self.envs.ty(expected),
            Some(ClosedType::Class(class)) if Some(*class) == self.core.syntax_parse
        );
        if !expected_ok {
            return Err(FaultCode::TypeMismatch);
        }
        let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
            .map_err(|_| FaultCode::BadCast)?;
        let root = view.record(view.root()).map_err(|_| FaultCode::BadCast)?;
        if root.class != lm_abi::syntax::SyntaxClass::Node {
            return Err(FaultCode::BadCast);
        }
        let mut roots = Vec::new();
        let result = (|| -> Result<Value, FaultCode> {
            let source = self.machines[vm as usize].alloc(Object::Str(source))?;
            let source_ref = source.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize]
                .vm
                .heap
                .push_host_root(source_ref);
            roots.push(source_ref);

            let records = self.machines[vm as usize].alloc(Object::Bytes(records))?;
            let records_ref = records.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize]
                .vm
                .heap
                .push_host_root(records_ref);
            roots.push(records_ref);

            let tree = self.make_instance(vm, self.core.syntax_tree, vec![source, records])?;
            let tree_ref = tree.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize].vm.heap.set_frozen(tree_ref);
            self.machines[vm as usize].vm.heap.push_host_root(tree_ref);
            roots.push(tree_ref);

            let status_class = match status {
                HostParseStatus::Complete => self.core.parse_complete,
                HostParseStatus::Incomplete => self.core.parse_incomplete,
                HostParseStatus::Invalid => self.core.parse_invalid,
            };
            let status = self.make_instance(vm, status_class, vec![])?;
            let status_ref = status.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize].vm.heap.set_frozen(status_ref);
            self.machines[vm as usize]
                .vm
                .heap
                .push_host_root(status_ref);
            roots.push(status_ref);

            let mut values = Vec::with_capacity(diagnostics.len());
            for diagnostic in diagnostics {
                let message =
                    self.machines[vm as usize].alloc(Object::Str(diagnostic.message.clone()))?;
                let message_ref = message.as_obj().ok_or(FaultCode::MalformedState)?;
                self.machines[vm as usize]
                    .vm
                    .heap
                    .push_host_root(message_ref);
                roots.push(message_ref);
                let value = self.make_instance(
                    vm,
                    self.core.syntax_diagnostic,
                    vec![
                        Value::Int(i64::from(diagnostic.start)),
                        Value::Int(i64::from(diagnostic.stop)),
                        message,
                    ],
                )?;
                let value_ref = value.as_obj().ok_or(FaultCode::MalformedState)?;
                self.machines[vm as usize].vm.heap.set_frozen(value_ref);
                self.machines[vm as usize].vm.heap.push_host_root(value_ref);
                roots.push(value_ref);
                values.push(value);
            }
            let diagnostics = self.machines[vm as usize].alloc(Object::List {
                items: values,
                epoch: StructuralEpoch::default(),
            })?;
            let diagnostics_ref = diagnostics.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize]
                .vm
                .heap
                .set_frozen(diagnostics_ref);
            self.machines[vm as usize]
                .vm
                .heap
                .push_host_root(diagnostics_ref);
            roots.push(diagnostics_ref);
            let value =
                self.make_instance(vm, self.core.syntax_parse, vec![tree, status, diagnostics])?;
            let value_ref = value.as_obj().ok_or(FaultCode::MalformedState)?;
            self.machines[vm as usize].vm.heap.set_frozen(value_ref);
            Ok(value)
        })();
        for root in roots.into_iter().rev() {
            self.machines[vm as usize].vm.heap.pop_host_root(root);
        }
        result
    }

    /// Build one portable socket address inside a machine.
    pub(super) fn build_host_address(
        &mut self,
        vm: VmId,
        address: crate::HostSocketAddress,
    ) -> Result<Value, FaultCode> {
        let (class, bytes) = match address.ip {
            crate::HostIpAddress::V4(bytes) => (self.core.ip_v4, bytes.to_vec()),
            crate::HostIpAddress::V6(bytes) => (self.core.ip_v6, bytes.to_vec()),
        };
        let bytes = self.machines[vm as usize].alloc(Object::Bytes(bytes.into()))?;
        let ip = self.make_instance(vm, class, vec![bytes])?;
        let address = self.make_instance(
            vm,
            self.core.socket_address,
            vec![
                ip,
                Value::Int(i64::from(address.port)),
                Value::Int(i64::from(address.flow_info)),
                Value::Int(i64::from(address.scope_id)),
            ],
        )?;
        let reference = address.as_obj().ok_or(FaultCode::MalformedState)?;
        lm_graph::freeze(
            &mut self.machines[vm as usize].vm.heap,
            reference,
            &self.config.graph,
        )?;
        Ok(address)
    }

    /// Register one host TCP resource and build its guest handle.
    pub(super) fn build_host_tcp(
        &mut self,
        vm: VmId,
        token: u64,
        kind: crate::ResourceKind,
    ) -> Result<Value, FaultCode> {
        let resource = self.next_resource;
        self.next_resource = resource.checked_add(1).ok_or(FaultCode::IntegerOverflow)?;
        let op = self.pending_op(vm).unwrap_or(lm_abi::OP_TCP_CONNECT);
        if let Err(code) =
            self.machines[vm as usize]
                .resources
                .register(kind, vm, resource, u64::MAX, op)
        {
            let host_kind = match kind {
                crate::ResourceKind::TcpStream => crate::HostTcpKind::Stream,
                crate::ResourceKind::TcpListener => crate::HostTcpKind::Listener,
                _ => return Err(FaultCode::MalformedState),
            };
            self.host.close_tcp(crate::HostTcpResource {
                kind: host_kind,
                token,
            });
            return Err(code);
        }
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner: vm,
                kind,
                backing: ResourceBacking::Host(token),
            },
        );
        let object = match kind {
            crate::ResourceKind::TcpStream => Object::NativeTcpStream { resource },
            crate::ResourceKind::TcpListener => Object::NativeTcpListener { resource },
            _ => return Err(FaultCode::MalformedState),
        };
        match self.machines[vm as usize].alloc(object) {
            Ok(value) => Ok(value),
            Err(code) => {
                self.retire_resource(resource, true);
                Err(code)
            }
        }
    }

    /// Register one host TLS stream and build its guest handle.
    pub(super) fn build_host_tls(&mut self, vm: VmId, token: u64) -> Result<Value, FaultCode> {
        let Some(handshake_op) = self.pending_op(vm) else {
            self.host.close_tls(token);
            return Err(FaultCode::TypeMismatch);
        };
        if !matches!(
            handshake_op,
            lm_abi::OP_TLS_HANDSHAKE | lm_abi::OP_TLS_SERVER_HANDSHAKE
        ) {
            self.host.close_tls(token);
            return Err(FaultCode::TypeMismatch);
        }
        let Some(source) = self.pending_resource_of(vm, ResourceErrors::Net) else {
            self.host.close_tls(token);
            return Err(FaultCode::TypeMismatch);
        };
        let valid_source = self.bound_resources.get(&source).is_some_and(|bound| {
            bound.owner == vm
                && bound.kind == crate::ResourceKind::TcpStream
                && matches!(bound.backing, ResourceBacking::Host(_))
        });
        if !valid_source {
            self.host.close_tls(token);
            return Err(FaultCode::TypeMismatch);
        }
        // The host consumed the TCP stream during the handshake.
        // Retire it before the TLS stream takes the same limit slot.
        self.retire_resource(source, false);
        let resource = self.next_resource;
        let Some(next_resource) = resource.checked_add(1) else {
            self.host.close_tls(token);
            return Err(FaultCode::IntegerOverflow);
        };
        self.next_resource = next_resource;
        if let Err(code) = self.machines[vm as usize].resources.register(
            crate::ResourceKind::TlsStream,
            vm,
            resource,
            u64::MAX,
            handshake_op,
        ) {
            self.host.close_tls(token);
            return Err(code);
        }
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner: vm,
                kind: crate::ResourceKind::TlsStream,
                backing: ResourceBacking::Host(token),
            },
        );
        match self.machines[vm as usize].alloc(Object::NativeTlsStream { resource }) {
            Ok(value) => Ok(value),
            Err(code) => {
                self.retire_resource(resource, true);
                Err(code)
            }
        }
    }

    /// Register one host file and build its guest handle.
    pub(super) fn build_host_file(&mut self, vm: VmId, token: u64) -> Result<Value, FaultCode> {
        let resource = self.next_resource;
        self.next_resource = resource.checked_add(1).ok_or(FaultCode::IntegerOverflow)?;
        if let Err(code) = self.machines[vm as usize].resources.register(
            crate::ResourceKind::File,
            vm,
            resource,
            u64::MAX,
            lm_abi::OP_FS_OPEN,
        ) {
            self.host.close_file(token);
            return Err(code);
        }
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner: vm,
                kind: crate::ResourceKind::File,
                backing: ResourceBacking::Host(token),
            },
        );
        match self.machines[vm as usize].alloc(Object::NativeFileHandle { resource }) {
            Ok(value) => Ok(value),
            Err(code) => {
                self.retire_file(resource, true);
                Err(code)
            }
        }
    }

    /// Register one terminal or signal resource and build its handle.
    pub(super) fn build_host_standard_resource(
        &mut self,
        vm: VmId,
        token: u64,
        kind: crate::ResourceKind,
        open_op: u32,
    ) -> Result<Value, FaultCode> {
        let close = |host: &mut Box<dyn Host>| match kind {
            crate::ResourceKind::RawMode => host.close_raw_mode(token),
            crate::ResourceKind::SignalStream => host.close_signal_stream(token),
            crate::ResourceKind::PipeReader | crate::ResourceKind::PipeWriter => {
                host.close_pipe(token)
            }
            crate::ResourceKind::Child => host.close_child(token),
            _ => false,
        };
        if self.pending_op(vm) != Some(open_op) {
            close(&mut self.host);
            return Err(FaultCode::TypeMismatch);
        }
        let resource = self.next_resource;
        let Some(next_resource) = resource.checked_add(1) else {
            close(&mut self.host);
            return Err(FaultCode::IntegerOverflow);
        };
        self.next_resource = next_resource;
        if let Err(code) =
            self.machines[vm as usize]
                .resources
                .register(kind, vm, resource, u64::MAX, open_op)
        {
            close(&mut self.host);
            return Err(code);
        }
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner: vm,
                kind,
                backing: ResourceBacking::Host(token),
            },
        );
        let object = match kind {
            crate::ResourceKind::RawMode => Object::NativeRawMode { resource },
            crate::ResourceKind::SignalStream => Object::NativeSignalStream { resource },
            crate::ResourceKind::PipeReader => Object::NativePipeReader { resource },
            crate::ResourceKind::PipeWriter => Object::NativePipeWriter { resource },
            crate::ResourceKind::Child => Object::NativeChild { resource },
            _ => return Err(FaultCode::MalformedState),
        };
        match self.machines[vm as usize].alloc(object) {
            Ok(value) => Ok(value),
            Err(code) => {
                self.retire_resource(resource, true);
                Err(code)
            }
        }
    }

    /// Register one opaque host resource and build its guest handle.
    pub(super) fn build_host_resource(
        &mut self,
        vm: VmId,
        host: crate::HostResource,
    ) -> Result<Value, FaultCode> {
        if self
            .loaded
            .bundle()
            .resource_by_identity(host.kind)
            .is_none()
        {
            self.host.close_resource(host);
            return Err(FaultCode::TypeMismatch);
        }
        let resource = self.next_resource;
        let Some(next_resource) = resource.checked_add(1) else {
            self.host.close_resource(host);
            return Err(FaultCode::IntegerOverflow);
        };
        self.next_resource = next_resource;
        let Some(op) = self.pending_op(vm) else {
            self.host.close_resource(host);
            return Err(FaultCode::MalformedState);
        };
        let kind = crate::ResourceKind::Extension(host.kind);
        if let Err(code) =
            self.machines[vm as usize]
                .resources
                .register(kind, vm, resource, u64::MAX, op)
        {
            self.host.close_resource(host);
            return Err(code);
        }
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner: vm,
                kind,
                backing: ResourceBacking::Extension(host),
            },
        );
        let object = Object::NativeHostResource {
            kind: host.kind,
            resource,
        };
        match self.machines[vm as usize].alloc(object) {
            Ok(value) => Ok(value),
            Err(code) => {
                self.retire_resource(resource, true);
                Err(code)
            }
        }
    }

    /// Remove one file resource. Close its host token when requested.
    pub(super) fn retire_file(&mut self, resource: u64, close_host: bool) -> bool {
        self.retire_resource(resource, close_host)
    }

    /// Remove one bound resource. Close its host token when requested.
    pub(super) fn retire_resource(&mut self, resource: u64, close_host: bool) -> bool {
        let Some(bound) = self.bound_resources.remove(&resource) else {
            return false;
        };
        if close_host {
            match bound.backing {
                ResourceBacking::Host(token) => match bound.kind {
                    crate::ResourceKind::File => {
                        self.host.close_file(token);
                    }
                    crate::ResourceKind::TcpStream | crate::ResourceKind::TcpListener => {
                        let kind = if bound.kind == crate::ResourceKind::TcpStream {
                            crate::HostTcpKind::Stream
                        } else {
                            crate::HostTcpKind::Listener
                        };
                        self.host.close_tcp(crate::HostTcpResource { kind, token });
                    }
                    crate::ResourceKind::TlsStream => {
                        self.host.close_tls(token);
                    }
                    crate::ResourceKind::RawMode => {
                        self.host.close_raw_mode(token);
                    }
                    crate::ResourceKind::SignalStream => {
                        self.host.close_signal_stream(token);
                    }
                    crate::ResourceKind::PipeReader | crate::ResourceKind::PipeWriter => {
                        self.host.close_pipe(token);
                    }
                    crate::ResourceKind::Child => {
                        self.host.close_child(token);
                    }
                    crate::ResourceKind::PendingOperation | crate::ResourceKind::Extension(_) => {}
                },
                ResourceBacking::Extension(resource) => {
                    self.host.close_resource(resource);
                }
                ResourceBacking::Driver(_) => {}
            }
        }
        if let Some(machine) = self.machines.get_mut(bound.owner as usize) {
            machine.resources.close_kind(bound.kind, resource);
        }
        true
    }

    /// Close every external resource owned or serviced by one machine.
    pub(super) fn close_resources_for_machine(&mut self, machine: VmId) {
        let pending: Vec<u64> = self.machines[machine as usize]
            .resources
            .pending_scopes()
            .collect();
        for token in pending {
            let _ = self.host.cancel_wait(token);
        }
        let resources: Vec<u64> = self
            .bound_resources
            .iter()
            .filter_map(|(resource, bound)| {
                let serviced =
                    matches!(bound.backing, ResourceBacking::Driver(driver) if driver == machine);
                (bound.owner == machine || serviced).then_some(*resource)
            })
            .collect();
        for resource in resources {
            self.retire_resource(resource, true);
        }
    }

    /// Return the file resource in the first pending argument.
    pub(super) fn pending_resource_object(&self, vm: VmId) -> Option<&Object> {
        let machine = self.machines.get(vm as usize)?;
        let value = *machine.vm.pending.as_ref()?.args.first()?;
        Some(machine.vm.heap.get(value.as_obj()?))
    }

    /// Return the external resource in the first pending argument,
    /// whatever kind it is.
    pub(super) fn pending_bound_resource(&self, vm: VmId) -> Option<u64> {
        object_resource(self.pending_resource_object(vm)?)
    }

    /// Return the error family of the resource in the first pending
    /// argument.
    pub(super) fn pending_resource_errors(&self, vm: VmId) -> Option<ResourceErrors> {
        object_errors(self.pending_resource_object(vm)?)
    }

    /// Return the resource in the first pending argument when it
    /// belongs to `family`.
    pub(super) fn pending_resource_of(&self, vm: VmId, family: ResourceErrors) -> Option<u64> {
        let object = self.pending_resource_object(vm)?;
        if object_errors(object)? != family {
            return None;
        }
        object_resource(object)
    }

    /// Return every pipe resource supplied to the pending child spawn.
    pub(super) fn pending_exec_pipe_resources(&self, vm: VmId) -> Vec<u64> {
        let Some(machine) = self.machines.get(vm as usize) else {
            return Vec::new();
        };
        let Some(spec) = machine
            .vm
            .pending
            .as_ref()
            .and_then(|pending| pending.args.first())
            .and_then(|value| value.as_obj())
            .map(|reference| machine.vm.heap.get(reference))
        else {
            return Vec::new();
        };
        let Object::Instance { class, fields, .. } = spec else {
            return Vec::new();
        };
        if Some(*class) != self.core.exec_spec || fields.len() != 7 {
            return Vec::new();
        }
        let mut resources = Vec::new();
        for value in [&fields[4], &fields[5], &fields[6]] {
            let Some(Object::Instance { fields, .. }) = value
                .as_obj()
                .map(|reference| machine.vm.heap.get(reference))
            else {
                continue;
            };
            let Some(resource) =
                fields
                    .first()
                    .and_then(|value| value.as_obj())
                    .and_then(|reference| match machine.vm.heap.get(reference) {
                        Object::NativePipeReader { resource }
                        | Object::NativePipeWriter { resource } => Some(*resource),
                        _ => None,
                    })
            else {
                continue;
            };
            if !resources.contains(&resource) {
                resources.push(resource);
            }
        }
        resources
    }

    pub(super) fn file_handle_resource(&self, holder: VmId, value: Value) -> Option<u64> {
        let reference = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(reference) {
            Object::NativeFileHandle { resource }
            | Object::NativeTcpStream { resource }
            | Object::NativeTcpListener { resource } => Some(*resource),
            Object::NativeTlsStream { resource } => Some(*resource),
            Object::NativeRawMode { resource } | Object::NativeSignalStream { resource } => {
                Some(*resource)
            }
            Object::NativePipeReader { resource }
            | Object::NativePipeWriter { resource }
            | Object::NativeChild { resource } => Some(*resource),
            _ => None,
        }
    }

    pub(super) fn resource_control(&self, holder: VmId, value: Value) -> Option<(VmId, u64)> {
        let reference = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(reference) {
            Object::NativeResourceHandle { surface, resource } => Some((*surface, *resource)),
            _ => None,
        }
    }

    pub(super) fn value_is_result_ok(&self, holder: VmId, value: Value) -> bool {
        value.as_obj().is_some_and(|reference| {
            matches!(
                self.machines[holder as usize].vm.heap.get(reference),
                Object::Instance { class, .. } if Some(*class) == self.core.result_ok
            )
        })
    }

    pub(super) fn value_is_result_error_class(
        &self,
        holder: VmId,
        value: Value,
        error_class: Option<u32>,
    ) -> bool {
        let Some(result) = value.as_obj() else {
            return false;
        };
        let Object::Instance { class, fields, .. } =
            self.machines[holder as usize].vm.heap.get(result)
        else {
            return false;
        };
        if Some(*class) != self.core.result_err || fields.len() != 1 {
            return false;
        }
        fields[0].as_obj().is_some_and(|error| {
            matches!(
                self.machines[holder as usize].vm.heap.get(error),
                Object::Instance { class, .. } if Some(*class) == error_class
            )
        })
    }

    pub(super) fn build_resource_control(
        &mut self,
        holder: VmId,
        surface: VmId,
        resource: u64,
    ) -> Result<Value, FaultCode> {
        self.machines[holder as usize].alloc(Object::NativeResourceHandle { surface, resource })
    }

    /// Find every machine in one controlled machine world.
    pub(super) fn controlled_machines(&mut self, root: VmId) -> Result<Vec<VmId>, FaultCode> {
        let mut machines = Vec::new();
        let mut queue = std::collections::VecDeque::from([root]);
        while let Some(machine) = queue.pop_front() {
            if machines.contains(&machine) {
                continue;
            }
            machines.push(machine);
            for target in self.machine_references(machine)? {
                if !machines.contains(&target) {
                    queue.push_back(target);
                }
            }
        }
        Ok(machines)
    }

    /// Find every live file resource in one controlled machine world.
    pub(super) fn controlled_file_resources(&mut self, root: VmId) -> Result<Vec<u64>, FaultCode> {
        let machines = self.controlled_machines(root)?;
        let mut resources = std::collections::BTreeSet::new();
        for (resource, file) in &self.bound_resources {
            if machines.contains(&file.owner) {
                resources.insert(*resource);
            }
        }
        for machine in machines {
            let order = self.snapshot_object_order(machine)?;
            let heap = &self.machines[machine as usize].vm.heap;
            for reference in order {
                let resource = match heap.get(reference) {
                    Object::NativeFileHandle { resource }
                    | Object::NativeResourceHandle { resource, .. }
                    | Object::NativeTcpStream { resource }
                    | Object::NativeTcpListener { resource } => *resource,
                    Object::NativeTlsStream { resource }
                    | Object::NativeRawMode { resource }
                    | Object::NativeSignalStream { resource } => *resource,
                    Object::NativePipeReader { resource }
                    | Object::NativePipeWriter { resource }
                    | Object::NativeChild { resource } => *resource,
                    _ => continue,
                };
                if self.bound_resources.contains_key(&resource) {
                    resources.insert(resource);
                }
            }
        }
        Ok(resources.into_iter().collect())
    }

    pub(super) fn build_resource_list(
        &mut self,
        holder: VmId,
        surface: VmId,
        resources: &[u64],
    ) -> Result<Value, FaultCode> {
        let mut items = Vec::with_capacity(resources.len());
        let mut roots = Vec::with_capacity(resources.len());
        for resource in resources {
            match self.build_resource_control(holder, surface, *resource) {
                Ok(value) => {
                    let reference = value.as_obj().ok_or(FaultCode::MalformedState)?;
                    self.machines[holder as usize]
                        .vm
                        .heap
                        .push_host_root(reference);
                    roots.push(reference);
                    items.push(value);
                }
                Err(code) => {
                    for root in roots {
                        self.machines[holder as usize].vm.heap.pop_host_root(root);
                    }
                    return Err(code);
                }
            }
        }
        let list = self.machines[holder as usize].alloc(Object::List {
            items,
            epoch: StructuralEpoch::default(),
        });
        for root in roots {
            self.machines[holder as usize].vm.heap.pop_host_root(root);
        }
        list
    }

    pub(super) fn register_driver_file(
        &mut self,
        owner: VmId,
        driver: VmId,
    ) -> Result<u64, FaultCode> {
        self.register_driver_resource(owner, driver, crate::ResourceKind::File, lm_abi::OP_FS_OPEN)
    }

    pub(super) fn register_driver_resource(
        &mut self,
        owner: VmId,
        driver: VmId,
        kind: crate::ResourceKind,
        op: u32,
    ) -> Result<u64, FaultCode> {
        self.machines[owner as usize].resources.prepare_register()?;
        let resource = self.next_resource;
        self.next_resource = resource.checked_add(1).ok_or(FaultCode::IntegerOverflow)?;
        self.machines[owner as usize]
            .resources
            .register(kind, owner, resource, u64::MAX, op)?;
        self.bound_resources.insert(
            resource,
            BoundResource {
                owner,
                kind,
                backing: ResourceBacking::Driver(driver),
            },
        );
        Ok(resource)
    }

    /// Build the ordinary result for a closed file operation.
    pub(super) fn closed_reply(
        &mut self,
        vm: VmId,
        family: ResourceErrors,
    ) -> Result<Value, FaultCode> {
        let arm = match family {
            ResourceErrors::Fs => self.core.fs_error_closed,
            ResourceErrors::Net => self.core.net_closed,
            ResourceErrors::Tls => self.core.tls_closed,
            ResourceErrors::Tty => self.core.tty_error_closed,
            ResourceErrors::Signal => self.core.signal_error_closed,
            ResourceErrors::Pipe => self.core.pipe_error_closed,
            ResourceErrors::Exec => self.core.exec_error_closed,
        };
        let closed = self.make_instance(vm, arm, vec![])?;
        self.make_instance(vm, self.core.result_err, vec![closed])
    }

    /// Build one ordinary service failure for a resource family.
    ///
    /// A TLS failure nests a network error, because `TlsError` states
    /// a transport failure through `TlsError.Network`.
    pub(super) fn failed_reply(
        &mut self,
        vm: VmId,
        family: ResourceErrors,
        message: &str,
    ) -> Result<Value, FaultCode> {
        let text = self.machines[vm as usize].alloc(Object::Str(message.to_string().into()))?;
        let error = match family {
            ResourceErrors::Fs => self.make_instance(vm, self.core.fs_error_failed, vec![text])?,
            ResourceErrors::Net => self.make_instance(vm, self.core.net_failed, vec![text])?,
            ResourceErrors::Tls => {
                let network = self.make_instance(vm, self.core.net_failed, vec![text])?;
                self.make_instance(vm, self.core.tls_network, vec![network])?
            }
            ResourceErrors::Tty => {
                self.make_instance(vm, self.core.tty_error_failed, vec![text])?
            }
            ResourceErrors::Signal => {
                self.make_instance(vm, self.core.signal_error_failed, vec![text])?
            }
            ResourceErrors::Pipe => {
                self.make_instance(vm, self.core.pipe_error_failed, vec![text])?
            }
            ResourceErrors::Exec => {
                self.make_instance(vm, self.core.exec_error_failed, vec![text])?
            }
        };
        self.make_instance(vm, self.core.result_err, vec![error])
    }

    /// Build one ordinary invalid network argument.
    pub(super) fn invalid_net_reply(
        &mut self,
        vm: VmId,
        message: &str,
    ) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.to_string().into()))?;
        let error = self.make_instance(vm, self.core.net_invalid_input, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
    }
}
