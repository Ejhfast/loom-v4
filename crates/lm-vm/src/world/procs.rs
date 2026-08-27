//! Proc operations and the VM control surface.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// Read one proc reference out of a handle value.
    ///
    /// The argument comes from the pending record, so the read tests
    /// the shape and the caller faults on `None`.
    pub(super) fn handle_proc(&self, holder: VmId, value: Value) -> Option<(VmId, u32)> {
        let r = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeHandle { proc, generation } => Some((*proc, *generation)),
            _ => None,
        }
    }

    /// The proc one argument names, or a fault on the caller.
    pub(super) fn proc_arg(&mut self, vm: VmId, op: u32, value: Value) -> Option<(VmId, u32)> {
        match self.handle_proc(vm, value) {
            Some(found) => Some(found),
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the receiver is not a proc handle",
                );
                None
            }
        }
    }

    /// True when the reference names a live machine slot.
    pub(super) fn proc_alive(&self, proc: VmId, generation: u32) -> bool {
        (proc as usize) < self.machines.len()
            && self.machines[proc as usize].generation() == generation
    }

    /// True when the reference names a machine that can still accept
    /// or answer: it exists, its generation matches, and it has not
    /// reached a terminal result.
    pub(super) fn proc_running(&self, proc: VmId, generation: u32) -> bool {
        self.proc_alive(proc, generation)
            && (!self.machines[proc as usize].is_resident()
                || !matches!(
                    self.machines[proc as usize].vm.state,
                    MachineState::Done | MachineState::Faulted
                ))
    }

    /// Allocate one frozen `Fault` value in `vm`.
    pub(super) fn make_fault(
        &mut self,
        vm: VmId,
        code: FaultCode,
        message: &str,
    ) -> Result<Value, FaultCode> {
        let trace = self.machines[vm as usize].execution_trace();
        self.machines[vm as usize].alloc(Object::NativeFault {
            code,
            message: message.to_string(),
            op: None,
            trace: trace.into_boxed_slice(),
        })
    }

    /// Install one built reply, or fault the caller when the build
    /// failed.
    pub(super) fn reply_or_fault(&mut self, vm: VmId, op: u32, built: Result<Value, FaultCode>) {
        match built {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    /// Block one machine on another machine of this world.
    pub(super) fn block_machine(&mut self, vm: VmId, block: Block) {
        let (kind, target) = match block {
            Block::Receive => (TraceBlock::Receive, 0),
            Block::Send { target, .. } => (TraceBlock::Send, target),
            Block::Done { target, .. } => (TraceBlock::Done, target),
            Block::Wait { .. } => (TraceBlock::Wait, 0),
            Block::Snapshot { target, .. } => (TraceBlock::Snapshot, target),
        };
        let m = &mut self.machines[vm as usize];
        m.vm.block = Some(block);
        m.vm.state = MachineState::Blocked;
        self.record(TraceEvent::Block { vm, kind, target });
    }

    /// Copy one value across a machine boundary.
    ///
    /// A crossing copies the value, so the receiver owns a fresh
    /// graph and the two machines share nothing (specification 16.1).
    ///
    /// A proc may hold its own handle, because a handle is sendable
    /// data. That send stays inside one heap, so it has no second
    /// heap to copy into. It owes the same rule, so it runs the
    /// one-heap copy, which carries the same `CopyCheck` visitor the
    /// two-heap copy carries. Without the copy the sender and the
    /// mailbox would share one mutable graph.
    pub(super) fn boundary_copy(
        &mut self,
        src: VmId,
        dst: VmId,
        value: Value,
    ) -> Result<Value, FaultCode> {
        if src != dst {
            return self.transfer(src, dst, value);
        }
        if let Some(result) = scalar_copy(value) {
            return result;
        }
        let limits = self.machines[dst as usize].config.graph;
        // The heap roots are read before the heap is borrowed: a
        // collection during the copy needs them.
        let roots = self.machines[dst as usize].gc_roots(&[]);
        let heap = &mut self.machines[dst as usize].vm.heap;
        lm_graph::copy_within(heap, &roots, value, &limits)
    }

    /// Execute one proc operation of the machine `vm`.
    pub(super) fn proc_exec(&mut self, vm: VmId, op: u32, stored: Vec<Value>) {
        // A restored machine states its own argument list, so a short
        // list reads as the uninitialized marker and every shape test
        // below rejects it.
        let args = Args(&stored);
        match op {
            lm_abi::OP_PROC_SPAWN => self.proc_spawn(vm, op, args),
            lm_abi::OP_PROC_RUN => self.proc_run(vm, op, args),
            lm_abi::OP_PROC_RUN_CLOSURE => self.proc_run_closure(vm, op, args),
            lm_abi::OP_PROC_SEND => self.proc_send(vm, op, args),
            lm_abi::OP_PROC_CLOSE => self.proc_close(vm, op, args),
            lm_abi::OP_PROC_RECV => self.proc_recv(vm, op),
            lm_abi::OP_PROC_DONE => self.proc_done(vm, op, args),
            lm_abi::OP_PROC_PAUSE => self.proc_pause(vm, op, args),
            lm_abi::OP_PROC_RESUME => self.proc_resume(vm, op, args),
            lm_abi::OP_PROC_SNAPSHOT_WAIT => self.proc_snapshot_wait(vm, op, args, None),
            // Every proc slot of the manifest has an arm above.
            _ => self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the operation has no proc rule",
            ),
        }
    }

    /// `Class.spawn(args...)`: build one proc machine, grant it the
    /// `Proc` group, and transfer its execution to the scheduler.
    ///
    /// The arguments are the constructor closure, the proc body
    /// closure, and the argument tuple. The proc instance is
    /// constructed inside its own machine (specification 18.1).
    pub(super) fn proc_spawn(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let child_config = match self.reserve_child(vm) {
            Some(config) => config,
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the parent has no child budget left",
                );
                return;
            }
        };
        let child = self.machines.len() as VmId;
        if let Err(code) = self.prepare_scheduler_proc(child) {
            self.machines[vm as usize].children -= 1;
            self.fault_caller(vm, op, code, "the scheduler has no task capacity");
            return;
        }
        let mut machine = self.empty_machine(child_config, Some(vm), 0);
        // An image proc reads the same slots as its spawner.
        // The link also keeps that image live.
        machine.image = self.machines[vm as usize].image;
        self.machines.push(machine.into());
        // The two closures and every argument cross the boundary. Each
        // result stays rooted while the next value crosses.
        let mut payload: Vec<Value> = vec![args[0], args[1]];
        if let Value::Obj(r) = args[2] {
            let items = match self.machines[vm as usize].vm.heap.get(r) {
                Object::Tuple { items } => items.clone(),
                _ => {
                    self.machines.pop();
                    self.machines[vm as usize].children -= 1;
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument view is not a tuple",
                    );
                    return;
                }
            };
            payload.extend(items);
        }
        let moved = match self.transfer_all(vm, child, &payload) {
            Ok(values) => values,
            Err(code) => {
                // Nothing names the child, so the whole call rolls
                // back: the record goes and the parent gets its
                // reservation back.
                self.machines.pop();
                self.machines[vm as usize].children -= 1;
                self.fault_caller(vm, op, code, "a spawn argument is not sendable");
                return;
            }
        };
        // The two closures come from the pending record, so a
        // restored machine can state another shape at either slot.
        let pair = moved[0].as_obj().zip(moved[1].as_obj()).and_then(|(c, b)| {
            let heap = &self.machines[child as usize].vm.heap;
            let ctor = match heap.get(c) {
                Object::Closure { func, env, .. } => (*func, env.env()),
                _ => return None,
            };
            let body = match heap.get(b) {
                Object::Closure { func, env, .. } => (*func, env.env()),
                _ => return None,
            };
            Some((c, b, ctor, body))
        });
        // The machine witness names the proc body, never the
        // constructor: the terminal result of a proc is the result of
        // its body, and the first parameter of the body is the proc
        // instance the mailbox type follows from.
        let Some((ctor, body, (func, ctor_env), (body_func, body_env))) = pair else {
            self.machines.pop();
            self.machines[vm as usize].children -= 1;
            self.fault_caller(
                vm,
                op,
                FaultCode::TypeMismatch,
                "a spawn program is not a closure",
            );
            return;
        };
        let ctor_args: Vec<Value> = moved[2..].to_vec();
        // The birth grant of specification 18.3. A mailbox-bearing
        // proc needs the `Proc` group to receive, and the spawner
        // already carries `Proc.Spawn`, so it may pass the group.
        let Some(group) = self.code_of(vm).bundle().group_by_name("Proc") else {
            self.machines.pop();
            self.machines[vm as usize].children -= 1;
            self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the manifest declares no Proc group",
            );
            return;
        };
        let (birth_ops, birth_groups) = self.declared_grants(child, body_func);
        let limit = self.machines[child as usize].config.mailbox_limit;
        {
            let m = &mut self.machines[child as usize];
            m.table.set_group(group, Some(Action::Pass));
            // The birth grant also passes the declared row of the proc
            // body. The spawner charges the same row, so this creates
            // no authority. A proc that drives therefore needs no
            // pause and no table edit before it runs.
            for op in &birth_ops {
                m.table.set_exact(*op, Some(Action::Pass));
            }
            for group in &birth_groups {
                m.table.set_group(*group, Some(Action::Pass));
            }
            m.vm.mailbox = Mailbox::new(limit);
            m.start_body = Some(body);
            m.owner = Ownership::Scheduler;
            m.is_proc = true;
        }
        // The spawn arguments cross a machine boundary, so they meet
        // the parameter types of the constructor before the frame
        // loads them. A refusal rolls the whole call back.
        if let Err(code) = self.check_frame_args(child, func, ctor_env, &ctor_args) {
            self.machines.pop();
            self.machines[vm as usize].children -= 1;
            self.fault_caller(
                vm,
                op,
                code,
                "a spawn argument does not carry its declared type",
            );
            return;
        }
        let code = self.code_of(child).clone();
        self.machines[child as usize].load_frame(
            code.as_ref(),
            func,
            ctor_args,
            Some(ctor),
            ctor_env,
        );
        {
            // `load_frame` recorded the constructor. The machine
            // witness states the body instead, so the record stays
            // true after the constructor frame returns.
            let m = &mut self.machines[child as usize];
            m.body_func = Some(body_func);
            m.witness = body_env;
        }
        let generation = self.machines[child as usize].generation;
        let built = self.machines[vm as usize].alloc(Object::NativeHandle {
            proc: child,
            generation,
        });
        match built {
            Ok(handle) => {
                self.activate_scheduler_proc_prepared(child);
                self.record(TraceEvent::Spawn {
                    parent: vm,
                    proc: child,
                    generation,
                });
                self.install_value_reply(vm, handle);
            }
            Err(code) => {
                self.machines.pop();
                self.machines[vm as usize].children -= 1;
                self.machines[vm as usize].set_fault(code, "", Some(op));
            }
        }
    }

    /// `sys.proc.run(vm)`: transfer one loaded machine to the
    /// scheduler. The launch carries no mailbox, so the handle takes
    /// the bottom message type.
    pub(super) fn proc_run(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some(target) = self.run_arg(vm, op, args[0]) else {
            return;
        };
        if target == vm || self.machines[target as usize].active > 0 {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
            return;
        }
        if !self.expect_holder_owned(vm, op, target) {
            return;
        }
        if self.machines[target as usize].vm.state != MachineState::Ready {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "the machine is not ready to run",
            );
            return;
        }
        let generation = self.machines[target as usize].generation;
        if let Err(code) = self.prepare_scheduler_proc(target) {
            self.fault_caller(vm, op, code, "the scheduler has no task capacity");
            return;
        }
        let built = self.machines[vm as usize].alloc(Object::NativeHandle {
            proc: target,
            generation,
        });
        match built {
            Ok(handle) => {
                self.machines[target as usize].owner = Ownership::Scheduler;
                self.activate_scheduler_proc_prepared(target);
                self.record(TraceEvent::Spawn {
                    parent: vm,
                    proc: target,
                    generation,
                });
                self.install_value_reply(vm, handle);
            }
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    /// Launch one nullary closure as a mailbox-free proc.
    pub(super) fn proc_run_closure(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let child_config = match self.reserve_child(vm) {
            Some(config) => config,
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the parent has no child budget left",
                );
                return;
            }
        };
        let child = self.machines.len() as VmId;
        if let Err(code) = self.prepare_scheduler_proc(child) {
            self.machines[vm as usize].children -= 1;
            self.fault_caller(vm, op, code, "the scheduler has no task capacity");
            return;
        }
        let mut machine = self.empty_machine(child_config, Some(vm), 0);
        machine.image = self.machines[vm as usize].image;
        self.machines.push(machine.into());
        let body = match self.transfer(vm, child, args[0]) {
            Ok(value) => value,
            Err(code) => {
                self.machines.pop();
                self.machines[vm as usize].children -= 1;
                self.fault_caller(vm, op, code, "the proc closure is not sendable");
                return;
            }
        };
        let Some(reference) = body.as_obj() else {
            self.machines.pop();
            self.machines[vm as usize].children -= 1;
            self.fault_caller(
                vm,
                op,
                FaultCode::TypeMismatch,
                "the proc body is not a closure",
            );
            return;
        };
        let (func, env) = match self.machines[child as usize].vm.heap.get(reference) {
            Object::Closure { func, env, .. } => (*func, env.env()),
            _ => {
                self.machines.pop();
                self.machines[vm as usize].children -= 1;
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the proc body is not a closure",
                );
                return;
            }
        };
        if let Err(code) = self.check_frame_args(child, func, env, &[]) {
            self.machines.pop();
            self.machines[vm as usize].children -= 1;
            self.fault_caller(vm, op, code, "the proc closure is not nullary");
            return;
        }
        let (birth_ops, birth_groups) = self.declared_grants(child, func);
        let code = self.code_of(child).clone();
        {
            let child_machine = &mut self.machines[child as usize];
            for operation in birth_ops {
                child_machine.table.set_exact(operation, Some(Action::Pass));
            }
            for group in birth_groups {
                child_machine.table.set_group(group, Some(Action::Pass));
            }
            let limit = child_machine.config.mailbox_limit;
            child_machine.vm.mailbox = Mailbox::new(limit);
            child_machine.owner = Ownership::Scheduler;
            child_machine.is_proc = true;
            child_machine.body_func = Some(func);
            child_machine.witness = env;
            child_machine.load_frame(code.as_ref(), func, Vec::new(), Some(reference), env);
        }
        let generation = self.machines[child as usize].generation;
        let built = self.machines[vm as usize].alloc(Object::NativeHandle {
            proc: child,
            generation,
        });
        match built {
            Ok(handle) => {
                self.activate_scheduler_proc_prepared(child);
                self.record(TraceEvent::Spawn {
                    parent: vm,
                    proc: child,
                    generation,
                });
                self.install_value_reply(vm, handle);
            }
            Err(code) => {
                self.machines.pop();
                self.machines[vm as usize].children -= 1;
                self.machines[vm as usize].set_fault(code, "", Some(op));
            }
        }
    }

    /// `h.send(message)`.
    ///
    /// The mailbox limit is checked before the copy, so a refused
    /// message never enters the target heap.
    pub(super) fn proc_send(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        self.metrics.sends = self.metrics.sends.saturating_add(1);
        if self
            .machines
            .get(proc as usize)
            .is_some_and(|machine| machine.active > 0)
        {
            self.metrics.destination_active_sends =
                self.metrics.destination_active_sends.saturating_add(1);
        }
        if !self.proc_running(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the target proc is dead")
                .and_then(|fault| self.make_instance(vm, self.core_of(vm).send_fault, vec![fault]));
            self.record(TraceEvent::Send {
                from: vm,
                to: proc,
                accepted: false,
            });
            self.reply_or_fault(vm, op, built);
            return;
        }
        let mailbox = &self.machines[proc as usize].vm.mailbox;
        if mailbox.closed {
            let built = self.make_instance(vm, self.core_of(vm).send_closed, vec![]);
            self.record(TraceEvent::Send {
                from: vm,
                to: proc,
                accepted: false,
            });
            self.reply_or_fault(vm, op, built);
            return;
        }
        if !mailbox.accepts() {
            // The queue is full, or a barrier froze acceptance. The
            // sender waits for one free slot; it never drops a
            // message and never copies one.
            self.block_machine(
                vm,
                Block::Send {
                    target: proc,
                    generation,
                },
            );
            return;
        }
        let moved = match self.boundary_copy(vm, proc, args[1]) {
            Ok(value) => value,
            Err(code) => {
                // A message that fails the sender-side boundary check
                // faults the sender (specification 18.4).
                let text = copy_failure(code, "message");
                self.fault_caller(vm, op, code, &text);
                return;
            }
        };
        {
            let mailbox = &mut self.machines[proc as usize].vm.mailbox;
            mailbox.push(moved);
        }
        let target = TaskKey {
            vm: proc,
            generation,
        };
        self.emit_wake(WakeKey::Receive(target));
        self.record(TraceEvent::Send {
            from: vm,
            to: proc,
            accepted: true,
        });
        let built = self.make_instance(vm, self.core_of(vm).send_sent, vec![]);
        self.reply_or_fault(vm, op, built);
    }

    /// `h.close()`. A successful close returns `Sent`; a repeat
    /// returns `Closed` (specification 18.4).
    pub(super) fn proc_close(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        if !self.proc_running(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the target proc is dead")
                .and_then(|fault| self.make_instance(vm, self.core_of(vm).send_fault, vec![fault]));
            self.reply_or_fault(vm, op, built);
            return;
        }
        let first = !self.machines[proc as usize].vm.mailbox.closed;
        self.machines[proc as usize].vm.mailbox.closed = true;
        let target = TaskKey {
            vm: proc,
            generation,
        };
        self.emit_wake(WakeKey::Receive(target));
        self.emit_wake(WakeKey::Send(target));
        self.record(TraceEvent::Close { proc, first });
        let arm = if first {
            self.core_of(vm).send_sent
        } else {
            self.core_of(vm).send_closed
        };
        let built = self.make_instance(vm, arm, vec![]);
        self.reply_or_fault(vm, op, built);
    }

    /// `self.receive()` inside a proc.
    ///
    /// The host answers only for a scheduler-owned machine, so the
    /// rule of specification 18.5 fails closed everywhere else.
    pub(super) fn proc_recv(&mut self, vm: VmId, op: u32) {
        if self.machines[vm as usize].owner != Ownership::Scheduler {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "receive is valid only on a scheduler-owned proc",
            );
            return;
        }
        let message = self.machines[vm as usize].vm.mailbox.pop();
        match message {
            Some(value) => {
                if let Some(target) = self.task_key(vm) {
                    self.emit_wake(WakeKey::Send(target));
                }
                self.record(TraceEvent::Receive {
                    proc: vm,
                    closed: false,
                });
                let built = self.make_instance(vm, self.core_of(vm).recv_msg, vec![value]);
                self.reply_or_fault(vm, op, built);
            }
            None if self.machines[vm as usize].vm.mailbox.closed => {
                self.record(TraceEvent::Receive {
                    proc: vm,
                    closed: true,
                });
                let built = self.make_instance(vm, self.core_of(vm).recv_closed, vec![]);
                self.reply_or_fault(vm, op, built);
            }
            // The mailbox is open and empty: wait for a message or a
            // close.
            None => self.block_machine(vm, Block::Receive),
        }
    }

    /// `h.done()`. The holder blocks until the proc is terminal.
    pub(super) fn proc_done(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        if !self.proc_alive(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the proc reference is stale")
                .and_then(|fault| self.make_instance(vm, self.core_of(vm).result_err, vec![fault]));
            self.reply_or_fault(vm, op, built);
            return;
        }
        match self.machines[proc as usize].vm.state {
            MachineState::Done | MachineState::Faulted => self.publish_terminal(vm, op, proc),
            _ => self.block_machine(
                vm,
                Block::Done {
                    target: proc,
                    generation,
                },
            ),
        }
    }

    /// `h.snapshot_wait(fuel)`: capture the proc at its next safe boundary.
    pub(super) fn proc_snapshot_wait(
        &mut self,
        vm: VmId,
        op: u32,
        args: Args<'_>,
        remaining: Option<u64>,
    ) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        let Value::Int(fuel) = args[1] else {
            self.fault_caller(
                vm,
                op,
                FaultCode::TypeMismatch,
                "the fuel argument is not an integer",
            );
            return;
        };
        if !self.proc_alive(proc, generation) {
            self.fault_caller(vm, op, FaultCode::DeadProc, "the proc reference is stale");
            return;
        }
        if proc == vm
            || self.machines[proc as usize].owner != Ownership::Scheduler
            || self.machines[proc as usize].paused
        {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "snapshot_wait needs a running scheduler proc",
            );
            return;
        }
        let remaining = remaining.unwrap_or(fuel.max(0) as u64);
        let barrier = self.next_gate();
        let result = self.capture_snapshot(barrier, proc, false);
        match result {
            Ok(image) => self.install_snapshot_result(vm, op, Ok(image)),
            Err(active @ crate::snapshot::SnapshotFail::ResourceActive { .. }) => {
                if remaining == 0 || !self.snapshot_target_can_progress(vm, proc) {
                    self.install_snapshot_result(vm, op, Err(active));
                    return;
                }
                self.block_machine(
                    vm,
                    Block::Snapshot {
                        target: proc,
                        generation,
                        remaining,
                        retry: false,
                    },
                );
            }
            Err(fail) => self.install_snapshot_result(vm, op, Err(fail)),
        }
    }

    pub(super) fn snapshot_target_can_progress(&mut self, caller: VmId, root: VmId) -> bool {
        let Ok(machines) = self.controlled_machines(root) else {
            return false;
        };
        let mut blocked = false;
        for vm in &machines {
            if *vm == caller {
                continue;
            }
            let Some(key) = self.task_key(*vm) else {
                continue;
            };
            match self.task_status(key) {
                TaskStatus::Ready | TaskStatus::Waiting(_) | TaskStatus::Parked(_) => return true,
                TaskStatus::Blocked(_) => blocked = true,
                TaskStatus::Terminal | TaskStatus::Dormant => {}
            }
        }
        if !blocked {
            return false;
        }
        for (key, status) in self.scheduler_seeds(true) {
            if key.vm == caller
                || machines.contains(&key.vm)
                || !matches!(
                    status,
                    TaskStatus::Ready | TaskStatus::Waiting(_) | TaskStatus::Parked(_)
                )
            {
                continue;
            }
            let reaches_target = self
                .controlled_machines(key.vm)
                .is_ok_and(|set| set.iter().any(|vm| machines.contains(vm)));
            if reaches_target {
                return true;
            }
        }
        false
    }

    /// Build and install `Result` for one terminal proc.
    pub(super) fn publish_terminal(&mut self, vm: VmId, op: u32, proc: VmId) {
        enum T {
            Done(Value),
            Fault(FaultRec),
        }
        let t = match &self.machines[proc as usize].vm.terminal {
            Some(Terminal::Done(value)) => T::Done(*value),
            Some(Terminal::Fault(rec)) => T::Fault(rec.clone()),
            None => T::Fault(FaultRec {
                code: FaultCode::MalformedState,
                message: "the terminal proc stores no result".to_string(),
                op: None,
                trace: Vec::new(),
            }),
        };
        let built = match t {
            T::Done(value) => match self.transfer(proc, vm, value) {
                Ok(value) => self.make_instance(vm, self.core_of(vm).result_ok, vec![value]),
                Err(code) => self
                    .make_fault(vm, code, "the terminal value did not cross the boundary")
                    .and_then(|fault| {
                        self.make_instance(vm, self.core_of(vm).result_err, vec![fault])
                    }),
            },
            T::Fault(rec) => self.machines[vm as usize]
                .alloc(Object::NativeFault {
                    code: rec.code,
                    message: rec.message.clone(),
                    op: rec.op,
                    trace: rec.trace.clone().into_boxed_slice(),
                })
                .and_then(|fault| self.make_instance(vm, self.core_of(vm).result_err, vec![fault])),
        };
        self.reply_or_fault(vm, op, built);
    }

    /// `h.pause()`: take execution ownership back from the scheduler.
    pub(super) fn proc_pause(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        let arm = if !self.proc_running(proc, generation) {
            self.core_of(vm).proc_error_dead
        } else if self.machines[proc as usize].paused {
            self.core_of(vm).proc_error_already_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core_of(vm).proc_error_in_use
        } else {
            let key = TaskKey {
                vm: proc,
                generation,
            };
            self.machines[proc as usize].owner = Ownership::Holder;
            self.machines[proc as usize].paused = true;
            self.deactivate_scheduler_proc(key);
            self.record(TraceEvent::Pause { proc });
            let built = self.machines[vm as usize]
                .alloc(Object::NativeRun { vm: proc })
                .and_then(|handle| {
                    self.make_instance(vm, self.core_of(vm).result_ok, vec![handle])
                });
            self.reply_or_fault(vm, op, built);
            return;
        };
        let built = self
            .make_instance(vm, arm, vec![])
            .and_then(|error| self.make_instance(vm, self.core_of(vm).result_err, vec![error]));
        self.reply_or_fault(vm, op, built);
    }

    /// `h.resume()`: give execution ownership back to the scheduler.
    pub(super) fn proc_resume(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        let arm = if !self.proc_running(proc, generation) {
            self.core_of(vm).proc_error_dead
        } else if !self.machines[proc as usize].paused {
            self.core_of(vm).proc_error_not_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core_of(vm).proc_error_in_use
        } else {
            if let Err(code) = self.prepare_scheduler_proc(proc) {
                self.fault_caller(vm, op, code, "the scheduler has no task capacity");
                return;
            }
            self.machines[proc as usize].owner = Ownership::Scheduler;
            self.machines[proc as usize].paused = false;
            self.activate_scheduler_proc_prepared(proc);
            self.record(TraceEvent::Resume { proc });
            let built = self.make_instance(vm, self.core_of(vm).result_ok, vec![Value::Unit]);
            self.reply_or_fault(vm, op, built);
            return;
        };
        let built = self
            .make_instance(vm, arm, vec![])
            .and_then(|error| self.make_instance(vm, self.core_of(vm).result_err, vec![error]));
        self.reply_or_fault(vm, op, built);
    }

    /// Apply one policy-table edit performed by `vm`.
    pub(super) fn handle_table_edit(
        &mut self,
        vm: VmId,
        table: ObjRef,
        action: u32,
        kind: u32,
        slot: u32,
        mock: Option<Value>,
    ) {
        let target = match self.machines[vm as usize].vm.heap.get(table) {
            Object::NativeTable { vm } => *vm,
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the receiver is not a policy table handle",
                    None,
                );
                return;
            }
        };
        let entry = match action {
            0 => Some(Action::Pass),
            1 => Some(Action::Block),
            2 => {
                let Some(closure) = mock else {
                    self.machines[vm as usize].set_fault(
                        FaultCode::MalformedState,
                        "the mock edit carries no handler",
                        None,
                    );
                    return;
                };
                // Installation boundary-copies the handler into
                // table-owned storage (specification 13.3). The
                // one-heap path runs the same copy, so a same-heap
                // install carries the same rule.
                //
                // No machine can reach the same-heap branch today: a
                // table handle comes from a machine handle, and no
                // operation mints a handle to the performing machine.
                // `docs/notes/week7.md` records it.
                debug_assert_ne!(target, vm, "a machine cannot hold a table handle to itself");
                match self.boundary_copy(vm, target, closure) {
                    Ok(value) => match value.as_obj() {
                        Some(r) => Some(Action::Mock(r)),
                        None => {
                            self.machines[vm as usize].set_fault(
                                FaultCode::TypeMismatch,
                                "the mock handler is not a closure",
                                None,
                            );
                            return;
                        }
                    },
                    Err(code) => {
                        self.machines[vm as usize].set_fault(
                            code,
                            "the mock handler is not sendable",
                            None,
                        );
                        return;
                    }
                }
            }
            _ => None,
        };
        let table = &mut self.machines[target as usize].table;
        // The verifier bounds every table-edit slot. A bad slot still
        // faults instead of changing the table.
        let stored = if kind == 0 {
            table.set_exact(slot, entry)
        } else {
            table.set_group(slot, entry)
        };
        if !stored {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the table edit names no policy slot",
                None,
            );
            return;
        }
        // A table edit is an ordinary instruction: push the unit
        // result directly.
        if let Err(code) = self.machines[vm as usize].push(Value::Unit) {
            self.machines[vm as usize].set_fault(code, "", None);
        }
    }

    /// `request.op_name()` executed by `vm`.
    ///
    /// A request token names a machine and an ordinal, and never the
    /// operation. The name therefore comes from the pending record of
    /// the target machine, and the request must still be live. A
    /// continuation spends the request, so a holder reads the name
    /// before it answers, rejects, or dispatches.
    pub(super) fn handle_request_op(&mut self, vm: VmId, request: ObjRef) {
        let (rv, ordinal) = match self.machines[vm as usize].vm.heap.get(request) {
            Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the receiver is not a request token",
                    None,
                );
                return;
            }
        };
        let found = self.machines.get(rv as usize).and_then(|m| {
            if m.vm.state != MachineState::Asked {
                return None;
            }
            m.vm.pending
                .as_ref()
                .filter(|pending| pending.ordinal == ordinal)
                .map(|pending| pending.op)
        });
        let Some(op) = found else {
            self.machines[vm as usize].set_fault(
                FaultCode::InvalidRequestToken,
                "the request token is consumed or stale; read `op_name` \
                 before the continuation spends the request",
                None,
            );
            return;
        };
        let name = self
            .code_of(rv)
            .bundle()
            .op_name(op)
            .unwrap_or("<invalid operation>")
            .to_string();
        let built = self.machines[vm as usize].alloc(Object::Str(name.into()));
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    /// The operation identity test of a `Call` pattern, run by `vm`.
    pub(super) fn handle_as_call(
        &mut self,
        vm: VmId,
        request: ObjRef,
        op: u32,
        ty: u32,
        env: lm_value::TypeEnvId,
    ) {
        let (rv, ordinal) = match self.machines[vm as usize].vm.heap.get(request) {
            Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the receiver is not a request token",
                    None,
                );
                return;
            }
        };
        let matches = {
            let m = &self.machines[rv as usize];
            m.vm.state == MachineState::Asked
                && m.vm
                    .pending
                    .as_ref()
                    .map(|p| p.ordinal == ordinal && p.op == op)
                    .unwrap_or(false)
        };
        let built = if matches {
            self.machines[vm as usize].alloc(Object::NativeCall {
                vm: rv,
                ordinal,
                op,
            })
        } else {
            self.native_option_none(vm, ty, env)
        };
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    /// `value.digest()` executed by `vm`.
    ///
    /// The digest mode requires a frozen graph and rejects a live
    /// holder-local value with `BoundaryViolation`. A frozen object
    /// never changes, so the heap caches the result.
    pub(super) fn handle_digest(&mut self, vm: VmId, value: ObjRef, ty: u32, env: TypeEnvId) {
        // The machine that asks for the digest pays for the walk.
        let limits = self.machines[vm as usize].config.graph;
        let code = self.code_of(vm).clone();
        let built = match code.identity() {
            Ok(identity) => {
                let expected = self
                    .envs
                    .close(code.as_ref(), ty, env)
                    .map_err(|_| FaultCode::BoundaryLimit);
                let mut codes = ModuleCodes {
                    identity,
                    bundle: code.bundle(),
                    module: code.as_ref(),
                    envs: &mut self.envs,
                    core: code.core_layout(),
                };
                let heap = &mut self.machines[vm as usize].vm.heap;
                expected.and_then(|expected| {
                    lm_graph::digest_typed_value(
                        heap,
                        Value::Obj(value),
                        expected,
                        &mut codes,
                        &limits,
                    )
                })
            }
            Err(code) => Err(code),
        };
        let pushed = built
            .and_then(|bytes| self.machines[vm as usize].alloc(Object::NativeDigest(bytes)))
            .and_then(|value| self.machines[vm as usize].push(value));
        if let Err(code) = pushed {
            self.machines[vm as usize].set_fault(code, "the value has no canonical digest", None);
        }
    }

    pub(super) fn handle_call_args(&mut self, vm: VmId, call: ObjRef) {
        let (cv, ordinal, op) = match self.machines[vm as usize].vm.heap.get(call) {
            Object::NativeCall { vm, ordinal, op } => (*vm, *ordinal, *op),
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the receiver is not a call token",
                    None,
                );
                return;
            }
        };
        let matches = {
            let m = &self.machines[cv as usize];
            m.vm.state == MachineState::Asked
                && m.vm
                    .pending
                    .as_ref()
                    .map(|p| p.ordinal == ordinal && p.op == op)
                    .unwrap_or(false)
        };
        if !matches {
            self.machines[vm as usize].set_fault(
                FaultCode::InvalidRequestToken,
                "the call token is stale or foreign",
                None,
            );
            return;
        }
        let source_args: Vec<Value> = match self.machines[cv as usize].vm.pending.as_ref() {
            Some(pending) => pending.args.clone(),
            None => Vec::new(),
        };
        let built = if source_args.is_empty() {
            Ok(Value::Unit)
        } else {
            // The tuple allocation reads its own children as roots,
            // so the values need no root after `transfer_all`.
            self.transfer_all(cv, vm, &source_args)
                .and_then(|items| self.machines[vm as usize].alloc(Object::Tuple { items }))
        };
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    // ------------------------------------------------------------
    // The scheduler interface.
    //
    // `lm-proc` drives these entry points. Every one of them names a
    // machine by identifier, so the scheduler holds no guest heap
    // reference.
    // ------------------------------------------------------------
}
