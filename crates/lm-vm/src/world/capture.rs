//! Snapshot capture, restore, and their replies.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// Capture one machine world and install the typed result.
    pub(super) fn take_snapshot(&mut self, vm: VmId, op: u32, root: VmId, self_root: bool) {
        // A barrier identifier and a world gate both need one number
        // this world never repeats, and one monotone counter serves
        // both. The two live in different machine fields, so a shared
        // counter never confuses a barrier with a gate.
        let barrier = self.next_gate();
        let result = self.capture_snapshot(barrier, root, self_root);
        self.install_snapshot_result(vm, op, result);
    }

    pub(super) fn install_snapshot_result(
        &mut self,
        vm: VmId,
        op: u32,
        result: Result<crate::snapshot::SnapshotImage, crate::snapshot::SnapshotFail>,
    ) {
        let built = match result {
            Ok(image) => {
                // The guest value names the admitted world of this
                // process. The capture therefore writes no container
                // and hashes nothing, and a restore reads the world
                // back with no decode and no lookup.
                self.last_image = Some(image.clone());
                let slot = self.intern_image(image);
                self.machines[vm as usize]
                    .alloc(Object::NativeSnapshotRef { image: slot })
                    .and_then(|value| self.make_instance(vm, self.core.result_ok, vec![value]))
            }
            Err(crate::snapshot::SnapshotFail::Fault(code, message)) => {
                self.fault_caller(vm, op, code, &message);
                return;
            }
            Err(fail) => self
                .build_snapshot_error(vm, &fail)
                .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error])),
        };
        self.reply_or_fault(vm, op, built);
    }

    /// Build one `SnapshotError` value of specification 17.4.
    pub(super) fn build_snapshot_error(
        &mut self,
        vm: VmId,
        fail: &crate::snapshot::SnapshotFail,
    ) -> Result<Value, FaultCode> {
        match fail {
            crate::snapshot::SnapshotFail::LimitExceeded => {
                self.make_instance(vm, self.core.snapshot_limit_exceeded, vec![])
            }
            crate::snapshot::SnapshotFail::ResourceActive { path, kind } => {
                let items: Vec<Value> = path.iter().map(|p| Value::Int(*p as i64)).collect();
                let list = self.machines[vm as usize].alloc(Object::List {
                    items,
                    epoch: StructuralEpoch::default(),
                })?;
                // The list holds no root yet, so it stays host-rooted
                // while the kind string allocates.
                let list_ref = list.as_obj().ok_or(FaultCode::MalformedState)?;
                self.machines[vm as usize].vm.heap.push_host_root(list_ref);
                let text = self.machines[vm as usize].alloc(Object::Str(kind.clone().into()));
                self.machines[vm as usize].vm.heap.pop_host_root(list_ref);
                let text = text?;
                self.make_instance(vm, self.core.snapshot_resource_active, vec![list, text])
            }
            crate::snapshot::SnapshotFail::Fault(_, message) => {
                let text = self.machines[vm as usize].alloc(Object::Str(message.clone().into()))?;
                self.make_instance(vm, self.core.snapshot_bad_image, vec![text])
            }
        }
    }

    /// `sys.vm.Vm().restore(snap)`.
    ///
    /// A guest holds a snapshot as container bytes. Bytes this world
    /// already wrote or already checked restore through the trusted
    /// path; any other bytes run the external loader once first, so no
    /// unchecked image ever builds a world.
    pub(super) fn restore_snapshot(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some(image_vm) = self.image_arg(vm, op, args[0]) else {
            return;
        };
        // A snapshot value takes one of two shapes. A capture of this
        // process names an admitted image of this world, so the
        // restore reads the world back with no decode, no hash, and
        // no lookup. A restored world states an opaque container
        // instead, because a nested image stays opaque until its own
        // restore admits it (specification 17.8).
        enum Held {
            Admitted(u32),
            Container(std::sync::Arc<Vec<u8>>),
        }
        let found =
            args[1]
                .as_obj()
                .and_then(|r| match self.machines[vm as usize].vm.heap.get(r) {
                    Object::NativeSnapshotRef { image } => Some(Held::Admitted(*image)),
                    Object::NativeSnapshot(bytes) => Some(Held::Container(bytes.clone())),
                    _ => None,
                });
        let Some(held) = found else {
            self.fault_caller(
                vm,
                op,
                FaultCode::TypeMismatch,
                "the argument is not a snapshot value",
            );
            return;
        };
        let image = match held {
            Held::Admitted(slot) => match self.image_at(slot) {
                Some(image) => image,
                None => {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::MalformedState,
                        "the snapshot value names no admitted image",
                    );
                    return;
                }
            },
            Held::Container(bytes) => {
                if bytes.len() < 32 {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::BoundaryViolation,
                        "the snapshot container is shorter than its frame",
                    );
                    return;
                }
                let hash = crate::snapshot::codec::container_hash(&bytes[..bytes.len() - 32]);
                match self.trusted_image(&hash) {
                    Some(image) => image,
                    None => match self.load_snapshot_bytes(&bytes) {
                        Ok(image) => image,
                        Err(error) => {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::BoundaryViolation,
                                &format!("the snapshot image did not load: {error}"),
                            );
                            return;
                        }
                    },
                }
            }
        };
        // The admitted state names the program it passed against. This
        // world runs one program, so a mismatch is a local fault, not
        // a restore this call trusts.
        let semantic = self.identity().map(|id| id.semantic_hash);
        if semantic != Ok(image.identity().module_semantic) {
            self.fault_caller(
                vm,
                op,
                FaultCode::BoundaryViolation,
                "the snapshot image was admitted against another program",
            );
            return;
        }
        let target = match self.prepare_run_target(vm, image_vm) {
            Some(target) => target,
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "the VM image has no run budget left",
                );
                return;
            }
        };
        let reply = match self.prepare_restore_reply(vm, target) {
            Ok(reply) => reply,
            Err(code) => {
                self.rollback_run_target(vm, target);
                self.machines[vm as usize].set_fault(code, "", Some(op));
                return;
            }
        };
        if let Err(code) = self.check_reply(vm, reply.value) {
            self.discard_restore_reply(vm, reply);
            self.rollback_run_target(vm, target);
            self.machines[vm as usize].set_fault(
                code,
                "the reply does not carry the type of its perform",
                Some(op),
            );
            return;
        }
        if let Err(code) = self.reserve_restore_reply_slot(vm) {
            self.discard_restore_reply(vm, reply);
            self.rollback_run_target(vm, target);
            self.machines[vm as usize].set_fault(code, "", Some(op));
            return;
        }
        let built = match self.prepare_restore(vm, target, &image) {
            Ok(plan) => {
                self.commit_restore(plan);
                self.install_prepared_restore_reply(vm, reply);
                return;
            }
            Err(crate::snapshot::RestoreFail::LimitExceeded) => {
                self.discard_restore_reply(vm, reply);
                self.rollback_run_target(vm, target);
                self.make_instance(vm, self.core.restore_limit_exceeded, vec![])
                    .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error]))
            }
            // The check above already answered this case, so the guest
            // never reaches it. Restore states the rule again for
            // every caller, and a mismatch here is a boundary fault.
            Err(crate::snapshot::RestoreFail::OtherProgram) => {
                self.discard_restore_reply(vm, reply);
                self.rollback_run_target(vm, target);
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::BoundaryViolation,
                    "the snapshot image was admitted against another program",
                );
                return;
            }
        };
        self.reply_or_fault(vm, op, built);
    }

    /// Build the successful restore reply without partial allocation.
    pub(super) fn prepare_restore_reply(
        &mut self,
        vm: VmId,
        target: VmId,
    ) -> Result<PreparedRestoreReply, FaultCode> {
        let class = self.core.result_ok.ok_or(FaultCode::MalformedState)?;
        let handle = Object::NativeRun { vm: target };
        let mut fields = Vec::new();
        fields
            .try_reserve_exact(1)
            .map_err(|_| FaultCode::HeapLimit)?;
        fields.push(Value::Unit);
        let mut reply = Object::Instance {
            class,
            fields,
            env: lm_value::Witness::EMPTY,
        };
        let bytes = handle
            .cost()
            .checked_add(reply.cost())
            .ok_or(FaultCode::HeapLimit)?;
        if self.machines[vm as usize]
            .vm
            .heap
            .would_exceed_batch(bytes, 2)
        {
            self.machines[vm as usize].collect_garbage(&[]);
            if self.machines[vm as usize]
                .vm
                .heap
                .would_exceed_batch(bytes, 2)
            {
                return Err(FaultCode::HeapLimit);
            }
        }
        let handle = self.machines[vm as usize]
            .vm
            .heap
            .try_alloc(handle)
            .map_err(|_| FaultCode::HeapLimit)?;
        let Object::Instance { fields, .. } = &mut reply else {
            return Err(FaultCode::MalformedState);
        };
        fields[0] = Value::Obj(handle);
        match self.machines[vm as usize].vm.heap.try_alloc(reply) {
            Ok(reply) => Ok(PreparedRestoreReply {
                value: Value::Obj(reply),
                handle,
                reply,
            }),
            Err(_) => {
                self.machines[vm as usize].vm.heap.free(handle);
                Err(FaultCode::HeapLimit)
            }
        }
    }

    /// Remove one prepared reply after restore preparation fails.
    pub(super) fn discard_restore_reply(&mut self, vm: VmId, reply: PreparedRestoreReply) {
        let heap = &mut self.machines[vm as usize].vm.heap;
        heap.free(reply.reply);
        heap.free(reply.handle);
    }

    /// Reserve the operand slot for one prepared restore reply.
    pub(super) fn reserve_restore_reply_slot(&mut self, vm: VmId) -> Result<(), FaultCode> {
        let machine = &mut self.machines[vm as usize];
        let stack = machine
            .vm
            .locals
            .len()
            .checked_add(machine.vm.operands.len())
            .and_then(|used| used.checked_add(1))
            .ok_or(FaultCode::StackLimit)?;
        if stack > machine.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        machine
            .vm
            .operands
            .try_reserve(1)
            .map_err(|_| FaultCode::StackLimit)
    }

    /// Install a checked reply after restore commit.
    pub(super) fn install_prepared_restore_reply(&mut self, vm: VmId, reply: PreparedRestoreReply) {
        let machine = &mut self.machines[vm as usize];
        if let Some(pending) = &machine.vm.pending {
            machine.resources.close_by_ordinal(pending.ordinal);
        }
        machine.vm.pending = None;
        machine.vm.operands.push(reply.value);
        if machine.vm.state != MachineState::Running {
            machine.vm.state = MachineState::Ready;
        }
    }

    /// Check that the holder still owns the execution of a machine.
    ///
    /// `Proc.Run` transfers ownership to the scheduler and leaves the
    /// original `Vm` handle dormant. Execution and inspection through
    /// it fault until `pause()` returns ownership (specification
    /// 18.2). Table edits stay legal, so revocation still works.
    pub(super) fn expect_holder_owned(&mut self, vm: VmId, op: u32, target: VmId) -> bool {
        if self.machines[target as usize].owner == Ownership::Scheduler {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "the machine belongs to the scheduler; pause the proc first",
            );
            return false;
        }
        true
    }

    /// Validate one direct or routed continuation token once.
    pub(super) fn reply_sink(
        &mut self,
        vm: VmId,
        control_op: u32,
        surface: VmId,
        target: VmId,
        ordinal: u64,
        expected_op: Option<u32>,
    ) -> Option<ReplySink> {
        if surface == vm || self.machines[surface as usize].active > 0 {
            self.fault_caller(
                vm,
                control_op,
                FaultCode::InvalidVmState,
                "the machine is in use",
            );
            return None;
        }
        if !self.expect_holder_owned(vm, control_op, surface) {
            return None;
        }
        if self.machines.get(target as usize).is_none() || self.machines[target as usize].active > 0
        {
            self.fault_caller(
                vm,
                control_op,
                FaultCode::InvalidRequestToken,
                "the request token names no parked machine",
            );
            return None;
        }
        let cursor = if surface == target {
            if self.machines[surface as usize].vm.state != MachineState::Asked {
                self.fault_caller(
                    vm,
                    control_op,
                    FaultCode::InvalidRequestToken,
                    "the request token is consumed or stale; `answer`, `reject`, \
                     `dispatch`, and `serve_file` each spend it once",
                );
                return None;
            }
            PolicyCursor::Table(surface)
        } else {
            match self.machines[surface as usize].vm.routed {
                Some(route) if route.target == target => route.cursor,
                _ => {
                    self.fault_caller(
                        vm,
                        control_op,
                        FaultCode::InvalidRequestToken,
                        "the request did not come through this machine",
                    );
                    return None;
                }
            }
        };
        let Some(pending) = self.machines[target as usize].vm.pending.as_ref() else {
            self.fault_caller(
                vm,
                control_op,
                FaultCode::InvalidRequestToken,
                "the request token is consumed or stale; `answer`, `reject`, \
                     `dispatch`, and `serve_file` each spend it once",
            );
            return None;
        };
        if self.machines[target as usize].vm.state != MachineState::Asked
            || pending.ordinal != ordinal
            || expected_op.is_some_and(|op| op != pending.op)
        {
            self.fault_caller(
                vm,
                control_op,
                FaultCode::InvalidRequestToken,
                "the request token is stale or foreign",
            );
            return None;
        }
        Some(ReplySink {
            surface,
            target,
            ordinal,
            op: pending.op,
            cursor,
        })
    }

    /// Clear the route after one validated reply consumes its token.
    pub(super) fn consume_reply_sink(&mut self, sink: ReplySink) {
        debug_assert!(sink.ordinal > 0);
        debug_assert!((sink.op as usize) < lm_abi::OP_COUNT as usize);
        if sink.surface != sink.target {
            debug_assert!(self.machines[sink.surface as usize]
                .vm
                .routed
                .is_some_and(|route| route.target == sink.target));
            // The slot is free now, so the next waiting request of this
            // surface takes it.
            self.machines[sink.surface as usize].vm.routed = None;
            // The reply of `sink.target` installs after this call, so
            // that machine still holds its request. Skip it.
            self.promote_next_route(sink.surface, sink.target);
        }
    }

    /// Fault the calling machine without mutating the controlled one.
    pub(super) fn fault_caller(&mut self, vm: VmId, op: u32, code: FaultCode, message: &str) {
        self.machines[vm as usize].set_fault(code, message, Some(op));
    }

    // ------------------------------------------------------------
    // Procs, mailboxes, and the scheduler interface.
    // ------------------------------------------------------------
}
