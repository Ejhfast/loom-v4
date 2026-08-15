//! The world: every machine, the one driver loop, policy resolution,
//! the boundary transfer, and the host completion channel.
//!
//! The driver executes nested machines through an explicit activation
//! stack. Machine records are data; the Rust stack never grows with
//! guest call depth or with nested VM depth. `run`, `step`, and
//! `drive` are stop modes of this one loop.

use crate::heap::{Heap, Object};
use crate::host::{CoreCtor, Host, HostArg, HostStart, HostValue};
use crate::machine::{
    Action, ExecOutcome, FaultRec, Machine, MachineState, Pending, Terminal, VmId,
};
use crate::{FaultCode, LoadedModule, Outcome, VmConfig};
use lm_bytecode::corelink::CoreLayout;
use lm_bytecode::{BcClassKind, Module};
use lm_value::{ObjRef, Value};
use std::collections::HashMap;

/// The fuel budget of one mock handler run.
const MOCK_FUEL: u64 = 1_000_000;

/// How one activation stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    RunToTerminal,
    OneStep,
    DriveToAsk,
}

/// The event family the consumer expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Run,
    Step,
    Drive,
    Mock,
}

/// One activation on the driver stack.
#[derive(Debug, Clone, Copy)]
struct Activation {
    vm: VmId,
    mode: StopMode,
    family: Family,
    /// The machine whose pending perform consumes the exit event.
    /// `None` delivers to the world caller.
    reply_to: Option<VmId>,
    /// True when this activation retired one instruction. `OneStep`
    /// stops when the flag is set and the machine can pause.
    retired: bool,
}

/// Why one activation exits.
#[derive(Debug, Clone, Copy)]
enum ExitKind {
    Terminal,
    Ran,
    Waiting,
}

/// One world-caller event.
#[derive(Debug)]
pub enum RootEvent {
    Ran,
    Waiting,
    Asked(u64),
    Done(Value),
    Fault(FaultRec),
}

/// The world: the loaded code plus every machine.
pub struct World<'m> {
    module: &'m Module,
    dispatch: &'m [Vec<u32>],
    core: CoreLayout,
    machines: Vec<Machine>,
    host: Box<dyn Host>,
    config: VmConfig,
}

impl<'m> World<'m> {
    /// Create a world with the entry loaded into the root machine.
    pub fn new(loaded: &'m LoadedModule, config: VmConfig, host: Box<dyn Host>) -> World<'m> {
        let module = loaded.module();
        let mut root = Machine::empty(config, None);
        root.load_frame(module, module.entry, Vec::new(), None);
        World {
            module,
            dispatch: loaded.dispatch(),
            core: lm_bytecode::corelink::core_layout(module),
            machines: vec![root],
            host,
            config,
        }
    }

    /// Grant one root policy target by name: an exact operation such
    /// as `Io.Print`, or a whole group such as `Clock`.
    pub fn allow(&mut self, name: &str) -> Result<(), String> {
        let table = &mut self.machines[0].table;
        if let Some(op) = lm_abi::op_by_name(name) {
            table.exact[op as usize] = Some(Action::Pass);
            return Ok(());
        }
        if let Some(group) = lm_abi::group_by_name(name) {
            table.group[group as usize] = Some(Action::Pass);
            return Ok(());
        }
        Err(format!(
            "`{name}` is not an operation or group in the operation manifest"
        ))
    }

    /// The root machine identifier.
    pub fn root(&self) -> VmId {
        0
    }

    /// Read access to one machine heap, for inspection and tests.
    pub fn heap_of(&self, vm: VmId) -> &Heap {
        &self.machines[vm as usize].heap
    }

    /// The state of one machine, for tests.
    pub fn state_of(&self, vm: VmId) -> MachineState {
        self.machines[vm as usize].state
    }

    /// The number of machines, for tests.
    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }

    /// Run the root machine to a terminal outcome.
    pub fn run_root(&mut self) -> Outcome {
        match self.control(0, StopMode::RunToTerminal, Family::Run) {
            RootEvent::Done(value) => Outcome::Done(value),
            RootEvent::Fault(rec) => Outcome::Fault(rec.code),
            // `run` waits out completions inside the loop.
            _ => unreachable!("run mode exits at a terminal only"),
        }
    }

    /// Retire one root instruction with automatic policy.
    pub fn step_root(&mut self) -> RootEvent {
        self.control(0, StopMode::OneStep, Family::Step)
    }

    /// The stored root fault record, for the CLI.
    pub fn root_fault(&self) -> Option<&FaultRec> {
        match &self.machines[0].terminal {
            Some(Terminal::Fault(rec)) => Some(rec),
            _ => None,
        }
    }

    /// Drive one machine with one stop mode. The public entry for the
    /// world caller; guest holders drive through `Vm.*` performs.
    fn control(&mut self, vm: VmId, mode: StopMode, family: Family) -> RootEvent {
        {
            let m = &self.machines[vm as usize];
            match m.state {
                MachineState::Done | MachineState::Faulted => {
                    return self.terminal_root_event(vm);
                }
                MachineState::Ready | MachineState::Waiting => {}
                _ => unreachable!("the world caller controls a paused machine"),
            }
        }
        let mut stack: Vec<Activation> = Vec::new();
        self.push_activation(
            &mut stack,
            Activation {
                vm,
                mode,
                family,
                reply_to: None,
                retired: false,
            },
        );
        self.drive_stack(&mut stack)
    }

    fn terminal_root_event(&self, vm: VmId) -> RootEvent {
        match &self.machines[vm as usize].terminal {
            Some(Terminal::Done(value)) => RootEvent::Done(*value),
            Some(Terminal::Fault(rec)) => RootEvent::Fault(rec.clone()),
            None => unreachable!("a terminal machine stores its result"),
        }
    }

    fn push_activation(&mut self, stack: &mut Vec<Activation>, act: Activation) {
        self.machines[act.vm as usize].active += 1;
        if let Some(p) = act.reply_to {
            self.machines[p as usize].active += 1;
        }
        // A waiting machine keeps its state: the loop completes the
        // pending host operation before it executes an instruction.
        let m = &mut self.machines[act.vm as usize];
        if m.state == MachineState::Ready {
            m.state = MachineState::Running;
        }
        stack.push(act);
    }

    /// The one driver loop over the activation stack.
    fn drive_stack(&mut self, stack: &mut Vec<Activation>) -> RootEvent {
        loop {
            let top_idx = stack.len() - 1;
            let act = stack[top_idx];
            let state = self.machines[act.vm as usize].state;
            match state {
                MachineState::Done | MachineState::Faulted => {
                    if let Some(event) = self.finish(stack, ExitKind::Terminal) {
                        return event;
                    }
                }
                MachineState::Waiting => {
                    if act.mode == StopMode::OneStep {
                        if act.retired {
                            if let Some(event) = self.finish(stack, ExitKind::Waiting) {
                                return event;
                            }
                            continue;
                        }
                        let token = self.wait_token(act.vm);
                        match self.host.poll(token) {
                            Some(reply) => self.install_host_reply(act.vm, reply),
                            None => {
                                if let Some(event) = self.finish(stack, ExitKind::Waiting) {
                                    return event;
                                }
                            }
                        }
                    } else {
                        let token = self.wait_token(act.vm);
                        let reply = self.host.wait(token);
                        self.install_host_reply(act.vm, reply);
                    }
                }
                MachineState::Ready => {
                    self.machines[act.vm as usize].state = MachineState::Running;
                }
                MachineState::Running => {
                    if act.mode == StopMode::OneStep && act.retired {
                        if let Some(event) = self.finish(stack, ExitKind::Ran) {
                            return event;
                        }
                        continue;
                    }
                    let outcome =
                        self.machines[act.vm as usize].exec_instr(self.module, self.dispatch);
                    stack[top_idx].retired = true;
                    match outcome {
                        Err(code) => {
                            self.machines[act.vm as usize].set_fault(code, "", None);
                        }
                        Ok(ExecOutcome::Continue) => {}
                        Ok(ExecOutcome::Terminal(value)) => {
                            self.machines[act.vm as usize].set_done(value);
                        }
                        Ok(ExecOutcome::Perform { op, args }) => {
                            if let Some(event) = self.handle_perform(stack, act.vm, op, args) {
                                return event;
                            }
                        }
                        Ok(ExecOutcome::TableEdit {
                            table,
                            action,
                            kind,
                            slot,
                            mock,
                        }) => self.handle_table_edit(act.vm, table, action, kind, slot, mock),
                        Ok(ExecOutcome::AsCall { request, op }) => {
                            self.handle_as_call(act.vm, request, op)
                        }
                        Ok(ExecOutcome::CallArgs { call }) => self.handle_call_args(act.vm, call),
                    }
                }
                MachineState::Empty | MachineState::Asked => {
                    unreachable!("an empty or asked machine is not on the driver stack")
                }
            }
        }
    }

    fn wait_token(&self, vm: VmId) -> u64 {
        self.machines[vm as usize]
            .pending
            .as_ref()
            .and_then(|p| p.wait_token)
            .expect("a waiting machine records its completion token")
    }

    /// Pop the top activation and deliver its exit event. Return the
    /// event when the consumer is the world caller.
    fn finish(&mut self, stack: &mut Vec<Activation>, kind: ExitKind) -> Option<RootEvent> {
        let act = stack.pop().expect("an activation exists");
        self.machines[act.vm as usize].active -= 1;
        if let Some(p) = act.reply_to {
            self.machines[p as usize].active -= 1;
        }
        // A paused exit leaves the machine holder-controlled.
        if matches!(kind, ExitKind::Ran) {
            let m = &mut self.machines[act.vm as usize];
            if m.state == MachineState::Running {
                m.state = MachineState::Ready;
            }
        }
        match act.reply_to {
            None => Some(match kind {
                ExitKind::Terminal => self.terminal_root_event(act.vm),
                ExitKind::Ran => RootEvent::Ran,
                ExitKind::Waiting => RootEvent::Waiting,
            }),
            Some(parent) => {
                self.deliver_event(act, parent, kind);
                None
            }
        }
    }

    /// Deliver one exit event of `act.vm` into `parent`.
    fn deliver_event(&mut self, act: Activation, parent: VmId, kind: ExitKind) {
        if act.family == Family::Mock {
            self.deliver_mock(act.vm, parent);
            return;
        }
        let value = match kind {
            ExitKind::Terminal => self.build_terminal_event(act.vm, parent, act.family),
            ExitKind::Ran => self.make_instance(parent, self.core.step_ran, vec![]),
            ExitKind::Waiting => self.make_instance(parent, self.core.step_waiting, vec![]),
        };
        match value {
            Ok(value) => self.install_value_reply(parent, value),
            Err(code) => self.machines[parent as usize].set_fault(code, "", None),
        }
    }

    /// Deliver a finished mock run as the raw perform reply.
    fn deliver_mock(&mut self, mock: VmId, target: VmId) {
        enum MockExit {
            Done(Value),
            Fault,
        }
        let exit = match &self.machines[mock as usize].terminal {
            Some(Terminal::Done(value)) => MockExit::Done(*value),
            Some(Terminal::Fault(_)) => MockExit::Fault,
            None => unreachable!("a finished mock stores its result"),
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
    }

    fn pending_op(&self, vm: VmId) -> Option<u32> {
        self.machines[vm as usize].pending.as_ref().map(|p| p.op)
    }

    /// Build the terminal event value of `child` in `parent`.
    fn build_terminal_event(
        &mut self,
        child: VmId,
        parent: VmId,
        family: Family,
    ) -> Result<Value, FaultCode> {
        enum T {
            Done(Value),
            Fault(FaultRec),
        }
        let t = match &self.machines[child as usize].terminal {
            Some(Terminal::Done(value)) => T::Done(*value),
            Some(Terminal::Fault(rec)) => T::Fault(rec.clone()),
            None => unreachable!("a terminal machine stores its result"),
        };
        match t {
            T::Done(value) => match self.transfer(child, parent, value) {
                Ok(value) => {
                    let class = self.done_arm(family);
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
                    };
                    self.build_fault_event(parent, family, &rec)
                }
            },
            T::Fault(rec) => self.build_fault_event(parent, family, &rec),
        }
    }

    fn done_arm(&self, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core.run_done,
            Family::Step => self.core.step_done,
            Family::Drive => self.core.drive_done,
            Family::Mock => unreachable!("mock exits carry no event"),
        }
    }

    fn fault_arm(&self, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core.run_fault,
            Family::Step => self.core.step_fault,
            Family::Drive => self.core.drive_fault,
            Family::Mock => unreachable!("mock exits carry no event"),
        }
    }

    fn build_fault_event(
        &mut self,
        parent: VmId,
        family: Family,
        rec: &FaultRec,
    ) -> Result<Value, FaultCode> {
        let fault = self.machines[parent as usize].alloc(Object::NativeFault {
            code: rec.code,
            message: rec.message.clone(),
            op: rec.op,
        })?;
        let class = self.fault_arm(family);
        self.make_instance(parent, class, vec![fault])
    }

    /// Allocate one core enum case instance. The verifier proved the
    /// class exists wherever an instruction needs it.
    fn make_instance(
        &mut self,
        vm: VmId,
        class: Option<u32>,
        fields: Vec<Value>,
    ) -> Result<Value, FaultCode> {
        let class = class.expect("the verifier required this core definition");
        self.machines[vm as usize].alloc(Object::Instance { class, fields })
    }

    /// Install one reply value into a machine whose pending perform
    /// completes now.
    fn install_value_reply(&mut self, vm: VmId, value: Value) {
        let m = &mut self.machines[vm as usize];
        m.pending = None;
        if let Err(code) = m.push(value) {
            m.set_fault(code, "", None);
            return;
        }
        if m.state != MachineState::Running {
            m.state = MachineState::Ready;
        }
    }

    /// Convert one host reply into a guest value and install it.
    fn install_host_reply(&mut self, vm: VmId, reply: HostValue) {
        match self.build_host_value(vm, &reply) {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    fn build_host_value(&mut self, vm: VmId, value: &HostValue) -> Result<Value, FaultCode> {
        match value {
            HostValue::Unit => Ok(Value::Unit),
            HostValue::Int(v) => Ok(Value::Int(*v)),
            HostValue::Str(s) => self.machines[vm as usize].alloc(Object::Str(s.clone())),
            HostValue::Ctor(ctor, parts) => {
                let mut fields = Vec::with_capacity(parts.len());
                for part in parts {
                    fields.push(self.build_host_value(vm, part)?);
                }
                let class = match ctor {
                    CoreCtor::Some => self.core.option_some,
                    CoreCtor::None => self.core.option_none,
                    CoreCtor::Ok => self.core.result_ok,
                    CoreCtor::Err => self.core.result_err,
                    CoreCtor::IoErrorFailed => self.core.io_error_failed,
                };
                self.make_instance(vm, class, fields)
            }
        }
    }

    /// Handle one perform of `vm`: record the pending request, then
    /// stop for a driver or resolve policy.
    fn handle_perform(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        op: u32,
        args: Vec<Value>,
    ) -> Option<RootEvent> {
        let m = &mut self.machines[vm as usize];
        let ordinal = m.next_ordinal;
        m.next_ordinal += 1;
        m.pending = Some(Pending {
            op,
            args,
            ordinal,
            wait_token: None,
        });
        let top = *stack
            .last()
            .expect("the performing machine is on the stack");
        debug_assert_eq!(top.vm, vm);
        if top.mode == StopMode::DriveToAsk {
            // Stop before policy lookup.
            let act = stack.pop().expect("an activation exists");
            self.machines[vm as usize].active -= 1;
            if let Some(p) = act.reply_to {
                self.machines[p as usize].active -= 1;
            }
            self.machines[vm as usize].state = MachineState::Asked;
            match act.reply_to {
                None => return Some(RootEvent::Asked(ordinal)),
                Some(parent) => {
                    self.deliver_asked(vm, parent, ordinal);
                    return None;
                }
            }
        }
        self.resolve_and_dispatch(stack, vm);
        None
    }

    /// Build and install `DriveEvent.Asked(request)` into `parent`.
    fn deliver_asked(&mut self, child: VmId, parent: VmId, ordinal: u64) {
        let built = self.machines[parent as usize]
            .alloc(Object::NativeRequest { vm: child, ordinal })
            .and_then(|request| self.make_instance(parent, self.core.drive_asked, vec![request]));
        match built {
            Ok(value) => self.install_value_reply(parent, value),
            Err(code) => self.machines[parent as usize].set_fault(code, "", None),
        }
    }

    /// Resolve the pending perform of `vm` through its policy chain
    /// and dispatch it.
    fn resolve_and_dispatch(&mut self, stack: &mut Vec<Activation>, vm: VmId) {
        let op = self
            .pending_op(vm)
            .expect("policy resolution needs a pending perform");
        match self.resolve_policy(vm, op) {
            Resolution::Denied => {
                self.machines[vm as usize].set_fault(
                    FaultCode::PolicyDenied,
                    format!("the operation {} is not granted", lm_abi::op_name(op)),
                    Some(op),
                );
            }
            Resolution::Mock { owner, closure } => self.start_mock(stack, vm, owner, closure),
            Resolution::Root => {
                if lm_abi::op(op).kind == lm_abi::OpKind::VmControl {
                    self.kernel_exec(stack, vm, op);
                } else {
                    let args = self.host_args(vm);
                    match self.host.start(op, args) {
                        HostStart::Completed(reply) => self.install_host_reply(vm, reply),
                        HostStart::Waiting(token) => {
                            let m = &mut self.machines[vm as usize];
                            m.pending
                                .as_mut()
                                .expect("the pending perform waits")
                                .wait_token = Some(token);
                            m.state = MachineState::Waiting;
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
    }

    /// Extract plain-data host arguments from the pending perform.
    fn host_args(&self, vm: VmId) -> Vec<HostArg> {
        let m = &self.machines[vm as usize];
        let pending = m.pending.as_ref().expect("a pending perform exists");
        pending
            .args
            .iter()
            .map(|value| match value {
                Value::Int(v) => HostArg::Int(*v),
                Value::Obj(r) => match m.heap.get(*r) {
                    Object::Str(text) => HostArg::Str(text.clone()),
                    _ => unreachable!("verified operation argument shape"),
                },
                _ => unreachable!("verified operation argument shape"),
            })
            .collect()
    }

    /// Walk the policy chain of `vm` for one exact operation.
    fn resolve_policy(&self, vm: VmId, op: u32) -> Resolution {
        let mut cur = vm;
        loop {
            let m = &self.machines[cur as usize];
            match m.table.lookup(op) {
                None | Some(Action::Block) => return Resolution::Denied,
                Some(Action::Mock(closure)) => {
                    return Resolution::Mock {
                        owner: cur,
                        closure,
                    };
                }
                Some(Action::Pass) => match m.parent {
                    Some(parent) => cur = parent,
                    None => return Resolution::Root,
                },
            }
        }
    }

    /// Run one mock handler in an ephemeral machine on the same loop.
    fn start_mock(&mut self, stack: &mut Vec<Activation>, vm: VmId, owner: VmId, closure: ObjRef) {
        let mock_config = VmConfig {
            fuel: MOCK_FUEL,
            ..self.config
        };
        let id = self.machines.len() as VmId;
        self.machines.push(Machine::empty(mock_config, None));
        let moved = if owner == vm {
            // The mock lives in the performing machine's own table.
            self.transfer(vm, id, Value::Obj(closure))
        } else {
            self.transfer(owner, id, Value::Obj(closure))
        };
        let closure_value = match moved {
            Ok(value) => value,
            Err(code) => {
                let op = self.pending_op(vm);
                self.machines[vm as usize].set_fault(code, "the mock handler is not sendable", op);
                return;
            }
        };
        let args: Vec<Value> = self.machines[vm as usize]
            .pending
            .as_ref()
            .expect("the mocked perform is pending")
            .args
            .clone();
        let mut moved_args = Vec::with_capacity(args.len());
        for arg in args {
            match self.transfer(vm, id, arg) {
                Ok(value) => moved_args.push(value),
                Err(code) => {
                    let op = self.pending_op(vm);
                    self.machines[vm as usize].set_fault(
                        code,
                        "an operation argument is not sendable",
                        op,
                    );
                    return;
                }
            }
        }
        let closure_ref = closure_value.as_obj().expect("a closure is a heap object");
        let func = match self.machines[id as usize].heap.get(closure_ref) {
            Object::Closure { func, .. } => *func,
            _ => unreachable!("a mock handler is a closure"),
        };
        self.machines[id as usize].load_frame(self.module, func, moved_args, Some(closure_ref));
        self.push_activation(
            stack,
            Activation {
                vm: id,
                mode: StopMode::RunToTerminal,
                family: Family::Mock,
                reply_to: Some(vm),
                retired: false,
            },
        );
    }

    /// Read one machine handle out of a holder value.
    fn handle_vm(&self, holder: VmId, value: Value) -> VmId {
        let r = value.as_obj().expect("verified handle value");
        match self.machines[holder as usize].heap.get(r) {
            Object::NativeVm { vm } => *vm,
            _ => unreachable!("verified handle shape"),
        }
    }

    /// Execute one VM control operation of the machine `vm`.
    fn kernel_exec(&mut self, stack: &mut Vec<Activation>, vm: VmId, op: u32) {
        let args: Vec<Value> = self.machines[vm as usize]
            .pending
            .as_ref()
            .expect("a pending perform exists")
            .args
            .clone();
        match op {
            lm_abi::OP_VM_NEW => {
                let child = self.machines.len() as VmId;
                self.machines.push(Machine::empty(self.config, Some(vm)));
                match self.machines[vm as usize].alloc(Object::NativeVm { vm: child }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_FROM_OBJECT => {
                let target = self.handle_vm(vm, args[0]);
                if self.machines[target as usize].state != MachineState::Empty {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is loaded");
                    return;
                }
                let program = match self.transfer(vm, target, args[1]) {
                    Ok(value) => value,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the program is not sendable");
                        return;
                    }
                };
                // The argument view: unit, or a tuple whose elements
                /* become the initial parameter locals. */
                let mut locals = Vec::new();
                if let Value::Obj(r) = args[2] {
                    let items = match self.machines[vm as usize].heap.get(r) {
                        Object::Tuple { items } => items.clone(),
                        _ => unreachable!("verified argument view shape"),
                    };
                    for item in items {
                        match self.transfer(vm, target, item) {
                            Ok(value) => locals.push(value),
                            Err(code) => {
                                self.fault_caller(vm, op, code, "an argument is not sendable");
                                return;
                            }
                        }
                    }
                }
                let closure_ref = program.as_obj().expect("a program value is a closure");
                let func = match self.machines[target as usize].heap.get(closure_ref) {
                    Object::Closure { func, .. } => *func,
                    _ => unreachable!("a program value is a closure"),
                };
                self.machines[target as usize].load_frame(
                    self.module,
                    func,
                    locals,
                    Some(closure_ref),
                );
                match self.machines[vm as usize].alloc(Object::NativeVm { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_RUN | lm_abi::OP_VM_STEP | lm_abi::OP_VM_DRIVE => {
                let target = self.handle_vm(vm, args[0]);
                let (mode, family) = match op {
                    lm_abi::OP_VM_RUN => (StopMode::RunToTerminal, Family::Run),
                    lm_abi::OP_VM_STEP => (StopMode::OneStep, Family::Step),
                    _ => (StopMode::DriveToAsk, Family::Drive),
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                match self.machines[target as usize].state {
                    MachineState::Empty => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine is empty",
                        );
                    }
                    MachineState::Done | MachineState::Faulted => {
                        // Terminal execution calls return the stored
                        // event idempotently.
                        match self.build_terminal_event(target, vm, family) {
                            Ok(value) => self.install_value_reply(vm, value),
                            Err(code) => {
                                self.machines[vm as usize].set_fault(code, "", Some(op));
                            }
                        }
                    }
                    MachineState::Asked => {
                        if op == lm_abi::OP_VM_DRIVE {
                            // Token recovery: the same semantic request
                            // with a fresh holder token.
                            let fresh = {
                                let m = &mut self.machines[target as usize];
                                let fresh = m.next_ordinal;
                                m.next_ordinal += 1;
                                m.pending
                                    .as_mut()
                                    .expect("an asked machine has a pending perform")
                                    .ordinal = fresh;
                                fresh
                            };
                            self.deliver_asked(target, vm, fresh);
                        } else {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::InvalidVmState,
                                "the machine is asked; drive it",
                            );
                        }
                    }
                    MachineState::Ready | MachineState::Waiting => {
                        self.machines[vm as usize].pending = None;
                        self.push_activation(
                            stack,
                            Activation {
                                vm: target,
                                mode,
                                family,
                                reply_to: Some(vm),
                                retired: false,
                            },
                        );
                    }
                    MachineState::Running => {
                        unreachable!("a running machine holds an active reference")
                    }
                }
            }
            lm_abi::OP_VM_TABLE => {
                let target = self.handle_vm(vm, args[0]);
                match self.machines[vm as usize].alloc(Object::NativeTable { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_ANSWER => {
                let target = self.handle_vm(vm, args[0]);
                let token = {
                    let r = args[1].as_obj().expect("verified call token");
                    match self.machines[vm as usize].heap.get(r) {
                        Object::NativeCall { vm, ordinal, op } => (*vm, *ordinal, *op),
                        _ => unreachable!("verified call token shape"),
                    }
                };
                if !self.expect_asked(vm, op, target) {
                    return;
                }
                let pending_ok = {
                    let pending = self.machines[target as usize]
                        .pending
                        .as_ref()
                        .expect("an asked machine has a pending perform");
                    token.0 == target && token.1 == pending.ordinal && token.2 == pending.op
                };
                if !pending_ok {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidRequestToken,
                        "the call token is stale or foreign",
                    );
                    return;
                }
                let reply = match self.transfer(vm, target, args[2]) {
                    Ok(value) => value,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the reply is not sendable");
                        return;
                    }
                };
                self.install_value_reply(target, reply);
                self.install_value_reply(vm, Value::Unit);
            }
            lm_abi::OP_VM_REJECT | lm_abi::OP_VM_DISPATCH => {
                let target = self.handle_vm(vm, args[0]);
                let token = {
                    let r = args[1].as_obj().expect("verified request token");
                    match self.machines[vm as usize].heap.get(r) {
                        Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
                        _ => unreachable!("verified request token shape"),
                    }
                };
                if !self.expect_asked(vm, op, target) {
                    return;
                }
                let pending_ok = {
                    let pending = self.machines[target as usize]
                        .pending
                        .as_ref()
                        .expect("an asked machine has a pending perform");
                    token.0 == target && token.1 == pending.ordinal
                };
                if !pending_ok {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidRequestToken,
                        "the request token is stale or foreign",
                    );
                    return;
                }
                if op == lm_abi::OP_VM_REJECT {
                    let rec = {
                        let r = args[2].as_obj().expect("verified fault value");
                        match self.machines[vm as usize].heap.get(r) {
                            Object::NativeFault { code, message, op } => FaultRec {
                                code: *code,
                                message: message.clone(),
                                op: *op,
                            },
                            _ => unreachable!("verified fault value shape"),
                        }
                    };
                    let pending_op = self.pending_op(target);
                    self.machines[target as usize].set_fault(rec.code, rec.message, pending_op);
                    self.install_value_reply(vm, Value::Unit);
                } else {
                    // Dispatch applies the controlled machine's own
                    // table. The caller's reply installs first; the
                    // resolution may stack a mock run above it.
                    self.install_value_reply(vm, Value::Unit);
                    self.resolve_and_dispatch(stack, target);
                }
            }
            _ => unreachable!("every VmControl slot has a kernel rule"),
        }
    }

    /// Check that a controlled machine accepts a continuation method.
    fn expect_asked(&mut self, vm: VmId, op: u32, target: VmId) -> bool {
        if target == vm || self.machines[target as usize].active > 0 {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
            return false;
        }
        if self.machines[target as usize].state != MachineState::Asked {
            // The machine has no pending request, so the caller's
            // token is consumed or stale (specification 12.3).
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidRequestToken,
                "the request token is consumed or stale",
            );
            return false;
        }
        true
    }

    /// Fault the calling machine without mutating the controlled one.
    fn fault_caller(&mut self, vm: VmId, op: u32, code: FaultCode, message: &str) {
        self.machines[vm as usize].set_fault(code, message, Some(op));
    }

    /// Apply one policy-table edit performed by `vm`.
    fn handle_table_edit(
        &mut self,
        vm: VmId,
        table: ObjRef,
        action: u32,
        kind: u32,
        slot: u32,
        mock: Option<Value>,
    ) {
        let target = match self.machines[vm as usize].heap.get(table) {
            Object::NativeTable { vm } => *vm,
            _ => unreachable!("verified table handle shape"),
        };
        let entry = match action {
            0 => Some(Action::Pass),
            1 => Some(Action::Block),
            2 => {
                let closure = mock.expect("a mock edit carries its handler");
                let moved = if target == vm {
                    Ok(closure)
                } else {
                    self.transfer(vm, target, closure)
                };
                match moved {
                    Ok(value) => Some(Action::Mock(
                        value.as_obj().expect("a mock handler is a closure"),
                    )),
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
        let t = &mut self.machines[target as usize].table;
        if kind == 0 {
            t.exact[slot as usize] = entry;
        } else {
            t.group[slot as usize] = entry;
        }
        // A table edit is an ordinary instruction: push the unit
        // result directly.
        if let Err(code) = self.machines[vm as usize].push(Value::Unit) {
            self.machines[vm as usize].set_fault(code, "", None);
        }
    }

    /// `request.as_call(op)` executed by `vm`.
    fn handle_as_call(&mut self, vm: VmId, request: ObjRef, op: u32) {
        let (rv, ordinal) = match self.machines[vm as usize].heap.get(request) {
            Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
            _ => unreachable!("verified request shape"),
        };
        let matches = {
            let m = &self.machines[rv as usize];
            m.state == MachineState::Asked
                && m.pending
                    .as_ref()
                    .map(|p| p.ordinal == ordinal && p.op == op)
                    .unwrap_or(false)
        };
        let built = if matches {
            self.machines[vm as usize]
                .alloc(Object::NativeCall {
                    vm: rv,
                    ordinal,
                    op,
                })
                .and_then(|call| self.make_instance(vm, self.core.option_some, vec![call]))
        } else {
            self.make_instance(vm, self.core.option_none, vec![])
        };
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    /// `call.args()` executed by `vm`.
    fn handle_call_args(&mut self, vm: VmId, call: ObjRef) {
        let (cv, ordinal, op) = match self.machines[vm as usize].heap.get(call) {
            Object::NativeCall { vm, ordinal, op } => (*vm, *ordinal, *op),
            _ => unreachable!("verified call shape"),
        };
        let matches = {
            let m = &self.machines[cv as usize];
            m.state == MachineState::Asked
                && m.pending
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
        let source_args: Vec<Value> = self.machines[cv as usize]
            .pending
            .as_ref()
            .expect("an asked machine has a pending perform")
            .args
            .clone();
        let built = if source_args.is_empty() {
            Ok(Value::Unit)
        } else {
            let mut items = Vec::with_capacity(source_args.len());
            let mut failed = None;
            for arg in source_args {
                match self.transfer(cv, vm, arg) {
                    Ok(value) => items.push(value),
                    Err(code) => {
                        failed = Some(code);
                        break;
                    }
                }
            }
            match failed {
                Some(code) => Err(code),
                None => self.machines[vm as usize].alloc(Object::Tuple { items }),
            }
        };
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    /// Split access to two distinct machines.
    fn two(&mut self, a: VmId, b: VmId) -> (&mut Machine, &mut Machine) {
        assert_ne!(a, b, "a boundary transfer needs two machines");
        let (a, b) = (a as usize, b as usize);
        if a < b {
            let (left, right) = self.machines.split_at_mut(b);
            (&mut left[a], &mut right[0])
        } else {
            let (left, right) = self.machines.split_at_mut(a);
            (&mut right[0], &mut left[b])
        }
    }

    /// Transfer one value from `src` into `dst`.
    ///
    /// The week-4 subset accepts scalars, first-class operation
    /// values, and deeply frozen graphs of strings, tuples, lists,
    /// maps, instances, closures, and fault values. It preserves
    /// cycles and sharing. Everything else faults `UnsendableValue`.
    pub(crate) fn transfer(
        &mut self,
        src: VmId,
        dst: VmId,
        value: Value,
    ) -> Result<Value, FaultCode> {
        let root = match value {
            Value::Unit | Value::Bool(_) | Value::Int(_) | Value::Op(_) => return Ok(value),
            Value::Uninit => unreachable!("no verified value is uninitialized"),
            Value::Obj(r) => r,
        };
        let (src_m, dst_m) = self.two(src, dst);
        // Pass 1: discover the reachable graph and check sendability.
        let mut order: Vec<ObjRef> = Vec::new();
        let mut index: HashMap<u32, usize> = HashMap::new();
        let mut work = vec![root];
        while let Some(r) = work.pop() {
            if index.contains_key(&r.slot) {
                continue;
            }
            if !src_m.heap.is_frozen(r) {
                return Err(FaultCode::UnsendableValue);
            }
            let object = src_m.heap.get(r);
            match object {
                Object::Str(_)
                | Object::Tuple { .. }
                | Object::List { .. }
                | Object::Map { .. }
                | Object::Instance { .. }
                | Object::Closure { .. }
                | Object::NativeFault { .. } => {}
                Object::StrBuilder(_)
                | Object::ByteBuf(_)
                | Object::NativeVm { .. }
                | Object::NativeTable { .. }
                | Object::NativeRequest { .. }
                | Object::NativeCall { .. } => return Err(FaultCode::UnsendableValue),
            }
            index.insert(r.slot, order.len());
            order.push(r);
            object.trace_children(&mut work);
        }
        // Pass 2: allocate shells in discovery order, rooted against
        // destination collections.
        let mut new_refs: Vec<ObjRef> = Vec::with_capacity(order.len());
        let mut result = Ok(());
        for r in &order {
            let shell = shell_of(src_m.heap.get(*r));
            let cost = shell.cost();
            if dst_m.heap.would_exceed(cost) {
                let mut extra = new_refs.clone();
                shell.trace_children(&mut extra);
                dst_m.collect_garbage(&extra);
                if dst_m.heap.would_exceed(cost) {
                    result = Err(FaultCode::HeapLimit);
                    break;
                }
            }
            let new_ref = dst_m.heap.alloc(shell);
            dst_m.heap.push_host_root(new_ref);
            new_refs.push(new_ref);
        }
        if result.is_ok() {
            // Pass 3: patch children through the identity map.
            let map_value = |value: Value| -> Value {
                match value {
                    Value::Obj(child) => Value::Obj(new_refs[index[&child.slot]]),
                    other => other,
                }
            };
            for (old, new) in order.iter().zip(new_refs.iter()) {
                let patched = match src_m.heap.get(*old) {
                    Object::Str(_) | Object::NativeFault { .. } => continue,
                    Object::Tuple { items } => Object::Tuple {
                        items: items.iter().map(|v| map_value(*v)).collect(),
                    },
                    Object::List { items } => Object::List {
                        items: items.iter().map(|v| map_value(*v)).collect(),
                    },
                    Object::Map { entries } => Object::Map {
                        entries: entries
                            .iter()
                            .map(|(k, v)| (map_value(*k), map_value(*v)))
                            .collect(),
                    },
                    Object::Instance { class, fields } => Object::Instance {
                        class: *class,
                        fields: fields.iter().map(|v| map_value(*v)).collect(),
                    },
                    Object::Closure { func, captures } => Object::Closure {
                        func: *func,
                        captures: captures.iter().map(|v| map_value(*v)).collect(),
                    },
                    _ => unreachable!("pass 1 admitted sendable shapes only"),
                };
                *dst_m.heap.get_mut(*new) = patched;
                dst_m.heap.recharge(*new);
            }
        }
        // Unroot in LIFO order.
        for r in new_refs.iter().rev() {
            dst_m.heap.pop_host_root(*r);
        }
        result?;
        let new_root = new_refs[index[&root.slot]];
        // The copy is as frozen as the source graph.
        dst_m.heap.freeze(new_root);
        Ok(Value::Obj(new_root))
    }
}

/// One resolution of a policy walk.
enum Resolution {
    Denied,
    Mock { owner: VmId, closure: ObjRef },
    Root,
}

/// An empty destination shell of one sendable object. Children are
/// patched afterwards; the shell keeps the payload sizes so the cost
/// accounting matches.
fn shell_of(object: &Object) -> Object {
    match object {
        Object::Str(s) => Object::Str(s.clone()),
        Object::NativeFault { code, message, op } => Object::NativeFault {
            code: *code,
            message: message.clone(),
            op: *op,
        },
        Object::Tuple { items } => Object::Tuple {
            items: vec![Value::Unit; items.len()],
        },
        Object::List { items } => Object::List {
            items: vec![Value::Unit; items.len()],
        },
        Object::Map { entries } => Object::Map {
            entries: vec![(Value::Unit, Value::Unit); entries.len()],
        },
        Object::Instance { class, fields } => Object::Instance {
            class: *class,
            fields: vec![Value::Unit; fields.len()],
        },
        Object::Closure { func, captures } => Object::Closure {
            func: *func,
            captures: vec![Value::Unit; captures.len()],
        },
        _ => unreachable!("pass 1 admitted sendable shapes only"),
    }
}

impl<'m> World<'m> {
    /// Render a terminal outcome as stable text.
    pub fn show_outcome(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Done(value) => format!("Done({})", self.show_value(*value)),
            Outcome::Fault(code) => format!("Fault({code})"),
        }
    }

    /// Render one root-machine value in a stable readable form.
    pub fn show_value(&self, value: Value) -> String {
        self.show_value_of(0, value)
    }

    /// Render one value of one machine.
    pub fn show_value_of(&self, vm: VmId, value: Value) -> String {
        let mut visited = Vec::new();
        self.show_value_inner(&self.machines[vm as usize].heap, value, 0, &mut visited)
    }

    fn show_value_inner(
        &self,
        heap: &Heap,
        value: Value,
        depth: u32,
        visited: &mut Vec<ObjRef>,
    ) -> String {
        const MAX_SHOW_DEPTH: u32 = 32;
        match value {
            Value::Unit => "()".to_string(),
            Value::Bool(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Op(op) => format!("<op {}>", lm_abi::op_name(op)),
            Value::Uninit => "<uninit>".to_string(),
            Value::Obj(r) => {
                if depth >= MAX_SHOW_DEPTH {
                    return "...".to_string();
                }
                if visited.contains(&r) {
                    return "<cycle>".to_string();
                }
                match heap.get(r) {
                    Object::Str(text) => render_string(text),
                    Object::List { items } => {
                        visited.push(r);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_value_inner(heap, *v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        format!("[{}]", parts.join(", "))
                    }
                    Object::Map { entries } => {
                        visited.push(r);
                        let parts: Vec<String> = entries
                            .iter()
                            .map(|(k, v)| {
                                format!(
                                    "{}: {}",
                                    self.show_value_inner(heap, *k, depth + 1, visited),
                                    self.show_value_inner(heap, *v, depth + 1, visited)
                                )
                            })
                            .collect();
                        visited.pop();
                        format!("{{{}}}", parts.join(", "))
                    }
                    Object::Tuple { items } => {
                        visited.push(r);
                        let parts: Vec<String> = items
                            .iter()
                            .map(|v| self.show_value_inner(heap, *v, depth + 1, visited))
                            .collect();
                        visited.pop();
                        if parts.len() == 1 {
                            format!("({},)", parts[0])
                        } else {
                            format!("({})", parts.join(", "))
                        }
                    }
                    Object::Instance { class, fields } => {
                        visited.push(r);
                        let bc = &self.module.classes[*class as usize];
                        let text = if bc.kind == BcClassKind::Case {
                            // A case instance prints in constructor
                            // form with its short arm name.
                            let short = bc.name.rsplit('.').next().unwrap_or(&bc.name);
                            if fields.is_empty() {
                                short.to_string()
                            } else {
                                let parts: Vec<String> = fields
                                    .iter()
                                    .map(|v| self.show_value_inner(heap, *v, depth + 1, visited))
                                    .collect();
                                format!("{}({})", short, parts.join(", "))
                            }
                        } else {
                            let parts: Vec<String> = bc
                                .fields
                                .iter()
                                .zip(fields.iter())
                                .map(|((name, _), v)| {
                                    format!(
                                        "{}: {}",
                                        name,
                                        self.show_value_inner(heap, *v, depth + 1, visited)
                                    )
                                })
                                .collect();
                            format!("{}{{{}}}", bc.name, parts.join(", "))
                        };
                        visited.pop();
                        text
                    }
                    Object::Closure { func, .. } => {
                        format!("<closure {}>", self.module.funcs[*func as usize].name)
                    }
                    Object::StrBuilder(buf) => format!("<StringBuilder len {}>", buf.len()),
                    Object::ByteBuf(bytes) => format!("<ByteBuffer len {}>", bytes.len()),
                    Object::NativeVm { vm } => format!("<vm {vm}>"),
                    Object::NativeTable { vm } => format!("<table {vm}>"),
                    Object::NativeRequest { .. } => "<request>".to_string(),
                    Object::NativeCall { op, .. } => {
                        format!("<call {}>", lm_abi::op_name(*op))
                    }
                    Object::NativeFault { code, .. } => code.to_string(),
                }
            }
        }
    }

    /// Render the live root-machine state: outcome, heap statistics,
    /// frame count, and every live object in slot order.
    pub fn dump_live(&self, outcome: &Outcome) -> String {
        use std::fmt::Write as _;
        let m = &self.machines[0];
        let mut out = String::new();
        let _ = writeln!(out, "outcome: {}", self.show_outcome(outcome));
        let s = m.heap.stats();
        let _ = writeln!(
            out,
            "heap: live={} slots={} pages={} free={} used_bytes={} cap_bytes={} collections={}",
            s.live, s.slots, s.pages, s.free, s.used_bytes, s.cap_bytes, s.collections
        );
        let _ = writeln!(out, "frames: {} active", m.frames.len());
        for frame in &m.frames {
            let func = &self.module.funcs[frame.func as usize];
            let _ = writeln!(
                out,
                "  frame {} block {} ip {}",
                func.name, frame.block, frame.ip
            );
        }
        let _ = writeln!(out, "objects:");
        m.heap.for_each_live(|r, frozen, _object| {
            let state = if frozen { "frozen" } else { "mutable" };
            let mut visited = Vec::new();
            let object = m.heap.get(r);
            let _ = writeln!(
                out,
                "  obj {} gen {} {} {} {}",
                r.slot,
                r.generation,
                object.shape().name,
                state,
                self.show_value_inner(&m.heap, Value::Obj(r), 0, &mut visited)
            );
        });
        out
    }
}

/// Render a string value with quotation marks and escapes.
pub(crate) fn render_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::NullHost;
    use crate::machine::Pending;
    use crate::{load, VmConfig};
    use lm_bytecode::{BcType, Func, Instr, Module};

    fn trivial_loaded() -> crate::LoadedModule {
        load(Module {
            strings: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(1), Instr::Return]],
            }],
            entry: 0,
        })
        .expect("the trivial module verifies")
    }

    /// Give machine 0 a pending VM-control perform over a handle to
    /// machine `target`.
    fn arm_pending(world: &mut World<'_>, op: u32, extra: Vec<Value>, target: VmId) {
        let handle = world.machines[0]
            .alloc(Object::NativeVm { vm: target })
            .expect("the handle allocates");
        let mut args = vec![handle];
        args.extend(extra);
        world.machines[0].pending = Some(Pending {
            op,
            args,
            ordinal: 1,
            wait_token: None,
        });
    }

    #[test]
    fn control_of_an_active_machine_faults_the_caller_only() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.state = MachineState::Ready;
        child.active = 1;
        world.machines.push(child);
        arm_pending(&mut world, lm_abi::OP_VM_RUN, vec![], 1);
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_RUN);
        assert_eq!(world.machines[0].state, MachineState::Faulted);
        match &world.machines[0].terminal {
            Some(Terminal::Fault(rec)) => assert_eq!(rec.code, FaultCode::InvalidVmState),
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine did not change.
        assert_eq!(world.machines[1].state, MachineState::Ready);
        assert_eq!(world.machines[1].active, 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn a_stale_token_faults_the_caller_and_keeps_the_request() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.state = MachineState::Asked;
        child.pending = Some(Pending {
            op: lm_abi::OP_CLOCK_NOW,
            args: vec![],
            ordinal: 7,
            wait_token: None,
        });
        world.machines.push(child);
        // A call token with a stale ordinal.
        let token = world.machines[0]
            .alloc(Object::NativeCall {
                vm: 1,
                ordinal: 6,
                op: lm_abi::OP_CLOCK_NOW,
            })
            .expect("the token allocates");
        arm_pending(
            &mut world,
            lm_abi::OP_VM_ANSWER,
            vec![token, Value::Int(1)],
            1,
        );
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_ANSWER);
        match &world.machines[0].terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine keeps its state and its request.
        assert_eq!(world.machines[1].state, MachineState::Asked);
        let pending = world.machines[1].pending.as_ref().expect("still pending");
        assert_eq!(pending.ordinal, 7);
    }

    #[test]
    fn a_cross_machine_token_faults_the_caller() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        for _ in 0..2 {
            let mut child = Machine::empty(VmConfig::default(), Some(0));
            child.state = MachineState::Asked;
            child.pending = Some(Pending {
                op: lm_abi::OP_CLOCK_NOW,
                args: vec![],
                ordinal: 1,
                wait_token: None,
            });
            world.machines.push(child);
        }
        // The token belongs to machine 2, the receiver is machine 1.
        let token = world.machines[0]
            .alloc(Object::NativeRequest { vm: 2, ordinal: 1 })
            .expect("the token allocates");
        arm_pending(&mut world, lm_abi::OP_VM_DISPATCH, vec![token], 1);
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_DISPATCH);
        match &world.machines[0].terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        assert_eq!(world.machines[1].state, MachineState::Asked);
        assert_eq!(world.machines[2].state, MachineState::Asked);
    }
}
