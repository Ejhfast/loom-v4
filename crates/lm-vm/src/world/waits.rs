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
        if machine.vm.waits.len() >= MAX_LIVE_WAITS || machine.vm.next_wait == u64::MAX {
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
        let mut stack = vec![(root, Vec::new(), true)];
        while let Some((token, path, is_root)) = stack.pop() {
            if tokens.contains(&token) || tokens.len() >= MAX_LIVE_WAITS {
                return Err(FaultCode::MalformedState);
            }
            let Some(entry) = waits.get(&token) else {
                return Err(FaultCode::MalformedState);
            };
            if entry.linked == is_root {
                return Err(FaultCode::MalformedState);
            }
            tokens.push(token);
            match entry.source {
                WaitSource::Receive => leaves.push(WaitLeafPath {
                    leaf: WaitLeaf::Receive,
                    path,
                }),
                WaitSource::Drive { target } => leaves.push(WaitLeafPath {
                    leaf: WaitLeaf::Drive { target },
                    path,
                }),
                WaitSource::Choice { first, second } => {
                    if first == second {
                        return Err(FaultCode::MalformedState);
                    }
                    let mut second_path = path.clone();
                    second_path.push(true);
                    stack.push((second, second_path, false));
                    let mut first_path = path;
                    first_path.push(false);
                    stack.push((first, first_path, false));
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
            lm_abi::OP_WAIT_CANCEL => {
                let Some(token) = self.wait_token(vm, op, args[0]) else {
                    return;
                };
                if !self.wait_root_is_current(vm, op, token) {
                    return;
                }
                let Ok((_, tokens)) = self.wait_tree(vm, token) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::MalformedState,
                        "the wait tree is malformed",
                    );
                    return;
                };
                self.retire_wait_tree(vm, &tokens);
                self.install_value_reply(vm, Value::Bool(true));
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

    pub(super) fn validate_wait_leases(
        &mut self,
        vm: VmId,
        op: u32,
        leaves: &[WaitLeafPath],
    ) -> bool {
        let mut drives = Vec::new();
        for leaf in leaves {
            let WaitLeaf::Drive { target } = leaf.leaf else {
                continue;
            };
            if drives.contains(&target) {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "a wait tree drives one machine more than once",
                );
                return false;
            }
            drives.push(target);
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
        self.machines[target as usize].vm.routed.is_some()
            || matches!(
                self.machines[target as usize].vm.state,
                MachineState::Asked | MachineState::Done | MachineState::Faulted
            )
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
        for leaf in leaves {
            let ready = match leaf.leaf {
                WaitLeaf::Receive => self.receive_wait_ready(vm),
                WaitLeaf::Drive { target } => self.drive_wait_ready(target),
            };
            if !ready {
                continue;
            }
            let built = match leaf.leaf {
                WaitLeaf::Receive => self.take_receive_wait_value(vm),
                WaitLeaf::Drive { target } => self.take_drive_wait_value(vm, target),
            }
            .and_then(|value| self.wrap_wait_choice(vm, value, &leaf.path));
            let mut seen = Vec::new();
            self.quiesce_wait_leases(vm, token, &mut seen);
            self.retire_wait_tree(vm, &tokens);
            self.machines[vm as usize].vm.block = None;
            match built {
                Ok(value) => self.install_value_reply(vm, value),
                Err(code) => {
                    self.machines[vm as usize].set_fault(code, "", Some(lm_abi::OP_WAIT_WAIT))
                }
            }
            return true;
        }
        false
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
            self.make_instance(vm, self.core.recv_msg, vec![value])
        } else if self.machines[vm as usize].vm.mailbox.closed {
            self.record(TraceEvent::Receive {
                proc: vm,
                closed: true,
            });
            self.make_instance(vm, self.core.recv_closed, vec![])
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
        self.make_instance(vm, self.core.drive_asked, vec![request])
    }

    pub(super) fn wrap_wait_choice(
        &mut self,
        vm: VmId,
        mut value: Value,
        path: &[bool],
    ) -> Result<Value, FaultCode> {
        for second in path.iter().rev() {
            let arm = if *second {
                self.core.choice_second
            } else {
                self.core.choice_first
            };
            value = self.make_instance(vm, arm, vec![value])?;
        }
        Ok(value)
    }

    // ------------------------------------------------------------
    // The snapshot operations of specification 23.5.
    // ------------------------------------------------------------
}
