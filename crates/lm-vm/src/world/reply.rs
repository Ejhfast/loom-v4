//! Host completions, terminal events, and reply installation.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

fn extension_reply_matches(value: &HostValue, ty: lm_abi::AbiType) -> bool {
    use lm_abi::{AbiConstructor, AbiPrimitive, AbiType};
    match (value, ty) {
        (HostValue::Unit, AbiType::Primitive(AbiPrimitive::Unit))
        | (HostValue::Bool(_), AbiType::Primitive(AbiPrimitive::Bool))
        | (HostValue::Int(_), AbiType::Primitive(AbiPrimitive::Int))
        | (HostValue::Float(_), AbiType::Primitive(AbiPrimitive::Float))
        | (HostValue::Str(_), AbiType::Primitive(AbiPrimitive::String))
        | (HostValue::Bytes(_), AbiType::Primitive(AbiPrimitive::Bytes)) => true,
        (HostValue::Resource(resource), AbiType::Resource(identity)) => resource.kind == identity,
        (HostValue::List(values), AbiType::List(element)) => values
            .iter()
            .all(|value| extension_reply_matches(value, *element)),
        (HostValue::Tuple(values), AbiType::Tuple(elements)) => {
            values.len() == elements.len()
                && values
                    .iter()
                    .zip(elements)
                    .all(|(value, ty)| extension_reply_matches(value, *ty))
        }
        (
            HostValue::Ctor(CoreCtor::Some, values),
            AbiType::Apply(AbiConstructor::Option, arguments),
        ) => {
            values.len() == 1
                && arguments.len() == 1
                && extension_reply_matches(&values[0], arguments[0])
        }
        (
            HostValue::Ctor(CoreCtor::None, values),
            AbiType::Apply(AbiConstructor::Option, arguments),
        ) => values.is_empty() && arguments.len() == 1,
        (
            HostValue::Ctor(CoreCtor::Ok, values),
            AbiType::Apply(AbiConstructor::Result, arguments),
        ) => {
            values.len() == 1
                && arguments.len() == 2
                && extension_reply_matches(&values[0], arguments[0])
        }
        (
            HostValue::Ctor(CoreCtor::Err, values),
            AbiType::Apply(AbiConstructor::Result, arguments),
        ) => {
            values.len() == 1
                && arguments.len() == 2
                && extension_reply_matches(&values[0], arguments[1])
        }
        _ => false,
    }
}

impl World {
    /// The completion key of one waiting machine.
    pub(super) fn completion_key(&self, vm: VmId) -> Option<CompletionKey> {
        let machine = self.machines.get(vm as usize)?;
        let pending = machine.vm.pending.as_ref()?;
        Some(CompletionKey {
            machine: TaskKey {
                vm,
                generation: machine.generation,
            },
            ordinal: pending.ordinal,
        })
    }

    /// Poll and install one accepted host completion.
    pub fn poll_host_completion(
        &mut self,
        mut accepts: impl FnMut(CompletionKey) -> bool,
    ) -> Option<CompletionKey> {
        self.prune_host_completions();
        loop {
            if let Some(completion) = self.take_host_completion(&mut accepts) {
                if let Some(key) = self.install_host_completion(completion) {
                    return Some(key);
                }
                continue;
            }
            let completion = self.host.poll()?;
            if !self.completion_is_current(completion.key) {
                continue;
            }
            if accepts(completion.key) && self.completion_target_is_resident(completion.key) {
                if let Some(key) = self.install_host_completion(completion) {
                    return Some(key);
                }
            } else {
                self.host_completions
                    .entry(completion.key)
                    .or_insert(completion);
            }
        }
    }

    /// Wait for and install one accepted host completion.
    pub fn wait_host_completion(
        &mut self,
        mut accepts: impl FnMut(CompletionKey) -> bool,
    ) -> Option<CompletionKey> {
        self.prune_host_completions();
        loop {
            if let Some(completion) = self.take_host_completion(&mut accepts) {
                if let Some(key) = self.install_host_completion(completion) {
                    return Some(key);
                }
                continue;
            }
            let completion = self.host.wait()?;
            if !self.completion_is_current(completion.key) {
                continue;
            }
            if accepts(completion.key) && self.completion_target_is_resident(completion.key) {
                if let Some(key) = self.install_host_completion(completion) {
                    return Some(key);
                }
            } else {
                self.host_completions
                    .entry(completion.key)
                    .or_insert(completion);
            }
        }
    }

    /// Take one buffered completion accepted by the caller.
    pub(super) fn take_host_completion(
        &mut self,
        accepts: &mut impl FnMut(CompletionKey) -> bool,
    ) -> Option<HostCompletion> {
        let key = self.host_completions.keys().copied().find(|key| {
            accepts(*key)
                && self.completion_target_is_resident(*key)
                && !self.prepared_wait_exists(*key)
        })?;
        self.host_completions.remove(&key)
    }

    /// Remove replies for requests that no longer wait.
    pub(super) fn prune_host_completions(&mut self) {
        let machines = &self.machines;
        self.host_completions.retain(|key, _| {
            let Some(machine) = machines.get(key.machine.vm as usize) else {
                return false;
            };
            if machine.generation() != key.machine.generation {
                return false;
            }
            if !machine.is_resident() {
                return true;
            }
            let direct = machines
                .get(key.machine.vm as usize)
                .is_some_and(|machine| {
                    machine.generation == key.machine.generation
                        && machine.vm.state == MachineState::Waiting
                        && machine
                            .vm
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.ordinal == key.ordinal)
                });
            let prepared = machines
                .get(key.machine.vm as usize)
                .is_some_and(|machine| {
                    machine.generation == key.machine.generation
                        && !matches!(machine.vm.state, MachineState::Done | MachineState::Faulted)
                        && machine.vm.waits.values().any(|entry| {
                            matches!(
                                entry.source,
                                WaitSource::Operation { ordinal, .. } if ordinal == key.ordinal
                            )
                        })
                });
            direct || prepared
        });
    }

    /// True when one completion still names its waiting request.
    pub(super) fn completion_is_current(&self, key: CompletionKey) -> bool {
        let Some(machine) = self.machines.get(key.machine.vm as usize) else {
            return false;
        };
        if machine.generation() != key.machine.generation {
            return false;
        }
        if !machine.is_resident() {
            return true;
        }
        let direct = self
            .machines
            .get(key.machine.vm as usize)
            .is_some_and(|machine| {
                machine.generation == key.machine.generation
                    && machine.vm.state == MachineState::Waiting
                    && machine
                        .vm
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.ordinal == key.ordinal)
            });
        direct || self.prepared_wait_exists(key)
    }

    fn completion_target_is_resident(&self, key: CompletionKey) -> bool {
        self.machines
            .get(key.machine.vm as usize)
            .is_some_and(|machine| {
                machine.generation() == key.machine.generation && machine.is_resident()
            })
    }

    /// Install one host completion when its machine still waits.
    pub(super) fn install_host_completion(
        &mut self,
        completion: HostCompletion,
    ) -> Option<CompletionKey> {
        let key = completion.key;
        if !self.completion_target_is_resident(key) {
            if self.completion_is_current(key) {
                self.host_completions.entry(key).or_insert(completion);
            }
            return None;
        }
        if !self.completion_is_current(key) {
            return None;
        }
        self.metrics.host_completions = self.metrics.host_completions.saturating_add(1);
        if self.prepared_wait_exists(key) {
            let machine = &self.machines[key.machine.vm as usize];
            let scope_matches = machine
                .resources
                .pending(key.ordinal)
                .is_some_and(|record| record.scope == completion.token);
            if !scope_matches {
                self.machines[key.machine.vm as usize].set_fault(
                    FaultCode::HostFault,
                    "the host completion has another wait scope",
                    None,
                );
                return Some(key);
            }
            self.host_completions.entry(key).or_insert(completion);
            return Some(key);
        }
        let machine = &self.machines[key.machine.vm as usize];
        let scope_matches = machine
            .resources
            .pending(key.ordinal)
            .is_some_and(|record| record.scope == completion.token);
        if !scope_matches {
            self.machines[key.machine.vm as usize].set_fault(
                FaultCode::HostFault,
                "the host completion has another scope",
                None,
            );
            return Some(key);
        }
        match completion.result {
            Ok(value) => self.install_host_reply(key.machine.vm, value),
            Err(message) => {
                let op = self.pending_op(key.machine.vm);
                self.machines[key.machine.vm as usize].set_fault(FaultCode::HostFault, message, op);
                self.close_resources_for_machine(key.machine.vm);
            }
        }
        Some(key)
    }

    /// Pop the top activation and deliver its exit event. Return the
    /// event when the consumer is the world caller.
    pub(super) fn finish(
        &mut self,
        stack: &mut Vec<Activation>,
        kind: ExitKind,
    ) -> Option<RootEvent> {
        let Some(act) = stack.pop() else {
            return Some(RootEvent::Ran);
        };
        self.release_activation(act);
        if kind == ExitKind::Terminal {
            self.close_resources_for_machine(act.vm);
        }
        match act.reply_to {
            None => Some(match kind {
                ExitKind::Terminal => self.terminal_root_event(act.vm),
                ExitKind::Ran | ExitKind::Bounded => RootEvent::Ran,
                ExitKind::Waiting => RootEvent::Waiting,
            }),
            Some(parent) => {
                self.deliver_event(act, parent, kind);
                None
            }
        }
    }

    /// Deliver one exit event of `act.vm` into `parent`.
    pub(super) fn deliver_event(&mut self, act: Activation, parent: VmId, kind: ExitKind) {
        if act.family == Family::Mock {
            self.deliver_mock(act.vm, parent);
            return;
        }
        if self.machines[parent as usize].vm.nested != Some(act.vm) {
            self.machines[parent as usize].set_fault(
                FaultCode::MalformedState,
                "the nested result has no matching control edge",
                None,
            );
            return;
        }
        self.machines[parent as usize].vm.nested = None;
        let value = match kind {
            ExitKind::Terminal => self.build_terminal_event(act.vm, parent, act.family),
            ExitKind::Ran => self.make_instance(parent, self.core_of(parent).step_ran, vec![]),
            ExitKind::Waiting => {
                self.make_instance(parent, self.core_of(parent).step_waiting, vec![])
            }
            ExitKind::Bounded => self.pending_option_none(parent),
        };
        match value {
            Ok(value) => self.install_value_reply(parent, value),
            Err(code) => self.machines[parent as usize].set_fault(code, "", None),
        }
    }

    /// Deliver a finished mock run as the raw perform reply.
    pub(super) fn deliver_mock(&mut self, mock: VmId, target: VmId) {
        enum MockExit {
            Done(Value),
            Fault,
        }
        let exit = match &self.machines[mock as usize].vm.terminal {
            Some(Terminal::Done(value)) => MockExit::Done(*value),
            _ => MockExit::Fault,
        };
        match exit {
            MockExit::Done(value) => match self.transfer(mock, target, value) {
                Ok(value) => self.install_value_reply(target, value),
                Err(code) => {
                    let op = self.pending_op(target);
                    self.machines[target as usize].set_fault(
                        code,
                        "the mock result did not cross the boundary",
                        op,
                    );
                }
            },
            MockExit::Fault => {
                let op = self.pending_op(target);
                self.machines[target as usize].set_fault(
                    FaultCode::HostFault,
                    "the mock handler faulted",
                    op,
                );
            }
        }
        self.retire_mock(mock);
    }

    /// Retire one finished mock machine.
    ///
    /// The result already crossed into the target, and no guest value
    /// names a mock machine, so the record drops its heap now and the
    /// slot joins the free list.
    pub(super) fn retire_mock(&mut self, mock: VmId) {
        debug_assert!(self.machines[mock as usize].active == 0);
        // The slot takes a new generation, so a reference minted for
        // the retired record names a dead machine, never the next one.
        let generation = self.machines[mock as usize].generation.wrapping_add(1);
        self.machines[mock as usize] = self.empty_machine(self.config, None, generation).into();
        self.mock_free.push(mock);
    }

    pub(super) fn pending_op(&self, vm: VmId) -> Option<u32> {
        self.machines[vm as usize].vm.pending.as_ref().map(|p| p.op)
    }

    /// Build the terminal event value of `child` in `parent`.
    pub(super) fn build_terminal_event(
        &mut self,
        child: VmId,
        parent: VmId,
        family: Family,
    ) -> Result<Value, FaultCode> {
        enum T {
            Done(Value),
            Fault(FaultRec),
        }
        let t = match &self.machines[child as usize].vm.terminal {
            Some(Terminal::Done(value)) => T::Done(*value),
            Some(Terminal::Fault(rec)) => T::Fault(rec.clone()),
            None => T::Fault(FaultRec {
                code: FaultCode::MalformedState,
                message: "the terminal machine stores no result".to_string(),
                op: None,
                trace: Vec::new(),
            }),
        };
        let built = match t {
            T::Done(value) => match self.cross_terminal_value(child, parent, value) {
                Ok(value) => {
                    let class = self.done_arm(parent, family);
                    self.make_instance(parent, class, vec![value])
                }
                Err(code) => {
                    // The terminal value cannot cross the boundary:
                    // the controlled machine converts to a fault.
                    self.machines[child as usize].set_fault(
                        code,
                        "the terminal value did not cross the boundary",
                        None,
                    );
                    let rec = FaultRec {
                        code,
                        message: "the terminal value did not cross the boundary".to_string(),
                        op: None,
                        trace: Vec::new(),
                    };
                    self.build_fault_event(parent, family, &rec)
                }
            },
            T::Fault(rec) => self.build_fault_event(parent, family, &rec),
        };
        self.wrap_turn(parent, family, built)
    }

    /// Move one terminal value from `child` into `parent`.
    ///
    /// A dynamic result stays in the machine that produced it. The
    /// holder receives a reference to that machine, so the value keeps
    /// its own code view, and a snapshot of the holder closes over the
    /// machine like a run handle.
    fn cross_terminal_value(
        &mut self,
        child: VmId,
        parent: VmId,
        value: Value,
    ) -> Result<Value, FaultCode> {
        let machine = &self.machines[child as usize];
        let packed = value.as_obj().is_some_and(|reference| {
            matches!(machine.vm.heap.get(reference), Object::DynValue { .. })
        });
        if machine.dynamic_result || packed {
            let generation = machine.generation;
            return self.machines[parent as usize].alloc(Object::NativeDynRef {
                vm: child,
                generation,
            });
        }
        self.transfer(child, parent, value)
    }

    /// Wrap one drive event for a bounded turn.
    ///
    /// `Vm.DriveFor` answers `Option[DriveEvent[T]]`, so an event of a
    /// bounded turn arrives as `Some`.
    pub(super) fn wrap_turn(
        &mut self,
        parent: VmId,
        family: Family,
        built: Result<Value, FaultCode>,
    ) -> Result<Value, FaultCode> {
        if family != Family::DriveFor {
            return built;
        }
        let _ = parent;
        built
    }

    /// The `Done` arm of one event family.
    ///
    /// `deliver_event` answers a mock exit before it reads an arm, so
    /// the mock family reaches neither call. `None` here becomes a
    /// machine fault at `make_instance`.
    pub(super) fn done_arm(&self, vm: VmId, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core_of(vm).result_ok,
            Family::Step => self.core_of(vm).step_done,
            Family::Drive | Family::DriveFor => self.core_of(vm).drive_done,
            Family::Mock => None,
        }
    }

    pub(super) fn fault_arm(&self, vm: VmId, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core_of(vm).result_err,
            Family::Step => self.core_of(vm).step_fault,
            Family::Drive | Family::DriveFor => self.core_of(vm).drive_fault,
            Family::Mock => None,
        }
    }

    pub(super) fn build_fault_event(
        &mut self,
        parent: VmId,
        family: Family,
        rec: &FaultRec,
    ) -> Result<Value, FaultCode> {
        let fault = self.machines[parent as usize].alloc(Object::NativeFault {
            code: rec.code,
            message: rec.message.clone(),
            op: rec.op,
            trace: rec.trace.clone().into_boxed_slice(),
        })?;
        let class = self.fault_arm(parent, family);
        self.make_instance(parent, class, vec![fault])
    }

    /// Allocate one core enum case instance.
    ///
    /// The verifier proves the parent slot wherever an instruction
    /// needs the family, and it rejects a family that resolves
    /// without every arm. The arm slot is therefore present. A module
    /// that reaches this call without one faults the machine.
    pub(super) fn make_instance(
        &mut self,
        vm: VmId,
        class: Option<u32>,
        fields: Vec<Value>,
    ) -> Result<Value, FaultCode> {
        let class = class.ok_or(FaultCode::MalformedState)?;
        // The kernel builds a core enum instance outside `New` and
        // `NewG`, and it holds no closed form of the class arguments:
        // those follow from the operation manifest, which `lm-vm` does
        // not type. The instance therefore records the empty witness,
        // which states nothing. Admission derives the arguments from
        // the edge that reaches the value, so no rule depends on it.
        self.machines[vm as usize].alloc(Object::Instance {
            class,
            fields: fields.into(),
            env: lm_value::Witness::EMPTY,
        })
    }

    pub(super) fn close_option_family(
        &mut self,
        vm: VmId,
        ty: u32,
        env: TypeEnvId,
    ) -> Result<ClosedTypeId, FaultCode> {
        let code = self.code_of(vm).clone();
        let closed = self
            .envs
            .close(code.as_ref(), ty, env)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let (class, argument) = match self.envs.ty(closed) {
            Some(ClosedType::Inst(class, args)) if args.len() == 1 => (*class, args[0]),
            _ => return Err(FaultCode::MalformedState),
        };
        let option = self.core_of(vm).option.ok_or(FaultCode::MalformedState)?;
        let some = self
            .core_of(vm)
            .option_some
            .ok_or(FaultCode::MalformedState)?;
        let none = self
            .core_of(vm)
            .option_none
            .ok_or(FaultCode::MalformedState)?;
        if class != option && class != some && class != none {
            return Err(FaultCode::MalformedState);
        }
        if class == option {
            return Ok(closed);
        }
        self.envs
            .intern(ClosedType::Inst(option, vec![argument]))
            .map_err(|_| FaultCode::BoundaryLimit)
    }

    pub(super) fn native_option_none(
        &mut self,
        vm: VmId,
        ty: u32,
        env: TypeEnvId,
    ) -> Result<Value, FaultCode> {
        let ty = self.close_option_family(vm, ty, env)?;
        Ok(Value::EmptyCase { ty, arm: 1 })
    }

    pub(super) fn pending_option_none(&mut self, vm: VmId) -> Result<Value, FaultCode> {
        let (ty, env) = self.reply_type(vm)?;
        self.native_option_none(vm, ty, env)
    }

    /// The reply of one perform carries the type the instruction
    /// states.
    ///
    /// The frame of the performing machine names the instruction, and
    /// the instruction carries the reply type. The frame also names
    /// the environment that closes it. Both inputs come from verified
    /// code or from live execution, never from a snapshot container.
    ///
    /// This is the one funnel of every perform reply, so it covers the
    /// terminal result of `run`, `step`, `drive`, and `done`, the
    /// mailbox receive, the pending call reply, the spawn result, the
    /// mock reply, and the restore result together.
    pub(super) fn reply_type(&self, vm: VmId) -> Result<(u32, TypeEnvId), FaultCode> {
        if let Some(found) = self.machines[vm as usize].native_effect_reply_type() {
            return Ok(found);
        }
        let frame = self.machines[vm as usize]
            .vm
            .frames
            .last()
            .ok_or(FaultCode::MalformedState)?;
        let at = frame.ip.checked_sub(1).ok_or(FaultCode::MalformedState)?;
        let instr = self
            .code_of(vm)
            .funcs
            .get(frame.func as usize)
            .and_then(|code| code.blocks.get(frame.block as usize))
            .and_then(|block| block.get(at as usize))
            .ok_or(FaultCode::MalformedState)?;
        let ty = match instr {
            lm_bytecode::Instr::Perform { reply_ty, .. }
            | lm_bytecode::Instr::PerformValue { reply_ty, .. } => *reply_ty,
            _ => return Err(FaultCode::MalformedState),
        };
        Ok((ty, frame.env))
    }

    pub(super) fn check_reply(&mut self, vm: VmId, value: Value) -> Result<(), FaultCode> {
        // Every value of a world that restored nothing came out of
        // verified code, so the check states a rule the verifier
        // already proved. The field doc of `restored_any` carries the
        // argument.
        if !self.restored_any {
            return Ok(());
        }
        let (reply_ty, env) = self.reply_type(vm)?;
        let module = self.code_of(vm).clone();
        let machine = &self.machines[vm as usize];
        crate::typecheck::check_boundary_value(
            crate::typecheck::BoundaryContext::new(module.as_ref(), &machine.vm.heap),
            &mut self.envs,
            &mut self.check,
            value,
            reply_ty,
            env,
        )
    }

    /// Every argument of one entry frame carries the parameter type
    /// its function declares.
    ///
    /// A spawn and a `Vm.Activate` both copy values into another
    /// machine and load them as the first local slots of a frame. The
    /// declared parameter types come from verified code, and the
    /// closure states the environment its creator frame held.
    pub(super) fn check_frame_args(
        &mut self,
        vm: VmId,
        func: u32,
        env: lm_value::TypeEnvId,
        args: &[Value],
    ) -> Result<(), FaultCode> {
        // The same rule as `check_reply`: a world that restored
        // nothing holds no value the verifier failed to prove.
        if !self.restored_any {
            return Ok(());
        }
        let module = self.code_of(vm).clone();
        let code = module
            .funcs
            .get(func as usize)
            .ok_or(FaultCode::MalformedState)?;
        if code.params.len() != args.len() {
            return Err(FaultCode::MalformedState);
        }
        let machine = &self.machines[vm as usize];
        for (value, ty) in args.iter().zip(code.params.iter()) {
            crate::typecheck::check_boundary_value(
                crate::typecheck::BoundaryContext::new(module.as_ref(), &machine.vm.heap),
                &mut self.envs,
                &mut self.check,
                *value,
                *ty,
                env,
            )?;
        }
        Ok(())
    }

    /// Install one reply value into a machine whose pending perform
    /// completes now.
    pub(super) fn install_value_reply(&mut self, vm: VmId, value: Value) {
        self.install_value_reply_with_file_close(vm, value, true);
    }

    /// Install one reply and apply a successful file close.
    pub(super) fn install_value_reply_with_file_close(
        &mut self,
        vm: VmId,
        value: Value,
        close_host: bool,
    ) {
        if self.machines[vm as usize].preparing_wait.is_some() {
            self.finish_prepared_guest_wait(vm, value);
            return;
        }
        let closing = match self.pending_op(vm) {
            Some(lm_abi::OP_FS_CLOSE) if self.value_is_result_ok(vm, value) => {
                self.pending_resource_of(vm, ResourceErrors::Fs)
            }
            Some(lm_abi::OP_TCP_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(vm, value, self.core_of(vm).net_closed) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Net)
            }
            Some(lm_abi::OP_TLS_HANDSHAKE) | Some(lm_abi::OP_TLS_SERVER_HANDSHAKE) => {
                self.pending_resource_of(vm, ResourceErrors::Net)
            }
            Some(lm_abi::OP_TLS_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(vm, value, self.core_of(vm).tls_closed) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Tls)
            }
            Some(lm_abi::OP_TTY_EXIT_RAW)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(
                        vm,
                        value,
                        self.core_of(vm).tty_error_closed,
                    ) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Tty)
            }
            Some(lm_abi::OP_SIGNAL_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(
                        vm,
                        value,
                        self.core_of(vm).signal_error_closed,
                    ) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Signal)
            }
            Some(lm_abi::OP_PIPE_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(
                        vm,
                        value,
                        self.core_of(vm).pipe_error_closed,
                    ) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Pipe)
            }
            Some(lm_abi::OP_EXEC_WAIT) | Some(lm_abi::OP_EXEC_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(
                        vm,
                        value,
                        self.core_of(vm).exec_error_closed,
                    ) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Exec)
            }
            Some(lm_abi::OP_UDP_CLOSE)
                if self.value_is_result_ok(vm, value)
                    || self.value_is_result_error_class(vm, value, self.core_of(vm).net_closed) =>
            {
                self.pending_resource_of(vm, ResourceErrors::Net)
            }
            _ => None,
        };
        let spawn_closing = if self.pending_op(vm) == Some(lm_abi::OP_EXEC_SPAWN)
            && self.value_is_result_ok(vm, value)
        {
            self.pending_exec_pipe_resources(vm)
        } else {
            Vec::new()
        };
        if let Err(code) = self.check_reply(vm, value) {
            self.machines[vm as usize].set_fault(
                code,
                "the reply does not carry the type of its perform",
                None,
            );
            if let Some(resource) = closing {
                self.retire_resource(resource, close_host);
            }
            return;
        }
        let native_reply = self.machines[vm as usize].install_native_effect_reply(value);
        let m = &mut self.machines[vm as usize];
        // A completed request closes the host attachment it opened.
        if let Some(pending) = &m.vm.pending {
            let ordinal = pending.ordinal;
            m.resources.close_by_ordinal(ordinal);
        }
        m.vm.pending = None;
        match native_reply {
            Ok(true) => {
                if m.vm.state != MachineState::Running {
                    m.vm.state = MachineState::Ready;
                }
            }
            Ok(false) => {
                if let Err(code) = m.push(value) {
                    m.set_fault(code, "", None);
                } else if m.vm.state != MachineState::Running {
                    m.vm.state = MachineState::Ready;
                }
            }
            Err(code) => m.set_fault(code, "the native effect reply did not resume", None),
        }
        if let Some(resource) = closing {
            self.retire_resource(resource, close_host);
        }
        for resource in spawn_closing {
            self.retire_resource(resource, false);
        }
        // A machine that parked at `Asked` for its driver leaves the
        // run set. This reply makes it runnable again, so the scheduler
        // needs the event.
        self.notify_task_state(vm);
    }

    /// Report the current schedulability of one machine.
    pub(super) fn notify_task_state(&mut self, vm: VmId) {
        let Some(key) = self.task_key(vm) else {
            return;
        };
        if !self.scheduler_procs.contains(key) && vm != 0 {
            return;
        }
        match self.task_status(key) {
            TaskStatus::Ready => self.emit_ready(key),
            TaskStatus::Terminal => self.note_terminal(vm),
            _ => {}
        }
    }

    /// Convert one host reply into a guest value and install it.
    pub(super) fn install_host_reply(&mut self, vm: VmId, reply: HostValue) {
        let (reply_ty, env) = match self.reply_type(vm) {
            Ok(found) => found,
            Err(code) => {
                self.machines[vm as usize].set_fault(code, "", None);
                return;
            }
        };
        let op = self.pending_op(vm).unwrap_or(u32::MAX);
        let built = self.build_host_reply_value(vm, op, &reply, reply_ty, env);
        match built {
            Ok(value) => self.install_value_reply_with_file_close(vm, value, false),
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
        if self.machines[vm as usize].vm.state == MachineState::Faulted {
            self.close_resources_for_machine(vm);
        }
    }

    /// Convert one host reply through one stored static reply type.
    pub(super) fn build_host_reply_value(
        &mut self,
        vm: VmId,
        op: u32,
        reply: &HostValue,
        reply_ty: u32,
        env: TypeEnvId,
    ) -> Result<Value, FaultCode> {
        let extension_schema = (op >= lm_abi::OP_COUNT)
            .then(|| self.code_of(vm).bundle().op(op))
            .flatten()
            .map(|operation| operation.reply);
        if extension_schema.is_some_and(|schema| !extension_reply_matches(reply, schema)) {
            return Err(FaultCode::TypeMismatch);
        }
        let code = self.code_of(vm).clone();
        let expected = self
            .envs
            .close(code.as_ref(), reply_ty, env)
            .map_err(|_| FaultCode::BoundaryLimit)?;
        let first_resource = self.next_resource;
        let built = self.build_host_value(vm, reply, expected);
        if built.is_err() {
            let opened: Vec<u64> = self
                .bound_resources
                .range(first_resource..)
                .map(|(resource, _)| *resource)
                .collect();
            for resource in opened {
                self.retire_resource(resource, true);
            }
        }
        built
    }
}
