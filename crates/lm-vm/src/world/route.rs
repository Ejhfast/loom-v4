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

    /// Prepare one exact operation as a selectable source.
    pub(super) fn handle_prepare_wait(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        op: u32,
        argc: u32,
        reply_ty: u32,
        env: TypeEnvId,
    ) -> Option<RootEvent> {
        if self
            .loaded
            .bundle()
            .op(op)
            .is_none_or(|operation| !operation.wait_source)
        {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the operation is not a wait source",
                Some(op),
            );
            return None;
        }
        let m = &mut self.machines[vm as usize];
        let args = match m.take_arguments(argc) {
            Ok(args) => args,
            Err(code) => {
                m.set_fault(code, "the wait source has a short stack", Some(op));
                return None;
            }
        };
        let ordinal = match m.take_request_ordinal() {
            Ok(ordinal) => ordinal,
            Err(code) => {
                m.set_fault(code, "the request ordinal is exhausted", Some(op));
                return None;
            }
        };
        if let Err(code) =
            m.resources
                .register(crate::ResourceKind::PendingOperation, vm, 0, ordinal, op)
        {
            m.set_fault(
                code,
                "the machine reached its host resource limit",
                Some(op),
            );
            return None;
        }
        m.vm.pending = Some(Pending { op, args, ordinal });
        m.preparing_wait = Some(WaitPreparation { op, reply_ty, env });
        let Some(top) = stack.last().copied() else {
            return Some(self.fault_event(vm, "the performing machine left the driver stack"));
        };
        debug_assert_eq!(top.vm, vm);
        if top.mode == StopMode::DriveToAsk {
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
                    format!(
                        "the operation {} is not granted",
                        self.loaded
                            .bundle()
                            .op_name(op)
                            .unwrap_or("<invalid operation>")
                    ),
                    Some(op),
                );
            }
            Resolution::Mock { owner, closure } => self.start_mock(stack, vm, owner, closure),
            Resolution::Driver { surface, cursor } => {
                return self.route_request(stack, surface, vm, cursor, dispatch_mode);
            }
            Resolution::Root => {
                if self
                    .loaded
                    .bundle()
                    .op(op)
                    .is_some_and(|operation| operation.kind == lm_abi::OpKind::VmControl)
                {
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
                                ResourceBacking::Host(_) | ResourceBacking::Extension(_) => None,
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
                    let moved = match self.host_move_resources(vm, op) {
                        Ok(resources) => resources,
                        Err(code) => {
                            self.fault_caller(vm, op, code, "a moved host resource is invalid");
                            return None;
                        }
                    };
                    if self.machines[vm as usize].preparing_wait.is_none()
                        && self.machines[vm as usize]
                            .resources
                            .prepare_register()
                            .is_err()
                    {
                        self.machines[vm as usize].set_fault(
                            FaultCode::HostFault,
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
                    let started = if self.machines[vm as usize].preparing_wait.is_some() {
                        self.host.start_wait(completion, op, args)
                    } else {
                        self.host.start(completion, op, args)
                    };
                    if !matches!(started, HostStart::Failed(_)) {
                        for resource in moved {
                            self.retire_resource(resource, false);
                        }
                    }
                    match started {
                        HostStart::Completed(reply) => self.install_host_reply(vm, reply),
                        HostStart::Waiting(token) => {
                            if self.machines[vm as usize].preparing_wait.is_some() {
                                self.start_prepared_host_wait(vm, op, token);
                            } else {
                                self.start_wait(vm, op, token);
                            }
                        }
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
        let operation = self
            .loaded
            .bundle()
            .op(op)
            .expect("verified code names an operation");
        if !operation.suspends() {
            self.machines[vm as usize].set_fault(
                FaultCode::HostFault,
                format!(
                    "the host suspended {}, which the manifest declares machine state",
                    operation.name
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
        if pending.op >= lm_abi::OP_COUNT {
            let operation = self
                .loaded
                .bundle()
                .op(pending.op)
                .ok_or(FaultCode::MalformedState)?;
            if operation.params.len() != pending.args.len() {
                return Err(FaultCode::MalformedState);
            }
            return pending
                .args
                .iter()
                .zip(&operation.params)
                .map(|(value, ty)| self.host_data_arg(vm, *value, *ty))
                .collect();
        }
        pending
            .args
            .iter()
            .map(|value| match value {
                Value::Int(v) => Ok(HostArg::Int(*v)),
                Value::Float(bits) => Ok(HostArg::Float(*bits)),
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
                            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                                Err(FaultCode::TypeMismatch)
                            }
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
                            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                                Err(FaultCode::TypeMismatch)
                            }
                        }
                    }
                    Object::NativeRawMode { resource } => {
                        let bound = self
                            .bound_resources
                            .get(resource)
                            .ok_or(FaultCode::TypeMismatch)?;
                        if bound.kind != crate::ResourceKind::RawMode {
                            return Err(FaultCode::TypeMismatch);
                        }
                        match bound.backing {
                            ResourceBacking::Host(token) => Ok(HostArg::RawMode(token)),
                            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                                Err(FaultCode::TypeMismatch)
                            }
                        }
                    }
                    Object::NativeSignalStream { resource } => {
                        let bound = self
                            .bound_resources
                            .get(resource)
                            .ok_or(FaultCode::TypeMismatch)?;
                        if bound.kind != crate::ResourceKind::SignalStream {
                            return Err(FaultCode::TypeMismatch);
                        }
                        match bound.backing {
                            ResourceBacking::Host(token) => Ok(HostArg::SignalStream(token)),
                            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                                Err(FaultCode::TypeMismatch)
                            }
                        }
                    }
                    Object::NativePipeReader { resource } => self
                        .host_pipe_token(vm, *resource, crate::ResourceKind::PipeReader)
                        .map(HostArg::PipeReader),
                    Object::NativePipeWriter { resource } => self
                        .host_pipe_token(vm, *resource, crate::ResourceKind::PipeWriter)
                        .map(HostArg::PipeWriter),
                    Object::NativeChild { resource } => self
                        .host_pipe_token(vm, *resource, crate::ResourceKind::Child)
                        .map(HostArg::Child),
                    Object::NativeUdpSocket { resource } => self
                        .host_pipe_token(vm, *resource, crate::ResourceKind::UdpSocket)
                        .map(HostArg::Udp),
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.exec_spec =>
                    {
                        self.host_exec_spec(vm, fields).map(HostArg::ExecSpec)
                    }
                    Object::List { items, .. } => {
                        let mut values = Vec::with_capacity(items.len());
                        for item in items {
                            let Value::Obj(reference) = item else {
                                return Err(FaultCode::TypeMismatch);
                            };
                            match m.vm.heap.get(*reference) {
                                Object::Bytes(bytes) => values.push(HostArg::Bytes(
                                    bytes.try_bounded().map_err(|_| FaultCode::HeapLimit)?,
                                )),
                                Object::Instance { class, fields, .. }
                                    if Some(*class) == self.core.signal_interrupt
                                        && fields.is_empty() =>
                                {
                                    values.push(HostArg::SignalKind(HostSignalKind::Interrupt));
                                }
                                Object::Instance { class, fields, .. }
                                    if Some(*class) == self.core.signal_terminate
                                        && fields.is_empty() =>
                                {
                                    values.push(HostArg::SignalKind(HostSignalKind::Terminate));
                                }
                                _ => return Err(FaultCode::TypeMismatch),
                            }
                        }
                        Ok(HostArg::List(values))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.std_stream_input && fields.is_empty() =>
                    {
                        Ok(HostArg::StdStream(HostStdStream::Input))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.std_stream_output && fields.is_empty() =>
                    {
                        Ok(HostArg::StdStream(HostStdStream::Output))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.std_stream_error && fields.is_empty() =>
                    {
                        Ok(HostArg::StdStream(HostStdStream::Error))
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
                        if Some(*class) == self.core.open_create_new && fields.is_empty() =>
                    {
                        Ok(HostArg::OpenOptions(HostOpenOptions::CreateNew))
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
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.rename_no_replace && fields.is_empty() =>
                    {
                        Ok(HostArg::RenameMode(crate::HostRenameMode::NoReplace))
                    }
                    Object::Instance { class, fields, .. }
                        if Some(*class) == self.core.rename_replace && fields.is_empty() =>
                    {
                        Ok(HostArg::RenameMode(crate::HostRenameMode::Replace))
                    }
                    _ => Err(FaultCode::TypeMismatch),
                },
                _ => Err(FaultCode::TypeMismatch),
            })
            .collect()
    }

    fn host_pipe_token(
        &self,
        _vm: VmId,
        resource: u64,
        kind: crate::ResourceKind,
    ) -> Result<u64, FaultCode> {
        let bound = self
            .bound_resources
            .get(&resource)
            .ok_or(FaultCode::TypeMismatch)?;
        if bound.kind != kind {
            return Err(FaultCode::TypeMismatch);
        }
        match bound.backing {
            ResourceBacking::Host(token) => Ok(token),
            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                Err(FaultCode::TypeMismatch)
            }
        }
    }

    fn host_exec_spec(&self, vm: VmId, fields: &[Value]) -> Result<HostExecSpec, FaultCode> {
        let [program, arguments, directory, environment, input, output, error] = fields else {
            return Err(FaultCode::TypeMismatch);
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let text = |value: Value| match value.as_obj().map(|reference| heap.get(reference)) {
            Some(Object::Str(value)) => Ok(value.clone()),
            _ => Err(FaultCode::TypeMismatch),
        };
        let program = text(*program)?;
        let arguments = match arguments.as_obj().map(|reference| heap.get(reference)) {
            Some(Object::List { items, .. }) => items
                .iter()
                .map(|value| text(*value))
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(FaultCode::TypeMismatch),
        };
        let directory = if matches!(directory, Value::EmptyCase { arm: 1, .. }) {
            None
        } else {
            Some(text(*directory)?)
        };
        let environment = match environment.as_obj().map(|reference| heap.get(reference)) {
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_env_inherit && fields.is_empty() =>
            {
                HostChildEnv::Inherit
            }
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_env_exact && fields.len() == 1 =>
            {
                let Some(Object::Map { entries, .. }) =
                    fields[0].as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                let mut values = Vec::new();
                values
                    .try_reserve(entries.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for entry in entries.iter().filter(|entry| entry.is_live()) {
                    values.push((text(entry.key)?, text(entry.value)?));
                }
                HostChildEnv::Exact(values)
            }
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_env_overlay && fields.len() == 1 =>
            {
                let Some(Object::Map { entries, .. }) =
                    fields[0].as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                let mut values = Vec::new();
                values
                    .try_reserve(entries.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for entry in entries.iter().filter(|entry| entry.is_live()) {
                    values.push((text(entry.key)?, text(entry.value)?));
                }
                HostChildEnv::Overlay(values)
            }
            _ => return Err(FaultCode::TypeMismatch),
        };
        let input = match input.as_obj().map(|reference| heap.get(reference)) {
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_input_inherit && fields.is_empty() =>
            {
                HostChildInput::Inherit
            }
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_input_null && fields.is_empty() =>
            {
                HostChildInput::Null
            }
            Some(Object::Instance { class, fields, .. })
                if Some(*class) == self.core.child_input_pipe && fields.len() == 1 =>
            {
                let Some(Object::NativePipeReader { resource }) =
                    fields[0].as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                HostChildInput::Pipe(self.host_pipe_token(
                    vm,
                    *resource,
                    crate::ResourceKind::PipeReader,
                )?)
            }
            _ => return Err(FaultCode::TypeMismatch),
        };
        let output_value = |value: Value| -> Result<HostChildOutput, FaultCode> {
            match value.as_obj().map(|reference| heap.get(reference)) {
                Some(Object::Instance { class, fields, .. })
                    if Some(*class) == self.core.child_output_inherit && fields.is_empty() =>
                {
                    Ok(HostChildOutput::Inherit)
                }
                Some(Object::Instance { class, fields, .. })
                    if Some(*class) == self.core.child_output_null && fields.is_empty() =>
                {
                    Ok(HostChildOutput::Null)
                }
                Some(Object::Instance { class, fields, .. })
                    if Some(*class) == self.core.child_output_pipe && fields.len() == 1 =>
                {
                    let Some(Object::NativePipeWriter { resource }) =
                        fields[0].as_obj().map(|reference| heap.get(reference))
                    else {
                        return Err(FaultCode::TypeMismatch);
                    };
                    Ok(HostChildOutput::Pipe(self.host_pipe_token(
                        vm,
                        *resource,
                        crate::ResourceKind::PipeWriter,
                    )?))
                }
                _ => Err(FaultCode::TypeMismatch),
            }
        };
        Ok(HostExecSpec {
            program,
            arguments,
            directory,
            environment,
            input,
            output: output_value(*output)?,
            error: output_value(*error)?,
        })
    }

    fn host_data_arg(
        &self,
        vm: VmId,
        value: Value,
        ty: lm_abi::AbiType,
    ) -> Result<HostArg, FaultCode> {
        use lm_abi::{AbiConstructor, AbiPrimitive, AbiType};
        let heap = &self.machines[vm as usize].vm.heap;
        match ty {
            AbiType::Primitive(AbiPrimitive::Unit) if value == Value::Unit => Ok(HostArg::Unit),
            AbiType::Primitive(AbiPrimitive::Bool) => match value {
                Value::Bool(value) => Ok(HostArg::Bool(value)),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Primitive(AbiPrimitive::Int) => match value {
                Value::Int(value) => Ok(HostArg::Int(value)),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Primitive(AbiPrimitive::Float) => match value {
                Value::Float(bits) => Ok(HostArg::Float(bits)),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Primitive(AbiPrimitive::String) => match value.as_obj().map(|r| heap.get(r)) {
                Some(Object::Str(value)) => Ok(HostArg::Str(value.clone())),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Primitive(AbiPrimitive::Bytes) => match value.as_obj().map(|r| heap.get(r)) {
                Some(Object::Bytes(value)) => value
                    .try_bounded()
                    .map(HostArg::Bytes)
                    .map_err(|_| FaultCode::HeapLimit),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Resource(identity) => match value.as_obj().map(|r| heap.get(r)) {
                Some(Object::NativeHostResource { kind, resource }) if *kind == identity => {
                    let bound = self
                        .bound_resources
                        .get(resource)
                        .ok_or(FaultCode::TypeMismatch)?;
                    if bound.owner != vm || bound.kind != crate::ResourceKind::Extension(identity) {
                        return Err(FaultCode::TypeMismatch);
                    }
                    match bound.backing {
                        ResourceBacking::Extension(resource) => Ok(HostArg::Resource(resource)),
                        _ => Err(FaultCode::TypeMismatch),
                    }
                }
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::List(element) => match value.as_obj().map(|r| heap.get(r)) {
                Some(Object::List { items, .. }) => items
                    .iter()
                    .map(|item| self.host_data_arg(vm, *item, *element))
                    .collect::<Result<Vec<_>, _>>()
                    .map(HostArg::List),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Tuple(elements) => match value.as_obj().map(|r| heap.get(r)) {
                Some(Object::Tuple { items }) if items.len() == elements.len() => items
                    .iter()
                    .zip(elements)
                    .map(|(item, element)| self.host_data_arg(vm, *item, *element))
                    .collect::<Result<Vec<_>, _>>()
                    .map(HostArg::Tuple),
                _ => Err(FaultCode::TypeMismatch),
            },
            AbiType::Apply(AbiConstructor::Option, arguments) if arguments.len() == 1 => {
                if matches!(value, Value::EmptyCase { arm: 1, .. }) {
                    return Ok(HostArg::Option(None));
                }
                self.host_data_arg(vm, value, arguments[0])
                    .map(Box::new)
                    .map(Some)
                    .map(HostArg::Option)
            }
            AbiType::Apply(AbiConstructor::Result, arguments) if arguments.len() == 2 => {
                let Some(Object::Instance { class, fields, .. }) =
                    value.as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                let [payload] = fields.as_slice() else {
                    return Err(FaultCode::TypeMismatch);
                };
                if Some(*class) == self.core.result_ok {
                    self.host_data_arg(vm, *payload, arguments[0])
                        .map(Box::new)
                        .map(Ok)
                        .map(HostArg::Result)
                } else if Some(*class) == self.core.result_err {
                    self.host_data_arg(vm, *payload, arguments[1])
                        .map(Box::new)
                        .map(Err)
                        .map(HostArg::Result)
                } else {
                    Err(FaultCode::TypeMismatch)
                }
            }
            _ => Err(FaultCode::TypeMismatch),
        }
    }

    fn host_move_resources(&self, vm: VmId, op: u32) -> Result<Vec<u64>, FaultCode> {
        let pending = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .ok_or(FaultCode::MalformedState)?;
        if op < lm_abi::OP_COUNT {
            return Ok(Vec::new());
        }
        let operation = self
            .loaded
            .bundle()
            .op(op)
            .ok_or(FaultCode::MalformedState)?;
        let mut resources = Vec::new();
        for ((value, ty), mode) in pending
            .args
            .iter()
            .zip(&operation.params)
            .zip(&operation.param_modes)
        {
            if *mode == lm_abi::BoundaryMode::Move {
                self.collect_host_resources(vm, *value, *ty, &mut resources)?;
            }
        }
        resources.sort_unstable();
        let original = resources.len();
        resources.dedup();
        if resources.len() != original {
            return Err(FaultCode::BoundaryViolation);
        }
        Ok(resources)
    }

    fn collect_host_resources(
        &self,
        vm: VmId,
        value: Value,
        ty: lm_abi::AbiType,
        out: &mut Vec<u64>,
    ) -> Result<(), FaultCode> {
        use lm_abi::{AbiConstructor, AbiType};
        let heap = &self.machines[vm as usize].vm.heap;
        match ty {
            AbiType::Resource(identity) => {
                let Some(Object::NativeHostResource { kind, resource }) =
                    value.as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                if *kind != identity || !self.bound_resources.contains_key(resource) {
                    return Err(FaultCode::TypeMismatch);
                }
                out.push(*resource);
            }
            AbiType::List(element) => {
                let Some(Object::List { items, .. }) =
                    value.as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                for item in items {
                    self.collect_host_resources(vm, *item, *element, out)?;
                }
            }
            AbiType::Tuple(elements) => {
                let Some(Object::Tuple { items }) =
                    value.as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                if items.len() != elements.len() {
                    return Err(FaultCode::TypeMismatch);
                }
                for (item, element) in items.iter().zip(elements) {
                    self.collect_host_resources(vm, *item, *element, out)?;
                }
            }
            AbiType::Apply(AbiConstructor::Option, arguments) if arguments.len() == 1 => {
                if !matches!(value, Value::EmptyCase { arm: 1, .. }) {
                    self.collect_host_resources(vm, value, arguments[0], out)?;
                }
            }
            AbiType::Apply(AbiConstructor::Result, arguments) if arguments.len() == 2 => {
                let Some(Object::Instance { class, fields, .. }) =
                    value.as_obj().map(|reference| heap.get(reference))
                else {
                    return Err(FaultCode::TypeMismatch);
                };
                let [payload] = fields.as_slice() else {
                    return Err(FaultCode::TypeMismatch);
                };
                let ty = if Some(*class) == self.core.result_ok {
                    arguments[0]
                } else if Some(*class) == self.core.result_err {
                    arguments[1]
                } else {
                    return Err(FaultCode::TypeMismatch);
                };
                self.collect_host_resources(vm, *payload, ty, out)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn host_compile_env(&self, vm: VmId, fields: &[Value]) -> Result<HostArg, FaultCode> {
        let [modules, roots, definitions] = fields else {
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

        let definitions = definitions.as_obj().ok_or(FaultCode::TypeMismatch)?;
        let Object::List {
            items: definitions, ..
        } = heap.get(definitions)
        else {
            return Err(FaultCode::TypeMismatch);
        };
        let mut host_definitions = Vec::new();
        host_definitions
            .try_reserve_exact(definitions.len())
            .map_err(|_| FaultCode::HeapLimit)?;
        for definition in definitions {
            let pair = definition.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Tuple { items } = heap.get(pair) else {
                return Err(FaultCode::TypeMismatch);
            };
            let [local_name, definition] = items.as_slice() else {
                return Err(FaultCode::TypeMismatch);
            };
            let local_name = local_name.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let definition = definition.as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Str(local_name) = heap.get(local_name) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::Instance { class, fields, .. } = heap.get(definition) else {
                return Err(FaultCode::TypeMismatch);
            };
            if Some(*class) != self.core.definition_spec || fields.len() != 3 {
                return Err(FaultCode::TypeMismatch);
            }
            let identity = fields[0].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let module_hash = fields[1].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let slots = fields[2].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Instance {
                class,
                fields: identity,
                ..
            } = heap.get(identity)
            else {
                return Err(FaultCode::TypeMismatch);
            };
            if Some(*class) != self.core.definition_identity || identity.len() != 4 {
                return Err(FaultCode::TypeMismatch);
            }
            let module_name = identity[0].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let qualified_key = identity[1].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let contract_hash = identity[2].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let implementation_hash = identity[3].as_obj().ok_or(FaultCode::TypeMismatch)?;
            let Object::Str(module_name) = heap.get(module_name) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::Str(qualified_key) = heap.get(qualified_key) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::NativeDigest(contract_hash) = heap.get(contract_hash) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::NativeDigest(implementation_hash) = heap.get(implementation_hash) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::NativeDigest(module_hash) = heap.get(module_hash) else {
                return Err(FaultCode::TypeMismatch);
            };
            let Object::List { items: slots, .. } = heap.get(slots) else {
                return Err(FaultCode::TypeMismatch);
            };
            let mut host_slots = Vec::new();
            host_slots
                .try_reserve_exact(slots.len())
                .map_err(|_| FaultCode::HeapLimit)?;
            for slot in slots {
                let slot = slot.as_obj().ok_or(FaultCode::TypeMismatch)?;
                let Object::NativeCode(code) = heap.get(slot) else {
                    return Err(FaultCode::TypeMismatch);
                };
                if code.kind != lm_heap::PortableCodeKind::SlotSpec {
                    return Err(FaultCode::TypeMismatch);
                }
                host_slots.push(HostCompileSlot {
                    artifact: code.bytes.try_bounded().map_err(|_| FaultCode::HeapLimit)?,
                    interface: code
                        .interface
                        .as_ref()
                        .map(|bytes| bytes.try_bounded())
                        .transpose()
                        .map_err(|_| FaultCode::HeapLimit)?,
                    index: code.index,
                });
            }
            host_definitions.push(HostCompileDefinition {
                local_name: local_name.clone(),
                module_name: module_name.clone(),
                qualified_key: qualified_key.clone(),
                contract_hash: *contract_hash,
                implementation_hash: *implementation_hash,
                module_hash: *module_hash,
                slots: host_slots,
            });
        }
        Ok(HostArg::CompileEnv(HostCompileEnv {
            modules: host_modules,
            roots: host_roots,
            definitions: host_definitions,
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
            ResourceBacking::Driver(_) | ResourceBacking::Extension(_) => {
                Err(FaultCode::TypeMismatch)
            }
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
                self.machines[id as usize] = self.empty_machine(mock_config, None, 0).into();
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
                self.machines.push(machine.into());
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
