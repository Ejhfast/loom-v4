//! Perform handling, request routing, policy, and mocks.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// Handle one perform of `vm`: record the pending request, then
    /// stop for a driver or resolve policy.
    pub(super) fn handle_perform(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        op: u32,
        args: Vec<Value>,
    ) -> Option<RootEvent> {
        let m = &mut self.machines[vm as usize];
        let ordinal = match m.take_request_ordinal() {
            Ok(ordinal) => ordinal,
            Err(code) => {
                m.set_fault(code, "the request ordinal is exhausted", Some(op));
                return None;
            }
        };
        m.vm.pending = Some(Pending { op, args, ordinal });
        let Some(top) = stack.last().copied() else {
            return Some(self.fault_event(vm, "the performing machine left the driver stack"));
        };
        debug_assert_eq!(top.vm, vm);
        if top.mode == StopMode::DriveToAsk {
            // Stop before policy lookup.
            let Some(act) = stack.pop() else {
                return Some(self.fault_event(vm, "the performing machine left the driver stack"));
            };
            self.release_activation(act);
            self.machines[vm as usize].vm.state = MachineState::Asked;
            match act.reply_to {
                None => return Some(RootEvent::Asked(ordinal)),
                Some(parent) => {
                    if self.machines[parent as usize].vm.nested != Some(vm) {
                        self.machines[parent as usize].set_fault(
                            FaultCode::MalformedState,
                            "the asked result has no matching control edge",
                            None,
                        );
                        return None;
                    }
                    self.machines[parent as usize].vm.nested = None;
                    self.deliver_asked(vm, parent, ordinal);
                    return None;
                }
            }
        }
        self.resolve_and_dispatch(stack, vm, PolicyCursor::Table(vm), DispatchMode::Continue)
    }

    /// Build and install `DriveEvent.Asked(request)` into `parent`.
    pub(super) fn deliver_asked(&mut self, child: VmId, parent: VmId, ordinal: u64) {
        let built = self.machines[parent as usize]
            .alloc(Object::NativeRequest { vm: child, ordinal })
            .and_then(|request| self.make_instance(parent, self.core.drive_asked, vec![request]));
        // A bounded turn answers `Option[DriveEvent[T]]`.
        let family = if self.pending_op(parent) == Some(lm_abi::OP_VM_DRIVE_FOR) {
            Family::DriveFor
        } else {
            Family::Drive
        };
        let built = self.wrap_turn(parent, family, built);
        match built {
            Ok(value) => self.install_value_reply(parent, value),
            Err(code) => self.machines[parent as usize].set_fault(code, "", None),
        }
    }

    /// Install a descendant request as the result of `surface.drive()`.
    pub(super) fn deliver_routed_asked(&mut self, target: VmId, parent: VmId, ordinal: u64) {
        let built = self.machines[parent as usize]
            .alloc(Object::NativeRequest {
                vm: target,
                ordinal,
            })
            .and_then(|request| self.make_instance(parent, self.core.drive_asked, vec![request]));
        // A bounded turn answers `Option[DriveEvent[T]]`.
        let family = if self.pending_op(parent) == Some(lm_abi::OP_VM_DRIVE_FOR) {
            Family::DriveFor
        } else {
            Family::Drive
        };
        let built = self.wrap_turn(parent, family, built);
        match built {
            Ok(value) => self.install_value_reply(parent, value),
            Err(code) => self.machines[parent as usize].set_fault(code, "", None),
        }
    }

    /// Mint a fresh token for one parked descendant request.
    pub(super) fn recover_routed_asked(&mut self, surface: VmId, parent: VmId, control_op: u32) {
        let Some(route) = self.machines[surface as usize].vm.routed else {
            self.fault_caller(
                parent,
                control_op,
                FaultCode::MalformedState,
                "the machine holds no routed request",
            );
            return;
        };
        if self.machines[route.target as usize].vm.pending.is_none() {
            self.fault_caller(
                parent,
                control_op,
                FaultCode::MalformedState,
                "the routed machine holds no pending request",
            );
            return;
        }
        let fresh = match self.machines[route.target as usize].take_request_ordinal() {
            Ok(ordinal) => ordinal,
            Err(code) => {
                let pending_op = self.pending_op(route.target);
                self.machines[route.target as usize].set_fault(
                    code,
                    "the request ordinal is exhausted",
                    pending_op,
                );
                self.machines[surface as usize].vm.routed = None;
                self.fault_caller(
                    parent,
                    control_op,
                    code,
                    "the routed request ordinal is exhausted",
                );
                return;
            }
        };
        if let Some(pending) = self.machines[route.target as usize].vm.pending.as_mut() {
            pending.ordinal = fresh;
        }
        self.deliver_routed_asked(route.target, parent, fresh);
    }

    /// Deliver a request that a descendant parked on a driven surface.
    ///
    /// The driver stack reached this surface again. The activation of
    /// the surface ends here, exactly as it ends for a request that
    /// reached the driver on one stack.
    pub(super) fn deliver_parked_route(
        &mut self,
        stack: &mut Vec<Activation>,
        at: usize,
    ) -> Option<RootEvent> {
        let surface = stack[at].vm;
        let holder = stack[at].reply_to;
        let route = self.machines[surface as usize].vm.routed?;
        let Some(ordinal) = self.machines[route.target as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| pending.ordinal)
        else {
            self.machines[surface as usize].vm.routed = None;
            self.machines[route.target as usize].set_fault(
                FaultCode::MalformedState,
                "the parked request has no pending operation",
                None,
            );
            return None;
        };
        while stack.len() > at {
            let act = stack.pop().expect("the activation index is in the stack");
            self.release_activation(act);
        }
        match holder {
            Some(parent) => {
                if self.machines[parent as usize].vm.nested != Some(surface) {
                    self.machines[parent as usize].set_fault(
                        FaultCode::MalformedState,
                        "the parked ask has no matching control edge",
                        None,
                    );
                    return None;
                }
                self.machines[parent as usize].vm.nested = None;
                self.deliver_routed_asked(route.target, parent, ordinal);
                None
            }
            None => Some(RootEvent::Asked(ordinal)),
        }
    }

    /// Park one request on a surface whose driver runs on another
    /// scheduler task.
    ///
    /// The performing machine stops at `Asked`, exactly as it stops for
    /// a driver on the same stack. The wake makes the driver task
    /// runnable, and the driver loop reads the routed request when it
    /// resumes.
    pub(super) fn park_routed_request(
        &mut self,
        surface: VmId,
        target: VmId,
        cursor: PolicyCursor,
    ) {
        if surface == target {
            self.machines[target as usize].set_fault(
                FaultCode::MalformedState,
                "a machine cannot route a request to itself",
                None,
            );
            return;
        }
        // Several procs of one driven world can surface at once. The
        // driver serves one at a time. Every performer waits at
        // `Asked` holding its own request, and the surface names only
        // the request it serves now. `promote_next_route` finds the
        // next waiting performer, so a waiting request needs no
        // separate record and no snapshot field.
        self.machines[target as usize].vm.state = MachineState::Asked;
        if self.machines[surface as usize].vm.routed.is_none() {
            self.machines[surface as usize].vm.routed = Some(RoutedRequest { target, cursor });
        }
        if let Some(key) = self.task_key(target) {
            self.emit_removed(key);
        }
        if let Some(key) = self.task_key(surface) {
            self.emit_wake(WakeKey::Asked(key));
        }
    }

    /// The policy position of one request when `surface` serves it.
    ///
    /// The walk follows the pass chain of `resolve_policy`. It stops at
    /// `surface` and answers the position after that pass. The walk
    /// reads no driver flag, because a driver clears that flag while it
    /// serves an earlier request.
    pub(super) fn route_cursor_to(&self, vm: VmId, op: u32, surface: VmId) -> Option<PolicyCursor> {
        let mut cur = vm;
        for _ in 0..=self.machines.len() {
            let machine = &self.machines[cur as usize];
            let Some(Action::Pass) = machine.table.lookup(op) else {
                return None;
            };
            let next = match machine.vm.parent {
                Some(parent) => PolicyCursor::Table(parent),
                None => PolicyCursor::Root,
            };
            if cur == surface {
                return Some(next);
            }
            let parent = machine.vm.parent?;
            cur = parent;
        }
        None
    }

    /// Give the free driver slot of one surface to the next waiting
    /// request.
    ///
    /// A performer that surfaced while the driver served another
    /// request waits at `Asked` with its own pending record. The walk
    /// below finds it and rebuilds its policy position, so the world
    /// stores no queue and a snapshot needs no queue field.
    pub(super) fn promote_next_route(&mut self, surface: VmId, served: VmId) {
        if self.machines[surface as usize].vm.routed.is_some() {
            return;
        }
        let mut found = None;
        for vm in 0..self.machines.len() as VmId {
            if vm == surface || vm == served {
                continue;
            }
            let machine = &self.machines[vm as usize];
            if machine.vm.state != MachineState::Asked {
                continue;
            }
            let Some(op) = machine.vm.pending.as_ref().map(|pending| pending.op) else {
                continue;
            };
            if let Some(cursor) = self.route_cursor_to(vm, op, surface) {
                found = Some((vm, cursor));
                break;
            }
        }
        let Some((target, cursor)) = found else {
            return;
        };
        self.machines[surface as usize].vm.routed = Some(RoutedRequest { target, cursor });
        if let Some(key) = self.task_key(surface) {
            self.emit_wake(WakeKey::Asked(key));
        }
    }

    /// The wake conditions of one blocked task.
    ///
    /// A task that holds a `drive` call also waits for every surface it
    /// drives. A descendant of one of those surfaces can surface a
    /// request while the task waits for another condition.
    pub fn block_wakes(&self, key: TaskKey, primary: WakeKey) -> Vec<WakeKey> {
        let mut wakes = vec![primary];
        let Some(saved) = self.suspended.get(&key.vm) else {
            return wakes;
        };
        for act in &saved.activations {
            if act.mode != StopMode::DriveToAsk {
                continue;
            }
            let Some(surface) = self.task_key(act.vm) else {
                continue;
            };
            let wake = WakeKey::Asked(surface);
            if !wakes.contains(&wake) {
                wakes.push(wake);
            }
        }
        wakes
    }

    /// Park a nested activation chain at its nearest active driver.
    pub(super) fn route_request(
        &mut self,
        stack: &mut Vec<Activation>,
        surface: VmId,
        target: VmId,
        cursor: PolicyCursor,
        dispatch_mode: DispatchMode,
    ) -> Option<RootEvent> {
        let Some(ordinal) = self.machines[target as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| pending.ordinal)
        else {
            self.machines[target as usize].set_fault(
                FaultCode::MalformedState,
                "the routed request has no pending operation",
                None,
            );
            return None;
        };
        let Some(at) = stack
            .iter()
            .rposition(|act| act.vm == surface && act.mode == StopMode::DriveToAsk)
        else {
            // The driver holds its `drive` call on another scheduler
            // task, so this stack cannot reach it. A proc of the driven
            // world takes this path, because the scheduler runs it on
            // its own task.
            //
            // Park the request on the surface and wake the driver task.
            // The driver reads the request when its stack resumes, so
            // the driver serves every descendant at every depth.
            self.park_routed_request(surface, target, cursor);
            return None;
        };
        let _ = dispatch_mode;
        let holder = stack[at].reply_to;
        if surface == target || self.machines[surface as usize].vm.routed.is_some() {
            self.machines[target as usize].set_fault(
                FaultCode::MalformedState,
                "the driver already holds a routed request",
                None,
            );
            return None;
        }
        while stack.len() > at {
            let act = stack.pop().expect("the activation index is in the stack");
            self.release_activation(act);
        }
        self.machines[target as usize].vm.state = MachineState::Asked;
        self.machines[surface as usize].vm.routed = Some(RoutedRequest { target, cursor });
        match holder {
            Some(parent) => {
                if self.machines[parent as usize].vm.nested != Some(surface) {
                    self.machines[parent as usize].set_fault(
                        FaultCode::MalformedState,
                        "the routed ask has no matching control edge",
                        None,
                    );
                    return None;
                }
                self.machines[parent as usize].vm.nested = None;
                self.deliver_routed_asked(target, parent, ordinal);
                None
            }
            None => Some(RootEvent::Asked(ordinal)),
        }
    }

    /// Resolve one pending perform from a saved policy position.
    pub(super) fn resolve_and_dispatch(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        cursor: PolicyCursor,
        dispatch_mode: DispatchMode,
    ) -> Option<RootEvent> {
        let Some(op) = self.pending_op(vm) else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "policy resolution found no pending request",
                None,
            );
            return None;
        };
        match self.resolve_policy(cursor, op) {
            Resolution::Denied => {
                self.machines[vm as usize].set_fault(
                    FaultCode::PolicyDenied,
                    format!("the operation {} is not granted", lm_abi::op_name(op)),
                    Some(op),
                );
            }
            Resolution::Mock { owner, closure } => self.start_mock(stack, vm, owner, closure),
            Resolution::Driver { surface, cursor } => {
                return self.route_request(stack, surface, vm, cursor, dispatch_mode);
            }
            Resolution::Root => {
                if lm_abi::op(op).kind == lm_abi::OpKind::VmControl {
                    self.kernel_exec(stack, vm, op, dispatch_mode);
                } else {
                    // An operation that names a handle answers
                    // "closed" when no live resource stands behind
                    // that handle. One lookup states both the family
                    // to read and the error value to build, so a new
                    // resource kind adds an arm and no branch here.
                    if let Some(family) = handle_op_errors(op) {
                        if self
                            .pending_resource_of(vm, family)
                            .is_none_or(|resource| !self.bound_resources.contains_key(&resource))
                        {
                            let built = self.closed_reply(vm, family);
                            self.reply_or_fault(vm, op, built);
                            return None;
                        }
                    }
                    let driver = self.pending_bound_resource(vm).and_then(|resource| {
                        self.bound_resources
                            .get(&resource)
                            .and_then(|file| match file.backing {
                                ResourceBacking::Driver(driver) => Some(driver),
                                ResourceBacking::Host(_) => None,
                            })
                    });
                    if let Some(driver) = driver {
                        let message = format!(
                            "a resource backed by driver machine {driver} reached the root host"
                        );
                        let family = self
                            .pending_resource_errors(vm)
                            .unwrap_or(ResourceErrors::Net);
                        let built = self.failed_reply(vm, family, &message);
                        self.reply_or_fault(vm, op, built);
                        return None;
                    }
                    let args = match self.host_args(vm) {
                        Ok(args) => args,
                        Err(code) => {
                            if matches!(op, lm_abi::OP_TCP_CONNECT | lm_abi::OP_TCP_LISTEN) {
                                let built = self
                                    .invalid_net_reply(vm, "the socket address has invalid fields");
                                self.reply_or_fault(vm, op, built);
                                return None;
                            }
                            self.fault_caller(
                                vm,
                                op,
                                code,
                                "an operation argument has another shape",
                            );
                            return None;
                        }
                    };
                    if let Err(code) = self.machines[vm as usize].resources.prepare_register() {
                        self.machines[vm as usize].set_fault(
                            code,
                            "the world has no host resource capacity",
                            Some(op),
                        );
                        return None;
                    }
                    let Some(completion) = self.completion_key(vm) else {
                        self.machines[vm as usize].set_fault(
                            FaultCode::MalformedState,
                            "the host operation has no completion key",
                            Some(op),
                        );
                        return None;
                    };
                    match self.host.start(completion, op, args) {
                        HostStart::Completed(reply) => self.install_host_reply(vm, reply),
                        HostStart::Waiting(token) => self.start_wait(vm, op, token),
                        HostStart::Failed(message) => {
                            self.machines[vm as usize].set_fault(
                                FaultCode::HostFault,
                                message,
                                Some(op),
                            );
                        }
                    }
                }
            }
        }
        None
    }

    /// Record one suspended host operation.
    ///
    /// The manifest classifies every operation. An operation declared
    /// machine state must complete inside the host call, because a
    /// suspended one would leave a live callback that no snapshot can
    /// copy. A host that breaks the contract faults the machine.
    ///
    /// A suspending operation registers one host attachment in the
    /// resource registry. Snapshot preflight reads it, and the
    /// completion or the machine termination closes it.
    pub(super) fn start_wait(&mut self, vm: VmId, op: u32, token: u64) {
        if !lm_abi::op(op).suspends() {
            self.machines[vm as usize].set_fault(
                FaultCode::HostFault,
                format!(
                    "the host suspended {}, which the manifest declares machine state",
                    lm_abi::op_name(op)
                ),
                Some(op),
            );
            return;
        }
        let Some(ordinal) = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .map(|p| p.ordinal)
        else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the host suspended a machine with no pending request",
                Some(op),
            );
            return;
        };
        let m = &mut self.machines[vm as usize];
        if let Err(code) = m.resources.register(
            crate::ResourceKind::PendingOperation,
            vm,
            token,
            ordinal,
            op,
        ) {
            m.set_fault(
                code,
                "the machine reached its host resource limit",
                Some(op),
            );
            return;
        }
        m.vm.state = MachineState::Waiting;
    }

    /// Extract plain-data host arguments from the pending perform.
    ///
    /// Extract only plain data and opaque host tokens.
    pub(super) fn host_args(&self, vm: VmId) -> Result<Vec<HostArg>, FaultCode> {
        let m = &self.machines[vm as usize];
        let pending = m.vm.pending.as_ref().ok_or(FaultCode::MalformedState)?;
        pending
            .args
            .iter()
            .map(|value| match value {
                Value::Int(v) => Ok(HostArg::Int(*v)),
                Value::Obj(r) => match m.vm.heap.get(*r) {
                    Object::Str(text) => Ok(HostArg::Str(text.clone())),
                    Object::Bytes(bytes) => bytes
                        .try_bounded()
                        .map(HostArg::Bytes)
                        .map_err(|_| FaultCode::HeapLimit),
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.compile_env =>
                    {
                        self.host_compile_env(vm, fields)
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.compile_options =>
                    {
                        self.host_compile_options(vm, fields)
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.syntax_node =>
                    {
                        self.host_syntax(vm, fields)
                    }
                    Object::NativeFileHandle { resource } => {
                        let file = self
                            .bound_resources
                            .get(resource)
                            .ok_or(FaultCode::TypeMismatch)?;
                        match file.backing {
                            ResourceBacking::Host(token) => Ok(HostArg::File(token)),
                            ResourceBacking::Driver(_) => Err(FaultCode::TypeMismatch),
                        }
                    }
                    Object::NativeTcpStream { resource } => {
                        self.host_tcp_arg(*resource, crate::HostTcpKind::Stream)
                    }
                    Object::NativeTcpListener { resource } => {
                        self.host_tcp_arg(*resource, crate::HostTcpKind::Listener)
                    }
                    Object::NativeTlsStream { resource } => {
                        let bound = self
                            .bound_resources
                            .get(resource)
                            .ok_or(FaultCode::TypeMismatch)?;
                        if bound.kind != crate::ResourceKind::TlsStream {
                            return Err(FaultCode::TypeMismatch);
                        }
                        match bound.backing {
                            ResourceBacking::Host(token) => Ok(HostArg::Tls(token)),
                            ResourceBacking::Driver(_) => Err(FaultCode::TypeMismatch),
                        }
                    }
                    Object::List { items, .. } => {
                        let mut values = Vec::with_capacity(items.len());
                        for item in items {
                            let Value::Obj(reference) = item else {
                                return Err(FaultCode::TypeMismatch);
                            };
                            let Object::Bytes(bytes) = m.vm.heap.get(*reference) else {
                                return Err(FaultCode::TypeMismatch);
                            };
                            values.push(HostArg::Bytes(
                                bytes.try_bounded().map_err(|_| FaultCode::HeapLimit)?,
                            ));
                        }
                        Ok(HostArg::List(values))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.socket_address =>
                    {
                        self.host_socket_address(vm, fields)
                            .map(HostArg::SocketAddress)
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.shutdown_read && fields.is_empty() =>
                    {
                        Ok(HostArg::Shutdown(crate::HostShutdown::Read))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.shutdown_write && fields.is_empty() =>
                    {
                        Ok(HostArg::Shutdown(crate::HostShutdown::Write))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.shutdown_both && fields.is_empty() =>
                    {
                        Ok(HostArg::Shutdown(crate::HostShutdown::Both))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_read_only && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::ReadOnly))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_write_only && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::WriteOnly))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_read_write && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::ReadWrite))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_create && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::Create))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_create_truncate && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::CreateTruncate))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.open_append && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::Append))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.seek_start =>
                    {
                        match fields.as_slice() {
                            [Value::Int(offset)] => {
                                Ok(HostArg::SeekFrom(HostSeekFrom::Start(*offset)))
                            }
                            _ => Err(FaultCode::TypeMismatch),
                        }
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.seek_current =>
                    {
                        match fields.as_slice() {
                            [Value::Int(offset)] => {
                                Ok(HostArg::SeekFrom(HostSeekFrom::Current(*offset)))
                            }
                            _ => Err(FaultCode::TypeMismatch),
                        }
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.seek_end =>
                    {
                        match fields.as_slice() {
                            [Value::Int(offset)] => {
                                Ok(HostArg::SeekFrom(HostSeekFrom::End(*offset)))
                            }
                            _ => Err(FaultCode::TypeMismatch),
                        }
                    }
                    _ => Err(FaultCode::TypeMismatch),
                },
                _ => Err(FaultCode::TypeMismatch),
            })
            .collect()
    }

    fn host_compile_env(&self, vm: VmId, fields: &[Value]) -> Result<HostArg, FaultCode> {
        let [modules, roots] = fields else {
            return Err(FaultCode::TypeMismatch);
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let modules = modules.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let Object::List { items: modules, .. } = heap.get(modules) else {
            return Err(FaultCode::TypeMismatch);
        };
        let mut host_modules = Vec::new();
        host_modules
            .try_reserve_exact(modules.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for module in modules {
            let reference = module.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::NativeCode(code) = heap.get(reference) else {
                return Err(FaultCode::TypeMismatch);
            };
            if code.kind != lm_heap::PortableCodeKind::VerifiedModule {
                return Err(FaultCode::TypeMismatch);
            }
            let interface = code.interface.as_ref().ok_or(FaultCode::TypeMismatch)?;
            host_modules.push(HostCompileModule {
                artifact: code.bytes.try_bounded().map_err(|_| FaultCode::HeapLimit)?,
                interface: interface.try_bounded().map_err(|_| FaultCode::HeapLimit)?,
            });
        }

        let roots = roots.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let Object::List { items: roots, .. } = heap.get(roots) else {
            return Err(FaultCode::TypeMismatch);
        };
        let mut host_roots = Vec::new();
        host_roots
            .try_reserve_exact(roots.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for root in roots {
            let reference = root.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Tuple { items } = heap.get(reference) else {
                return Err(FaultCode::TypeMismatch);
            };
            let [name, prefix] = items.as_slice() else {
                return Err(FaultCode::TypeMismatch);
            };
            let name = name.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let prefix = prefix.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let (Object::Str(name), Object::Str(prefix)) = (heap.get(name), heap.get(prefix))
            else {
                return Err(FaultCode::TypeMismatch);
            };
            host_roots.push((name.clone(), prefix.clone()));
        }
        Ok(HostArg::CompileEnv(HostCompileEnv {
            modules: host_modules,
            roots: host_roots,
        }))
    }

    fn host_compile_options(&self, vm: VmId, fields: &[Value]) -> Result<HostArg, FaultCode> {
        let [Value::Bool(is_main), Value::Bool(dynamic_result), Value::Bool(late_definitions), late_functions, late_classes] =
            fields
        else {
            return Err(FaultCode::TypeMismatch);
        };
        Ok(HostArg::CompileOptions(HostCompileOptions {
            is_main: *is_main,
            dynamic_result: *dynamic_result,
            late_definitions: *late_definitions,
            late_functions: self.host_string_list(vm, *late_functions)?,
            late_classes: self.host_string_list(vm, *late_classes)?,
        }))
    }

    fn host_syntax(&self, vm: VmId, fields: &[Value]) -> Result<HostArg, FaultCode> {
        let [source, records, Value::Int(index)] = fields else {
            return Err(FaultCode::TypeMismatch);
        };
        let index = u32::try_from(*index).map_err(|_| FaultCode::TypeMismatch)?;
        let heap = &self.machines[vm as usize].vm.heap;
        let source = source.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let records = records.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let Object::Str(source) = heap.get(source) else {
            return Err(FaultCode::TypeMismatch);
        };
        let Object::Bytes(records) = heap.get(records) else {
            return Err(FaultCode::TypeMismatch);
        };
        let records = records.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
        let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
            .map_err(|_| FaultCode::BadCast)?;
        let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
        if !matches!(
            record.class,
            lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
        ) {
            return Err(FaultCode::BadCast);
        }
        Ok(HostArg::Syntax {
            source: source.clone(),
            records,
            index,
        })
    }

    fn host_string_list(&self, vm: VmId, value: Value) -> Result<Vec<SharedText>, FaultCode> {
        let heap = &self.machines[vm as usize].vm.heap;
        let reference = value.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let Object::List { items, .. } = heap.get(reference) else {
            return Err(FaultCode::TypeMismatch);
        };
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(items.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for value in items {
            let reference = value.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Str(text) = heap.get(reference) else {
                return Err(FaultCode::TypeMismatch);
            };
            strings.push(text.clone());
        }
        Ok(strings)
    }

    /// Convert one live TCP resource to an opaque host token.
    pub(super) fn host_tcp_arg(
        &self,
        resource: u64,
        expected: crate::HostTcpKind,
    ) -> Result<HostArg, FaultCode> {
        let bound = self
            .bound_resources
            .get(&resource)
            .ok_or(FaultCode::TypeMismatch)?;
        let found = match bound.kind {
            crate::ResourceKind::TcpStream => crate::HostTcpKind::Stream,
            crate::ResourceKind::TcpListener => crate::HostTcpKind::Listener,
            _ => return Err(FaultCode::TypeMismatch),
        };
        if found != expected {
            return Err(FaultCode::TypeMismatch);
        }
        match bound.backing {
            ResourceBacking::Host(token) => {
                Ok(HostArg::Tcp(crate::HostTcpResource { kind: found, token }))
            }
            ResourceBacking::Driver(_) => Err(FaultCode::TypeMismatch),
        }
    }

    /// Decode and validate one guest socket address.
    pub(super) fn host_socket_address(
        &self,
        vm: VmId,
        fields: &[Value],
    ) -> Result<crate::HostSocketAddress, FaultCode> {
        let [Value::Obj(ip), Value::Int(port), Value::Int(flow_info), Value::Int(scope_id)] =
            fields
        else {
            return Err(FaultCode::TypeMismatch);
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let ip = match heap.get(*ip) {
            Object::Instance { class, fields, .. }
                if Some(*class) == self.core.ip_v4 && fields.len() == 1 =>
            {
                let Value::Obj(bytes) = fields[0] else {
                    return Err(FaultCode::TypeMismatch);
                };
                let Object::Bytes(bytes) = heap.get(bytes) else {
                    return Err(FaultCode::TypeMismatch);
                };
                let bytes: [u8; 4] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| FaultCode::TypeMismatch)?;
                crate::HostIpAddress::V4(bytes)
            }
            Object::Instance { class, fields, .. }
                if Some(*class) == self.core.ip_v6 && fields.len() == 1 =>
            {
                let Value::Obj(bytes) = fields[0] else {
                    return Err(FaultCode::TypeMismatch);
                };
                let Object::Bytes(bytes) = heap.get(bytes) else {
                    return Err(FaultCode::TypeMismatch);
                };
                let bytes: [u8; 16] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| FaultCode::TypeMismatch)?;
                crate::HostIpAddress::V6(bytes)
            }
            _ => return Err(FaultCode::TypeMismatch),
        };
        Ok(crate::HostSocketAddress {
            ip,
            port: u16::try_from(*port).map_err(|_| FaultCode::TypeMismatch)?,
            flow_info: u32::try_from(*flow_info).map_err(|_| FaultCode::TypeMismatch)?,
            scope_id: u32::try_from(*scope_id).map_err(|_| FaultCode::TypeMismatch)?,
        })
    }

    /// Walk one policy chain from a saved resolution position.
    ///
    /// The walk follows the parent chain. A cut world proves that chain
    /// acyclic, so the loop terminates. The step bound is a second
    /// defense: a chain longer than the machine table has a cycle,
    /// whatever built the state, so the walk fails closed rather than
    /// spins.
    pub(super) fn resolve_policy(&self, cursor: PolicyCursor, op: u32) -> Resolution {
        let mut cur = match cursor {
            PolicyCursor::Table(vm) => vm,
            PolicyCursor::Root => return Resolution::Root,
        };
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps > self.machines.len() {
                // A well-formed chain visits each machine once, so this
                // is a cycle. Fail closed.
                return Resolution::Denied;
            }
            let m = &self.machines[cur as usize];
            match m.table.lookup(op) {
                None | Some(Action::Block) => return Resolution::Denied,
                Some(Action::Mock(closure)) => {
                    return Resolution::Mock {
                        owner: cur,
                        closure,
                    };
                }
                Some(Action::Pass) => {
                    let next = match m.vm.parent {
                        Some(parent) => PolicyCursor::Table(parent),
                        None => PolicyCursor::Root,
                    };
                    if m.driven {
                        return Resolution::Driver {
                            surface: cur,
                            cursor: next,
                        };
                    }
                    match m.vm.parent {
                        Some(parent) => cur = parent,
                        None if matches!(
                            m.vm.state,
                            MachineState::Done | MachineState::Faulted
                        ) =>
                        {
                            return Resolution::Denied;
                        }
                        None => return Resolution::Root,
                    }
                }
            }
        }
    }

    /// Run one mock handler in an ephemeral machine on the same loop.
    pub(super) fn start_mock(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        owner: VmId,
        closure: ObjRef,
    ) {
        if !self.share_heap_budget() {
            let op = self.pending_op(vm);
            self.machines[vm as usize].set_fault(
                FaultCode::HeapLimit,
                "the aggregate heap cannot hold the root machine",
                op,
            );
            return;
        }
        let mock_config = VmConfig {
            fuel: MOCK_FUEL,
            ..self.config
        };
        // Reuse a retired mock slot before the table grows.
        let id = match self.mock_free.pop() {
            Some(id) => {
                self.machines[id as usize] = self.empty_machine(mock_config, None, 0);
                id
            }
            None => {
                if !self.has_machine_room(1) || self.machines.try_reserve(1).is_err() {
                    let op = self.pending_op(vm);
                    self.machines[vm as usize].set_fault(
                        FaultCode::BoundaryLimit,
                        "the world machine limit stopped the mock",
                        op,
                    );
                    return;
                }
                let id = self.machines.len() as VmId;
                let machine = self.empty_machine(mock_config, None, 0);
                self.machines.push(machine);
                id
            }
        };
        let moved = if owner == vm {
            // The mock lives in the performing machine's own table.
            self.transfer(vm, id, Value::Obj(closure))
        } else {
            self.transfer(owner, id, Value::Obj(closure))
        };
        let closure_value = match moved {
            Ok(value) => value,
            Err(code) => {
                // The mock never started, so its slot returns at once.
                self.retire_mock(id);
                let op = self.pending_op(vm);
                self.machines[vm as usize].set_fault(code, "the mock handler is not sendable", op);
                return;
            }
        };
        let args: Vec<Value> = match self.machines[vm as usize].vm.pending.as_ref() {
            Some(pending) => pending.args.clone(),
            None => {
                self.retire_mock(id);
                self.machines[vm as usize].set_fault(
                    FaultCode::MalformedState,
                    "the mocked perform holds no request",
                    None,
                );
                return;
            }
        };
        // The handler is not reachable from the mock machine yet, so
        // it stays rooted while the arguments cross.
        let Some(closure_ref) = closure_value.as_obj() else {
            self.retire_mock(id);
            let op = self.pending_op(vm);
            self.machines[vm as usize].set_fault(
                FaultCode::TypeMismatch,
                "the mock handler is not a closure",
                op,
            );
            return;
        };
        self.machines[id as usize]
            .vm
            .heap
            .push_host_root(closure_ref);
        let moved = self.transfer_all(vm, id, &args);
        self.machines[id as usize]
            .vm
            .heap
            .pop_host_root(closure_ref);
        let moved_args = match moved {
            Ok(values) => values,
            Err(code) => {
                // The mock never started, so its slot returns at once.
                self.retire_mock(id);
                let op = self.pending_op(vm);
                self.machines[vm as usize].set_fault(
                    code,
                    "an operation argument is not sendable",
                    op,
                );
                return;
            }
        };
        // The handler closure carries the environment of the frame
        // that built it, and the mock body runs under exactly that
        // environment.
        let (func, env) = match self.machines[id as usize].vm.heap.get(closure_ref) {
            Object::Closure { func, env, .. } => (*func, env.env()),
            _ => {
                self.retire_mock(id);
                let op = self.pending_op(vm);
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the mock handler is not a closure",
                    op,
                );
                return;
            }
        };
        self.machines[id as usize].load_frame(
            &self.module,
            func,
            moved_args,
            Some(closure_ref),
            env,
        );
        self.push_activation(
            stack,
            Activation {
                vm: id,
                mode: StopMode::RunToTerminal,
                family: Family::Mock,
                reply_to: Some(vm),
                retired: false,
                fuel: None,
            },
        );
    }
}
