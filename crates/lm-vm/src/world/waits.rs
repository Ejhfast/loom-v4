//! Selectable waits.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    pub(super) fn allocate_wait(
        &mut self,
        vm: VmId,
        source: WaitSource,
    ) -> Result<(u64, Value), FaultCode> {
        let machine = &self.machines[vm as usize];
        if machine.vm.waits.len() >= self.budget.limits.max_waits as usize
            || machine.vm.next_wait == u64::MAX
        {
            return Err(FaultCode::BoundaryLimit);
        }
        let token = machine.vm.next_wait;
        let value = self.machines[vm as usize].alloc(Object::NativeWait { owner: vm, token })?;
        let machine = &mut self.machines[vm as usize];
        machine.vm.next_wait += 1;
        machine.vm.waits.insert(
            token,
            WaitEntry {
                source,
                linked: false,
            },
        );
        Ok((token, value))
    }

    fn allocate_internal_wait(&mut self, vm: VmId, source: WaitSource) -> Result<u64, FaultCode> {
        let machine = &self.machines[vm as usize];
        if machine.vm.waits.len() >= self.budget.limits.max_waits as usize
            || machine.vm.next_wait == u64::MAX
        {
            return Err(FaultCode::BoundaryLimit);
        }
        let token = machine.vm.next_wait;
        let machine = &mut self.machines[vm as usize];
        machine.vm.next_wait += 1;
        machine.vm.waits.insert(
            token,
            WaitEntry {
                source,
                linked: false,
            },
        );
        Ok(token)
    }

    pub(super) fn create_wait(&mut self, vm: VmId, op: u32, source: WaitSource) {
        match self.allocate_wait(vm, source) {
            Ok((_, value)) => self.install_value_reply(vm, value),
            Err(code) => self.fault_caller(vm, op, code, "the wait limit is full"),
        }
    }

    pub(super) fn wait_token(&mut self, vm: VmId, op: u32, value: Value) -> Option<u64> {
        let found = value.as_obj().and_then(|reference| {
            match self.machines[vm as usize].vm.heap.get(reference) {
                Object::NativeWait { owner, token } => Some((*owner, *token)),
                _ => None,
            }
        });
        match found {
            Some((owner, token)) if owner == vm => Some(token),
            Some(_) => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the wait belongs to another machine",
                );
                None
            }
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the argument is not a wait token",
                );
                None
            }
        }
    }

    pub(super) fn wait_root_is_current(&mut self, vm: VmId, op: u32, token: u64) -> bool {
        match self.machines[vm as usize].vm.waits.get(&token) {
            Some(entry) if !entry.linked => true,
            Some(_) => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the wait token was consumed by a choice",
                );
                false
            }
            None => {
                self.fault_caller(vm, op, FaultCode::InvalidVmState, "the wait token is stale");
                false
            }
        }
    }

    pub(super) fn wait_tree(
        &self,
        vm: VmId,
        root: u64,
    ) -> Result<(Vec<WaitLeafPath>, Vec<u64>), FaultCode> {
        let waits = &self.machines[vm as usize].vm.waits;
        let mut leaves = Vec::new();
        let mut tokens = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let limit = self.budget.limits.max_waits as usize;
        let mut stack = vec![(root, Vec::new(), true, None)];
        while let Some((token, path, is_root, any_index)) = stack.pop() {
            if !seen.insert(token) || tokens.len() >= limit {
                return Err(FaultCode::MalformedState);
            }
            let Some(entry) = waits.get(&token) else {
                return Err(FaultCode::MalformedState);
            };
            if entry.linked == is_root {
                return Err(FaultCode::MalformedState);
            }
            tokens.push(token);
            match &entry.source {
                WaitSource::Receive => leaves.push(WaitLeafPath {
                    leaf: WaitLeaf::Receive,
                    path,
                    any_index,
                }),
                WaitSource::Drive { target } => leaves.push(WaitLeafPath {
                    leaf: WaitLeaf::Drive { target: *target },
                    path,
                    any_index,
                }),
                WaitSource::Operation {
                    op,
                    ordinal,
                    scope,
                    consume_resource,
                    reply_ty,
                    env,
                    ready,
                } => leaves.push(WaitLeafPath {
                    leaf: WaitLeaf::Operation {
                        op: *op,
                        ordinal: *ordinal,
                        scope: *scope,
                        consume_resource: *consume_resource,
                        reply_ty: *reply_ty,
                        env: *env,
                        ready: *ready,
                    },
                    path,
                    any_index,
                }),
                WaitSource::Choice { first, second } => {
                    if first == second {
                        return Err(FaultCode::MalformedState);
                    }
                    let mut second_path = path.clone();
                    second_path.push(true);
                    stack.push((*second, second_path, false, any_index));
                    let mut first_path = path;
                    first_path.push(false);
                    stack.push((*first, first_path, false, any_index));
                }
                WaitSource::Any { roots } => {
                    if roots.is_empty() || any_index.is_some() || !path.is_empty() {
                        return Err(FaultCode::MalformedState);
                    }
                    for (index, child) in roots.iter().enumerate().rev() {
                        stack.push((*child, Vec::new(), false, Some(index)));
                    }
                }
            }
        }
        if leaves.is_empty() {
            return Err(FaultCode::MalformedState);
        }
        Ok((leaves, tokens))
    }

    pub(super) fn retire_wait_tree(&mut self, vm: VmId, tokens: &[u64]) {
        for token in tokens {
            self.machines[vm as usize].vm.waits.remove(token);
        }
    }

    /// Finish a prepared operation whose guest path produced a value.
    pub(super) fn finish_prepared_guest_wait(&mut self, vm: VmId, value: Value) {
        let Some(preparation) = self.machines[vm as usize].preparing_wait else {
            return;
        };
        if let Err(code) = self.check_reply(vm, value) {
            self.machines[vm as usize].set_fault(
                code,
                "the reply does not carry the wait source type",
                Some(preparation.op),
            );
            return;
        }
        let Some(ordinal) = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| pending.ordinal)
        else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the wait preparation has no pending operation",
                Some(preparation.op),
            );
            return;
        };
        if self.machines[vm as usize].push(value).is_err() {
            self.machines[vm as usize].set_fault(FaultCode::HeapLimit, "", Some(preparation.op));
            return;
        }
        let built = self.allocate_wait(
            vm,
            WaitSource::Operation {
                op: preparation.op,
                ordinal,
                scope: 0,
                consume_resource: (preparation.op == lm_abi::OP_EXEC_WAIT)
                    .then(|| self.pending_resource_of(vm, ResourceErrors::Exec))
                    .flatten(),
                reply_ty: preparation.reply_ty,
                env: preparation.env,
                ready: Some(value),
            },
        );
        let _ = self.machines[vm as usize].vm.operands.pop();
        let Ok((_, wait)) = built else {
            self.machines[vm as usize].set_fault(
                FaultCode::BoundaryLimit,
                "the wait limit is full",
                Some(preparation.op),
            );
            return;
        };
        let machine = &mut self.machines[vm as usize];
        machine.vm.pending = None;
        machine.preparing_wait = None;
        if let Err(code) = machine.push(wait) {
            machine.set_fault(code, "", Some(preparation.op));
        } else if machine.vm.state != MachineState::Running {
            machine.vm.state = MachineState::Ready;
        }
        self.notify_task_state(vm);
    }

    /// Finish a prepared operation after the root host arms it.
    pub(super) fn start_prepared_host_wait(&mut self, vm: VmId, op: u32, scope: u64) {
        let Some(preparation) = self.machines[vm as usize].preparing_wait else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the host armed an operation without a wait preparation",
                Some(op),
            );
            return;
        };
        let Some(ordinal) = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| pending.ordinal)
        else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the host armed an operation without a pending request",
                Some(op),
            );
            return;
        };
        if !self.machines[vm as usize]
            .resources
            .set_pending_scope(ordinal, scope)
        {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the wait source has no resource record",
                Some(op),
            );
            return;
        }
        let built = self.allocate_wait(
            vm,
            WaitSource::Operation {
                op,
                ordinal,
                scope,
                consume_resource: (op == lm_abi::OP_EXEC_WAIT)
                    .then(|| self.pending_resource_of(vm, ResourceErrors::Exec))
                    .flatten(),
                reply_ty: preparation.reply_ty,
                env: preparation.env,
                ready: None,
            },
        );
        let Ok((_, wait)) = built else {
            let _ = self.host.cancel_wait(scope);
            self.machines[vm as usize]
                .resources
                .close_by_ordinal(ordinal);
            self.machines[vm as usize].set_fault(
                FaultCode::BoundaryLimit,
                "the wait limit is full",
                Some(op),
            );
            return;
        };
        let machine = &mut self.machines[vm as usize];
        machine.vm.pending = None;
        machine.preparing_wait = None;
        if let Err(code) = machine.push(wait) {
            machine.set_fault(code, "", Some(op));
        }
    }

    pub(super) fn wait_exec(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        match op {
            lm_abi::OP_PROC_RECV_WAIT => {
                if self.machines[vm as usize].owner != Ownership::Scheduler {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidVmState,
                        "receive_wait is valid only on a scheduler-owned proc",
                    );
                    return;
                }
                self.create_wait(vm, op, WaitSource::Receive);
            }
            lm_abi::OP_WAIT_CHOOSE => {
                let Some(first) = self.wait_token(vm, op, args[0]) else {
                    return;
                };
                let Some(second) = self.wait_token(vm, op, args[1]) else {
                    return;
                };
                if first == second {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidVmState,
                        "a choice cannot use one wait twice",
                    );
                    return;
                }
                if !self.wait_root_is_current(vm, op, first)
                    || !self.wait_root_is_current(vm, op, second)
                {
                    return;
                }
                let built = self.allocate_wait(vm, WaitSource::Choice { first, second });
                let Ok((_, value)) = built else {
                    self.fault_caller(vm, op, FaultCode::BoundaryLimit, "the wait limit is full");
                    return;
                };
                self.machines[vm as usize]
                    .vm
                    .waits
                    .get_mut(&first)
                    .expect("the checked first wait exists")
                    .linked = true;
                self.machines[vm as usize]
                    .vm
                    .waits
                    .get_mut(&second)
                    .expect("the checked second wait exists")
                    .linked = true;
                self.install_value_reply(vm, value);
            }
            lm_abi::OP_WAIT_ANY => {
                let Some(reference) = args[0].as_obj() else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a list of waits",
                    );
                    return;
                };
                let roots = match self.machines[vm as usize].vm.heap.get(reference) {
                    Object::List { items, .. } => items.clone(),
                    _ => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::TypeMismatch,
                            "the argument is not a list of waits",
                        );
                        return;
                    }
                };
                if roots.is_empty() {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidVmState,
                        "sys.wait.any needs at least one wait",
                    );
                    return;
                }
                if roots.len() >= self.budget.limits.max_waits as usize {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::BoundaryLimit,
                        "the dynamic wait set passes its limit",
                    );
                    return;
                }
                let mut tokens = Vec::with_capacity(roots.len());
                let mut seen = std::collections::BTreeSet::new();
                for value in roots {
                    let Some(token) = self.wait_token(vm, op, value) else {
                        return;
                    };
                    if !seen.insert(token) {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "a dynamic wait set cannot use one wait twice",
                        );
                        return;
                    }
                    if !self.wait_root_is_current(vm, op, token) {
                        return;
                    }
                    tokens.push(token);
                }
                let source = WaitSource::Any {
                    roots: Arc::from(tokens.clone()),
                };
                let root = match self.allocate_internal_wait(vm, source) {
                    Ok(root) => root,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the wait limit is full");
                        return;
                    }
                };
                for token in &tokens {
                    self.machines[vm as usize]
                        .vm
                        .waits
                        .get_mut(token)
                        .expect("the checked wait exists")
                        .linked = true;
                }
                let leaves = match self.wait_tree(vm, root) {
                    Ok((leaves, _)) => leaves,
                    Err(_) => {
                        self.rollback_dynamic_wait(vm, root, &tokens);
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::MalformedState,
                            "the dynamic wait tree is malformed",
                        );
                        return;
                    }
                };
                if !self.validate_wait_leases(vm, op, &leaves) {
                    self.rollback_dynamic_wait(vm, root, &tokens);
                    return;
                }
                if self.complete_ready_wait(vm, root) {
                    return;
                }
                self.block_machine(vm, Block::Wait { token: root });
            }
            lm_abi::OP_WAIT_CANCEL => {
                let Some(token) = self.wait_token(vm, op, args[0]) else {
                    return;
                };
                if !self.wait_root_is_current(vm, op, token) {
                    return;
                }
                let Ok((leaves, tokens)) = self.wait_tree(vm, token) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::MalformedState,
                        "the wait tree is malformed",
                    );
                    return;
                };
                let cancelled = self.cancel_wait_operations(vm, &leaves, None);
                self.retire_wait_tree(vm, &tokens);
                match cancelled {
                    Ok(()) => self.install_value_reply(vm, Value::Bool(true)),
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the host could not cancel a wait source")
                    }
                }
            }
            lm_abi::OP_WAIT_WAIT => {
                let Some(token) = self.wait_token(vm, op, args[0]) else {
                    return;
                };
                if !self.wait_root_is_current(vm, op, token) {
                    return;
                }
                let Ok((leaves, _)) = self.wait_tree(vm, token) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::MalformedState,
                        "the wait tree is malformed",
                    );
                    return;
                };
                if !self.validate_wait_leases(vm, op, &leaves) {
                    return;
                }
                if self.complete_ready_wait(vm, token) {
                    return;
                }
                self.block_machine(vm, Block::Wait { token });
            }
            _ => self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the operation has no wait rule",
            ),
        }
    }

    fn rollback_dynamic_wait(&mut self, vm: VmId, root: u64, children: &[u64]) {
        self.machines[vm as usize].vm.waits.remove(&root);
        for child in children {
            if let Some(entry) = self.machines[vm as usize].vm.waits.get_mut(child) {
                entry.linked = false;
            }
        }
    }

    pub(super) fn validate_wait_leases(
        &mut self,
        vm: VmId,
        op: u32,
        leaves: &[WaitLeafPath],
    ) -> bool {
        let mut drives = std::collections::BTreeSet::new();
        for leaf in leaves {
            let WaitLeaf::Drive { target } = leaf.leaf else {
                continue;
            };
            if !drives.insert(target) {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "a wait tree drives one machine more than once",
                );
                return false;
            }
            let valid = target != vm
                && self.machines.get(target as usize).is_some_and(|machine| {
                    machine.owner == Ownership::Holder
                        && machine.active == 0
                        && machine.vm.state != MachineState::Empty
                });
            if !valid {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "a drive wait names a machine that is not available",
                );
                return false;
            }
        }
        true
    }

    pub(super) fn receive_wait_ready(&self, vm: VmId) -> bool {
        let mailbox = &self.machines[vm as usize].vm.mailbox;
        !mailbox.queue.is_empty() || mailbox.closed
    }

    pub(super) fn drive_wait_ready(&self, target: VmId) -> bool {
        self.machines[target as usize].is_resident()
            && (self.machines[target as usize].vm.routed.is_some()
                || matches!(
                    self.machines[target as usize].vm.state,
                    MachineState::Asked | MachineState::Done | MachineState::Faulted
                ))
    }

    pub(super) fn complete_ready_wait(&mut self, vm: VmId, token: u64) -> bool {
        let Ok((leaves, tokens)) = self.wait_tree(vm, token) else {
            let op = self.pending_op(vm);
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the wait tree is malformed",
                op,
            );
            return true;
        };
        for (selected, leaf) in leaves.iter().enumerate() {
            let ready = match leaf.leaf {
                WaitLeaf::Receive => self.receive_wait_ready(vm),
                WaitLeaf::Drive { target } => self.drive_wait_ready(target),
                WaitLeaf::Operation { ordinal, ready, .. } => {
                    ready.is_some()
                        || self
                            .host_completions
                            .contains_key(&self.prepared_completion_key(vm, ordinal))
                }
            };
            if !ready {
                continue;
            }
            let built = match leaf.leaf {
                WaitLeaf::Receive => self.take_receive_wait_value(vm),
                WaitLeaf::Drive { target } => self.take_drive_wait_value(vm, target),
                operation @ WaitLeaf::Operation { .. } => {
                    self.take_operation_wait_value(vm, operation)
                }
            }
            .and_then(|value| self.wrap_wait_choice(vm, value, &leaf.path))
            .and_then(|value| {
                let op = self.pending_op(vm).ok_or(FaultCode::MalformedState)?;
                match (op, leaf.any_index) {
                    (lm_abi::OP_WAIT_WAIT, None) => Ok(value),
                    (lm_abi::OP_WAIT_ANY, Some(index)) => {
                        self.machines[vm as usize].alloc(Object::Tuple {
                            items: vec![Value::Int(index as i64), value],
                        })
                    }
                    _ => Err(FaultCode::MalformedState),
                }
            });
            let cancelled = self.cancel_wait_operations(vm, &leaves, Some(selected));
            let mut seen = Vec::new();
            self.quiesce_wait_leases(vm, token, &mut seen);
            self.retire_wait_tree(vm, &tokens);
            self.machines[vm as usize].vm.block = None;
            let built = built.and_then(|value| cancelled.map(|()| value));
            match built {
                Ok(value) => self.install_value_reply(vm, value),
                Err(code) => {
                    let op = self.pending_op(vm);
                    self.machines[vm as usize].set_fault(code, "", op)
                }
            }
            return true;
        }
        false
    }

    pub(super) fn prepared_completion_key(&self, vm: VmId, ordinal: u64) -> CompletionKey {
        CompletionKey {
            machine: TaskKey {
                vm,
                generation: self.machines[vm as usize].generation,
            },
            ordinal,
        }
    }

    pub(super) fn prepared_wait_exists(&self, key: CompletionKey) -> bool {
        let Some(machine) = self.machines.get(key.machine.vm as usize) else {
            return false;
        };
        if machine.generation() != key.machine.generation || !machine.is_resident() {
            return false;
        }
        if matches!(machine.vm.state, MachineState::Done | MachineState::Faulted) {
            return false;
        }
        machine.vm.waits.values().any(|entry| {
            matches!(
                entry.source,
                WaitSource::Operation { ordinal, .. } if ordinal == key.ordinal
            )
        })
    }

    pub(super) fn take_operation_wait_value(
        &mut self,
        vm: VmId,
        operation: WaitLeaf,
    ) -> Result<Value, FaultCode> {
        let WaitLeaf::Operation {
            op,
            ordinal,
            scope,
            consume_resource,
            reply_ty,
            env,
            ready,
        } = operation
        else {
            return Err(FaultCode::MalformedState);
        };
        let value = match ready {
            Some(value) => value,
            None => {
                let key = self.prepared_completion_key(vm, ordinal);
                let completion = self
                    .host_completions
                    .remove(&key)
                    .ok_or(FaultCode::MalformedState)?;
                if completion.token != scope {
                    return Err(FaultCode::HostFault);
                }
                if !self.host.commit_wait(scope) {
                    return Err(FaultCode::HostFault);
                }
                let result = completion.result.map_err(|_| FaultCode::HostFault)?;
                self.build_host_reply_value(vm, op, &result, reply_ty, env)?
            }
        };
        self.machines[vm as usize]
            .resources
            .close_by_ordinal(ordinal);
        if let Some(resource) = consume_resource {
            let consumed = self.value_is_result_ok(vm, value)
                || self.value_is_result_error_class(vm, value, self.core_of(vm).exec_error_closed);
            if consumed {
                self.retire_resource(resource, false);
            }
        }
        Ok(value)
    }

    pub(super) fn cancel_wait_operations(
        &mut self,
        vm: VmId,
        leaves: &[WaitLeafPath],
        selected: Option<usize>,
    ) -> Result<(), FaultCode> {
        let mut missing = false;
        for (index, leaf) in leaves.iter().enumerate() {
            if selected == Some(index) {
                continue;
            }
            let WaitLeaf::Operation { ordinal, scope, .. } = leaf.leaf else {
                continue;
            };
            let key = self.prepared_completion_key(vm, ordinal);
            self.host_completions.remove(&key);
            if scope != 0 && self.host.cancel_wait(scope) == HostWaitCancel::Missing {
                missing = true;
            }
            self.machines[vm as usize]
                .resources
                .close_by_ordinal(ordinal);
        }
        if missing {
            Err(FaultCode::HostFault)
        } else {
            Ok(())
        }
    }

    pub(super) fn take_receive_wait_value(&mut self, vm: VmId) -> Result<Value, FaultCode> {
        if let Some(value) = self.machines[vm as usize].vm.mailbox.pop() {
            if let Some(target) = self.task_key(vm) {
                self.emit_wake(WakeKey::Send(target));
            }
            self.record(TraceEvent::Receive {
                proc: vm,
                closed: false,
            });
            self.make_instance(vm, self.core_of(vm).recv_msg, vec![value])
        } else if self.machines[vm as usize].vm.mailbox.closed {
            self.record(TraceEvent::Receive {
                proc: vm,
                closed: true,
            });
            self.make_instance(vm, self.core_of(vm).recv_closed, vec![])
        } else {
            Err(FaultCode::MalformedState)
        }
    }

    pub(super) fn take_drive_wait_value(
        &mut self,
        vm: VmId,
        surface: VmId,
    ) -> Result<Value, FaultCode> {
        if let Some(route) = self.machines[surface as usize].vm.routed {
            return self.fresh_asked_wait_value(vm, route.target);
        }
        match self.machines[surface as usize].vm.state {
            MachineState::Asked => self.fresh_asked_wait_value(vm, surface),
            MachineState::Done | MachineState::Faulted => {
                self.build_terminal_event(surface, vm, Family::Drive)
            }
            _ => Err(FaultCode::MalformedState),
        }
    }

    pub(super) fn fresh_asked_wait_value(
        &mut self,
        vm: VmId,
        target: VmId,
    ) -> Result<Value, FaultCode> {
        if self.machines[target as usize].vm.pending.is_none() {
            return Err(FaultCode::MalformedState);
        }
        let fresh = self.machines[target as usize].take_request_ordinal()?;
        if let Some(pending) = self.machines[target as usize].vm.pending.as_mut() {
            pending.ordinal = fresh;
        }
        let request = self.machines[vm as usize].alloc(Object::NativeRequest {
            vm: target,
            ordinal: fresh,
        })?;
        self.make_instance(vm, self.core_of(vm).drive_asked, vec![request])
    }

    pub(super) fn wrap_wait_choice(
        &mut self,
        vm: VmId,
        mut value: Value,
        path: &[bool],
    ) -> Result<Value, FaultCode> {
        for second in path.iter().rev() {
            let arm = if *second {
                self.core_of(vm).choice_second
            } else {
                self.core_of(vm).choice_first
            };
            value = self.make_instance(vm, arm, vec![value])?;
        }
        Ok(value)
    }

    // ------------------------------------------------------------
    // The snapshot operations of specification 23.5.
    // ------------------------------------------------------------
}
