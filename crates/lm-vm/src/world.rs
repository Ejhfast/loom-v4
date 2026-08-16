//! The world: every machine, the one driver loop, policy resolution,
//! the boundary transfer, and the host completion channel.
//!
//! The driver executes nested machines through an explicit activation
//! stack. Machine records are data; the Rust stack never grows with
//! guest call depth or with nested VM depth. `run`, `step`, and
//! `drive` are stop modes of this one loop.

use crate::host::{CoreCtor, Host, HostArg, HostStart, HostValue};
use crate::machine::{
    Action, Block, ExecOutcome, FaultRec, Machine, MachineState, Mailbox, Ownership, Pending,
    Terminal, VmId,
};
use crate::{FaultCode, LoadedModule, Outcome, VmConfig};
use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{BcClassKind, Module};
use lm_heap::{Heap, Object};
use lm_value::{ObjRef, Value};

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
    /// The driven machine blocked on another machine of this world.
    /// The activation stack is stored; the scheduler resumes it.
    Blocked,
    Done(Value),
    Fault(FaultRec),
}

/// The stop of one scheduler-driven proc slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcStop {
    /// The proc blocked on another machine of this world.
    Blocked,
    /// The proc reached a terminal result.
    Terminal,
    /// The proc waits for a host completion.
    Waiting,
}

/// The verified semantic identity of the loaded code, for the
/// canonical digest.
///
/// A closure holds a numeric function slot and an instance holds a
/// numeric class slot. Both slots belong to this linked program only,
/// so the digest encoder reads the definition hash instead.
struct ModuleCodes<'m> {
    identity: &'m lm_bytecode::identity::ModuleIdentity,
}

impl lm_graph::CodeIdentity for ModuleCodes<'_> {
    fn func_hash(&self, func: u32) -> Result<[u8; 32], FaultCode> {
        self.identity
            .func_hashes
            .get(func as usize)
            .copied()
            .ok_or(FaultCode::BoundaryViolation)
    }

    fn class_hash(&self, class: u32) -> Result<[u8; 32], FaultCode> {
        self.identity
            .class_hashes
            .get(class as usize)
            .copied()
            .ok_or(FaultCode::BoundaryViolation)
    }
}

/// The world: the loaded code plus every machine.
pub struct World<'m> {
    loaded: &'m LoadedModule,
    module: &'m Module,
    dispatch: &'m [crate::DispatchRow],
    core: CoreLayout,
    pub(crate) machines: Vec<Machine>,
    /// Retired mock-handler slots, ready for reuse.
    ///
    /// A mock machine is ephemeral: no guest value names it, it takes
    /// no child, and it cannot reach an asked state. One mocked
    /// perform therefore leaves nothing behind, and the next mock
    /// takes the same slot. Without the list, a loop of mocked
    /// performs grows the machine table without any bound.
    mock_free: Vec<VmId>,
    /// Suspended activation stacks, keyed by the machine the stack
    /// started from.
    ///
    /// A machine that blocks on another machine of this world stops
    /// its whole activation stack. The scheduler runs other machines
    /// and resumes the stored stack when the block clears. The record
    /// holds machine identifiers and stop modes only, never a guest
    /// heap reference.
    suspended: std::collections::BTreeMap<VmId, Vec<Activation>>,
    host: Box<dyn Host>,
    config: VmConfig,
    /// The proc trace, when tracing is on.
    trace: Option<Vec<TraceEvent>>,
    /// The monotone mailbox cut marker of this world.
    cut: u64,
    /// The monotone world-gate marker of this world.
    ///
    /// A restore puts every machine it builds behind one gate. The
    /// first `run`, `step`, or `drive` of the restored root opens that
    /// gate for the whole restored world (specification 17.5).
    gate: u32,
    /// The number of whole-image structural checks this world ran.
    ///
    /// The count instruments the rule of specification 17.8: external
    /// bytes are checked once, and a later restore repeats nothing.
    checks: u64,
    /// The images this world already trusts, newest first.
    ///
    /// A guest holds a snapshot as container bytes. A restore looks
    /// the bytes up by container hash: a hit is an image this process
    /// wrote or already checked, so the restore reads the decoded
    /// world and repeats no structural check. A miss runs the external
    /// loader once. The table is bounded, so a program that captures
    /// in a loop never grows it; an evicted image is checked again on
    /// its next restore, which is safe and slower.
    trusted: Vec<([u8; 32], std::sync::Arc<crate::snapshot::Image>)>,
    /// The last image a guest capture produced in this world.
    ///
    /// `lm snapshot save` writes it, so a program states in its own
    /// source which world a checkpoint holds.
    last_image: Option<crate::snapshot::SnapshotImage>,
}

/// The largest number of trusted images one world remembers.
const TRUSTED_IMAGES: usize = 64;

/// One recorded scheduler event. A trace record names machines by
/// identifier and generation, never by a guest reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEvent {
    Spawn {
        parent: VmId,
        proc: VmId,
        generation: u32,
    },
    Send {
        from: VmId,
        to: VmId,
        accepted: bool,
    },
    Receive {
        proc: VmId,
        closed: bool,
    },
    Close {
        proc: VmId,
        first: bool,
    },
    Block {
        vm: VmId,
        block: Block,
    },
    Unblock {
        vm: VmId,
    },
    Pause {
        proc: VmId,
    },
    Resume {
        proc: VmId,
    },
    Terminal {
        proc: VmId,
        faulted: bool,
    },
}

/// The mailbox counters of one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxMetrics {
    pub limit: u32,
    pub queued: u32,
    pub accepted: u64,
    pub delivered: u64,
    pub closed: bool,
    pub frozen: bool,
}

impl<'m> World<'m> {
    /// Create a world with the entry loaded into the root machine.
    pub fn new(loaded: &'m LoadedModule, config: VmConfig, host: Box<dyn Host>) -> World<'m> {
        let module = loaded.module();
        let mut root = Machine::empty(config, None);
        root.load_frame(module, module.entry, Vec::new(), None);
        World {
            loaded,
            module,
            dispatch: loaded.dispatch(),
            core: loaded.core_layout(),
            machines: vec![root],
            mock_free: Vec::new(),
            suspended: std::collections::BTreeMap::new(),
            host,
            config,
            trace: None,
            cut: 0,
            gate: 0,
            checks: 0,
            trusted: Vec::new(),
            last_image: None,
        }
    }

    /// Turn the proc trace on. The trace records scheduler events in
    /// order, by machine identifier and generation.
    pub fn trace_procs(&mut self) {
        self.trace = Some(Vec::new());
    }

    /// The recorded proc trace.
    pub fn trace(&self) -> &[TraceEvent] {
        self.trace.as_deref().unwrap_or(&[])
    }

    /// A readable dump of the proc trace, one event per line.
    ///
    /// Every line names machines by identifier, so the dump repeats
    /// exactly when the scheduler repeats.
    pub fn dump_trace(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for event in self.trace() {
            let _ = writeln!(out, "{}", show_trace_event(event));
        }
        out
    }

    /// A readable dump of the mailbox of every proc machine.
    pub fn dump_mailboxes(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for vm in 0..self.machines.len() as VmId {
            if self.machines[vm as usize].owner != Ownership::Scheduler {
                continue;
            }
            let m = self.mailbox_metrics(vm);
            let _ = writeln!(
                out,
                "proc {vm} gen {} limit {} queued {} accepted {} delivered {} \
                 closed {} frozen {}",
                self.machines[vm as usize].generation,
                m.limit,
                m.queued,
                m.accepted,
                m.delivered,
                m.closed,
                m.frozen
            );
        }
        out
    }

    fn record(&mut self, event: TraceEvent) {
        if let Some(trace) = &mut self.trace {
            trace.push(event);
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
        &self.machines[vm as usize].vm.heap
    }

    /// The state of one machine, for tests.
    pub fn state_of(&self, vm: VmId) -> MachineState {
        self.machines[vm as usize].vm.state
    }

    /// The number of machines, for tests.
    pub fn machine_count(&self) -> usize {
        self.machines.len()
    }

    /// Run the root machine to a terminal outcome.
    ///
    /// This entry admits no proc block. A program that launches procs
    /// runs through the scheduler of `lm-proc`, which resumes a
    /// blocked stack after it runs another machine.
    pub fn run_root(&mut self) -> Outcome {
        match self.control(0, StopMode::RunToTerminal, Family::Run) {
            RootEvent::Done(value) => Outcome::Done(value),
            RootEvent::Fault(rec) => Outcome::Fault(rec.code),
            RootEvent::Blocked => {
                // No scheduler drives this world, so the block can
                // never clear. The root faults instead of hanging.
                self.fail_blocked(0, "no scheduler drives this world");
                match self.terminal_root_event(0) {
                    RootEvent::Fault(rec) => Outcome::Fault(rec.code),
                    _ => unreachable!("a failed block stores a fault"),
                }
            }
            // `run` waits out completions inside the loop.
            _ => unreachable!("run mode exits at a terminal only"),
        }
    }

    /// Drive the root machine to a terminal result or to a block.
    /// The scheduler of `lm-proc` calls it.
    pub fn drive_root(&mut self) -> RootEvent {
        // A barrier stops the machines of its set for the length of
        // one call, so no scheduler slice runs inside one.
        debug_assert!(
            self.machines[0].barrier.is_none(),
            "a barrier holds the root machine"
        );
        if self.suspended.contains_key(&0) {
            return self.resume_stack(0);
        }
        let state = self.machines[0].vm.state;
        if state == MachineState::Blocked {
            return RootEvent::Blocked;
        }
        self.control(0, StopMode::RunToTerminal, Family::Run)
    }

    /// Resume one suspended activation stack.
    fn resume_stack(&mut self, vm: VmId) -> RootEvent {
        let mut stack = self
            .suspended
            .remove(&vm)
            .expect("the caller proved a suspended stack");
        self.drive_stack(&mut stack)
    }

    /// Retire one root instruction with automatic policy.
    pub fn step_root(&mut self) -> RootEvent {
        self.control(0, StopMode::OneStep, Family::Step)
    }

    /// Drive one machine of this world to a terminal result, a block,
    /// or a pending request. Tools call it; guest holders drive
    /// through `Vm.*` performs.
    ///
    /// A restored world lives beside the machine that restored it, so
    /// `lm snapshot run` needs an entry that names a machine other
    /// than the root.
    pub fn run_machine(&mut self, vm: VmId) -> RootEvent {
        // The first run, step, or drive of a restored root opens the
        // world gate, whatever the root state is.
        self.open_gate(vm);
        if self.suspended.contains_key(&vm) {
            return self.resume_stack(vm);
        }
        match self.machines[vm as usize].vm.state {
            MachineState::Blocked => RootEvent::Blocked,
            MachineState::Asked => {
                let ordinal = self.machines[vm as usize]
                    .vm
                    .pending
                    .as_ref()
                    .expect("an asked machine holds its request")
                    .ordinal;
                RootEvent::Asked(ordinal)
            }
            MachineState::Empty => RootEvent::Ran,
            _ => self.control(vm, StopMode::RunToTerminal, Family::Run),
        }
    }

    /// Create one empty child machine of `parent`, for tools.
    ///
    /// The reservation charges the parent budget, exactly as `Vm.New`
    /// charges it for guest code.
    pub fn new_child(&mut self, parent: VmId) -> Option<VmId> {
        let config = self.reserve_child(parent)?;
        let id = self.machines.len() as VmId;
        self.machines.push(Machine::empty(config, Some(parent)));
        Some(id)
    }

    /// Grant one root policy target to one machine, for tools.
    pub fn allow_on(&mut self, vm: VmId, name: &str) -> Result<(), String> {
        let table = &mut self.machines[vm as usize].table;
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

    /// The number of policy entries the table of one machine holds.
    ///
    /// A restored machine starts default-deny, so the count states
    /// exactly what restore granted.
    pub fn table_entry_count(&self, vm: VmId) -> usize {
        let table = &self.machines[vm as usize].table;
        table.exact.iter().flatten().count() + table.group.iter().flatten().count()
    }

    /// True when the table of one machine passes one group by name.
    pub fn table_passes_group(&self, vm: VmId, name: &str) -> bool {
        let Some(group) = lm_abi::group_by_name(name) else {
            return false;
        };
        matches!(
            self.machines[vm as usize].table.group[group as usize],
            Some(Action::Pass)
        )
    }

    /// The pending operation slot of one machine, for tools.
    pub fn pending_op_of(&self, vm: VmId) -> Option<u32> {
        self.pending_op(vm)
    }

    /// The last image a guest capture produced in this world.
    pub fn last_snapshot(&self) -> Option<&crate::snapshot::SnapshotImage> {
        self.last_image.as_ref()
    }

    /// The stored root fault record, for the CLI.
    pub fn root_fault(&self) -> Option<&FaultRec> {
        self.fault_of(0)
    }

    /// The stored fault record of one machine.
    pub fn fault_of(&self, vm: VmId) -> Option<&FaultRec> {
        match &self.machines[vm as usize].vm.terminal {
            Some(Terminal::Fault(rec)) => Some(rec),
            _ => None,
        }
    }

    /// Drive one machine with one stop mode. The public entry for the
    /// world caller; guest holders drive through `Vm.*` performs.
    fn control(&mut self, vm: VmId, mode: StopMode, family: Family) -> RootEvent {
        {
            let m = &self.machines[vm as usize];
            match m.vm.state {
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
        match &self.machines[vm as usize].vm.terminal {
            Some(Terminal::Done(value)) => RootEvent::Done(*value),
            Some(Terminal::Fault(rec)) => RootEvent::Fault(rec.clone()),
            None => unreachable!("a terminal machine stores its result"),
        }
    }

    fn push_activation(&mut self, stack: &mut Vec<Activation>, act: Activation) {
        // A restored world runs behind one gate until its root moves.
        self.open_gate(act.vm);
        self.machines[act.vm as usize].active += 1;
        if let Some(p) = act.reply_to {
            self.machines[p as usize].active += 1;
        }
        // A waiting machine keeps its state: the loop completes the
        // pending host operation before it executes an instruction.
        let m = &mut self.machines[act.vm as usize];
        if m.vm.state == MachineState::Ready {
            m.vm.state = MachineState::Running;
        }
        stack.push(act);
    }

    /// The one driver loop over the activation stack.
    fn drive_stack(&mut self, stack: &mut Vec<Activation>) -> RootEvent {
        loop {
            let top_idx = stack.len() - 1;
            let act = stack[top_idx];
            let state = self.machines[act.vm as usize].vm.state;
            match state {
                MachineState::Blocked => {
                    // The whole stack stops. Every activation keeps
                    // its execution reference, so no control call can
                    // reach a machine of the stopped stack.
                    let base = stack[0].vm;
                    self.suspended.insert(base, std::mem::take(stack));
                    return RootEvent::Blocked;
                }
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
                    self.machines[act.vm as usize].vm.state = MachineState::Running;
                }
                MachineState::Running => {
                    if act.mode == StopMode::OneStep && act.retired {
                        if let Some(event) = self.finish(stack, ExitKind::Ran) {
                            return event;
                        }
                        continue;
                    }
                    let machine = &mut self.machines[act.vm as usize];
                    let outcome = if act.mode == StopMode::OneStep {
                        machine.exec_instr(self.module, self.dispatch)
                    } else {
                        machine.exec_until_boundary(self.module, self.dispatch)
                    };
                    stack[top_idx].retired = true;
                    match outcome {
                        Err(code) => {
                            self.machines[act.vm as usize].set_fault(code, "", None);
                        }
                        Ok(ExecOutcome::Continue) => {}
                        Ok(ExecOutcome::Terminal(value)) => {
                            // A launched proc runs two frames: the
                            // constructor, then the proc body over the
                            // constructed instance.
                            if self.machines[act.vm as usize].start_body.is_some() {
                                self.enter_proc_body(act.vm, value);
                            } else {
                                self.machines[act.vm as usize].set_done(value);
                            }
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
                        Ok(ExecOutcome::Digest { value }) => self.handle_digest(act.vm, value),
                    }
                }
                MachineState::Empty | MachineState::Asked => {
                    unreachable!("an empty or asked machine is not on the driver stack")
                }
            }
        }
    }

    /// The host completion token of a waiting machine.
    ///
    /// The token is host work, so it lives in the resource registry
    /// beside the machine, never in the serializable `VmState`. The
    /// pending request ordinal links the two.
    fn wait_token(&self, vm: VmId) -> u64 {
        let m = &self.machines[vm as usize];
        let ordinal =
            m.vm.pending
                .as_ref()
                .expect("a waiting machine has a pending perform")
                .ordinal;
        m.resources
            .pending(ordinal)
            .expect("a waiting machine holds its pending-operation resource")
            .scope
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
            if m.vm.state == MachineState::Running {
                m.vm.state = MachineState::Ready;
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
        let exit = match &self.machines[mock as usize].vm.terminal {
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
        self.retire_mock(mock);
    }

    /// Retire one finished mock machine.
    ///
    /// The result already crossed into the target, and no guest value
    /// names a mock machine, so the record drops its heap now and the
    /// slot joins the free list.
    fn retire_mock(&mut self, mock: VmId) {
        debug_assert!(self.machines[mock as usize].active == 0);
        // The slot takes a new generation, so a reference minted for
        // the retired record names a dead machine, never the next one.
        let generation = self.machines[mock as usize].generation.wrapping_add(1);
        self.machines[mock as usize] = Machine::empty_at(self.config, None, generation);
        self.mock_free.push(mock);
    }

    fn pending_op(&self, vm: VmId) -> Option<u32> {
        self.machines[vm as usize].vm.pending.as_ref().map(|p| p.op)
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
        let t = match &self.machines[child as usize].vm.terminal {
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

    /// Allocate one core enum case instance.
    ///
    /// The verifier proves the parent slot wherever an instruction
    /// needs the family, and it rejects a family that resolves
    /// without every arm. The arm slot is therefore present.
    fn make_instance(
        &mut self,
        vm: VmId,
        class: Option<u32>,
        fields: Vec<Value>,
    ) -> Result<Value, FaultCode> {
        let class = class.expect("the verifier requires the whole core family");
        self.machines[vm as usize].alloc(Object::Instance { class, fields })
    }

    /// Install one reply value into a machine whose pending perform
    /// completes now.
    fn install_value_reply(&mut self, vm: VmId, value: Value) {
        let m = &mut self.machines[vm as usize];
        // A completed request closes the host attachment it opened.
        if let Some(pending) = &m.vm.pending {
            let ordinal = pending.ordinal;
            m.resources.close_by_ordinal(ordinal);
        }
        m.vm.pending = None;
        if let Err(code) = m.push(value) {
            m.set_fault(code, "", None);
            return;
        }
        if m.vm.state != MachineState::Running {
            m.vm.state = MachineState::Ready;
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
        let ordinal = m.vm.next_ordinal;
        m.vm.next_ordinal += 1;
        m.vm.pending = Some(Pending { op, args, ordinal });
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
            self.machines[vm as usize].vm.state = MachineState::Asked;
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
            Resolution::DeadParent => {
                // The denial has one cause the holder cannot see from
                // the code alone, so the message names it.
                self.machines[vm as usize].set_fault(
                    FaultCode::PolicyDenied,
                    format!(
                        "the operation {} lost its pass through: \
                         the parent machine is gone",
                        lm_abi::op_name(op)
                    ),
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
    fn start_wait(&mut self, vm: VmId, op: u32, token: u64) {
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
        let ordinal = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .expect("the pending perform waits")
            .ordinal;
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
    fn host_args(&self, vm: VmId) -> Vec<HostArg> {
        let m = &self.machines[vm as usize];
        let pending = m.vm.pending.as_ref().expect("a pending perform exists");
        pending
            .args
            .iter()
            .map(|value| match value {
                Value::Int(v) => HostArg::Int(*v),
                Value::Obj(r) => match m.vm.heap.get(*r) {
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
                Some(Action::Pass) => match m.vm.parent {
                    Some(parent) => {
                        // A child table passes through the live parent
                        // table. Parent death removes the pass
                        // through, and a later request fails closed
                        // (specification 18.6).
                        if matches!(
                            self.machines[parent as usize].vm.state,
                            MachineState::Done | MachineState::Faulted
                        ) {
                            return Resolution::DeadParent;
                        }
                        cur = parent;
                    }
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
        // Reuse a retired mock slot before the table grows.
        let id = match self.mock_free.pop() {
            Some(id) => {
                self.machines[id as usize] = Machine::empty(mock_config, None);
                id
            }
            None => {
                let id = self.machines.len() as VmId;
                self.machines.push(Machine::empty(mock_config, None));
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
        let args: Vec<Value> = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .expect("the mocked perform is pending")
            .args
            .clone();
        // The handler is not reachable from the mock machine yet, so
        // it stays rooted while the arguments cross.
        let closure_ref = closure_value.as_obj().expect("a closure is a heap object");
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
        let func = match self.machines[id as usize].vm.heap.get(closure_ref) {
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

    /// Reserve one child machine from the budget of `parent`.
    ///
    /// The parent holds a child budget. Each reservation charges one
    /// unit to the parent and hands the rest of the budget to the
    /// child, so the machine tower can never grow deeper than the
    /// budget the root minted. The reservation happens before any
    /// machine record exists, so a refusal changes nothing.
    ///
    /// The budget bounds tower depth per branch. It does not bound the
    /// total machine count across branches; full transitive accounting
    /// of fuel and heap bytes waits for the proc scheduler.
    pub(crate) fn reserve_child(&mut self, parent: VmId) -> Option<VmConfig> {
        let m = &mut self.machines[parent as usize];
        let budget = m.config.max_children;
        if m.children >= budget {
            return None;
        }
        m.children += 1;
        let remaining = budget - m.children;
        Some(VmConfig {
            max_children: remaining,
            ..m.config
        })
    }

    /// Enter the proc body after the constructor frame returned.
    fn enter_proc_body(&mut self, vm: VmId, instance: Value) {
        let body = self.machines[vm as usize]
            .start_body
            .take()
            .expect("the caller proved a stored proc body");
        let func = match self.machines[vm as usize].vm.heap.get(body) {
            Object::Closure { func, .. } => *func,
            _ => unreachable!("a proc body is a closure"),
        };
        self.machines[vm as usize].load_frame(self.module, func, vec![instance], Some(body));
    }

    /// Read one machine handle out of a holder value.
    fn handle_vm(&self, holder: VmId, value: Value) -> VmId {
        let r = value.as_obj().expect("verified handle value");
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeVm { vm } => *vm,
            _ => unreachable!("verified handle shape"),
        }
    }

    /// Execute one VM control operation of the machine `vm`.
    fn kernel_exec(&mut self, stack: &mut Vec<Activation>, vm: VmId, op: u32) {
        let args: Vec<Value> = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .expect("a pending perform exists")
            .args
            .clone();
        match op {
            lm_abi::OP_VM_NEW => {
                // The parent reserves the child from its own budget
                // first. The reservation is fail-atomic: a rejected
                // reservation creates no machine and charges nothing.
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
                self.machines.push(Machine::empty(child_config, Some(vm)));
                match self.machines[vm as usize].alloc(Object::NativeVm { vm: child }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => {
                        // No handle names the child, so the whole call
                        // rolls back: the record goes and the parent
                        // gets its reservation back.
                        self.machines.pop();
                        self.machines[vm as usize].children -= 1;
                        self.machines[vm as usize].set_fault(code, "", Some(op));
                    }
                }
            }
            lm_abi::OP_VM_FROM_OBJECT => {
                let target = self.handle_vm(vm, args[0]);
                if self.machines[target as usize].vm.state != MachineState::Empty {
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
                let closure_ref = program.as_obj().expect("a program value is a closure");
                let mut locals = Vec::new();
                if let Value::Obj(r) = args[2] {
                    let items = match self.machines[vm as usize].vm.heap.get(r) {
                        Object::Tuple { items } => items.clone(),
                        _ => unreachable!("verified argument view shape"),
                    };
                    // The program is not reachable from the target
                    // machine yet, so it stays rooted while the
                    // arguments cross.
                    self.machines[target as usize]
                        .vm
                        .heap
                        .push_host_root(closure_ref);
                    let moved = self.transfer_all(vm, target, &items);
                    self.machines[target as usize]
                        .vm
                        .heap
                        .pop_host_root(closure_ref);
                    match moved {
                        Ok(values) => locals = values,
                        Err(code) => {
                            self.fault_caller(vm, op, code, "an argument is not sendable");
                            return;
                        }
                    }
                }
                let func = match self.machines[target as usize].vm.heap.get(closure_ref) {
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
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                // The first run, step, or drive of a restored root
                // opens the world gate (specification 17.5).
                self.open_gate(target);
                match self.machines[target as usize].vm.state {
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
                                let fresh = m.vm.next_ordinal;
                                m.vm.next_ordinal += 1;
                                m.vm.pending
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
                        self.machines[vm as usize].vm.pending = None;
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
                    MachineState::Blocked => {
                        // A holder-owned machine blocks only inside a
                        // proc operation of its own stack, and that
                        // stack holds an execution reference.
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine is blocked on another machine",
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
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeCall { vm, ordinal, op } => (*vm, *ordinal, *op),
                        _ => unreachable!("verified call token shape"),
                    }
                };
                if !self.expect_asked(vm, op, target) {
                    return;
                }
                let pending_ok = {
                    let pending = self.machines[target as usize]
                        .vm
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
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
                        _ => unreachable!("verified request token shape"),
                    }
                };
                if !self.expect_asked(vm, op, target) {
                    return;
                }
                let pending_ok = {
                    let pending = self.machines[target as usize]
                        .vm
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
                        match self.machines[vm as usize].vm.heap.get(r) {
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
            lm_abi::OP_VM_SNAPSHOT_HELD => {
                let target = self.handle_vm(vm, args[0]);
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                self.take_snapshot(vm, op, target, false);
            }
            lm_abi::OP_VM_SNAPSHOT_SELF => {
                // The performing machine is the root of its own world.
                // The capture runs while `Vm.SnapshotSelf` is pending,
                // so the restored root holds that request
                // (specification 17.6).
                self.take_snapshot(vm, op, vm, true);
            }
            lm_abi::OP_VM_LOAD_SNAPSHOT => {
                // Version 0.2 has no `Bytes` value, so no guest code
                // can build the argument. The verifier rejects the
                // instruction; this arm states the same rule for a
                // hand-built module that reached the kernel.
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "Vm.LoadSnapshot has no guest form without a Bytes value",
                );
            }
            lm_abi::OP_VM_RESTORE => self.restore_snapshot(vm, op, &args),
            lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_SPAWN
            | lm_abi::OP_PROC_SEND
            | lm_abi::OP_PROC_CLOSE
            | lm_abi::OP_PROC_RECV
            | lm_abi::OP_PROC_DONE
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME => self.proc_exec(vm, op, args),
            _ => unreachable!("every VmControl slot has a kernel rule"),
        }
    }

    // ------------------------------------------------------------
    // The snapshot operations of specification 23.5.
    // ------------------------------------------------------------

    /// Capture one machine world and install the typed result.
    fn take_snapshot(&mut self, vm: VmId, op: u32, root: VmId, self_root: bool) {
        // A barrier identifier and a world gate both need one number
        // this world never repeats, and one monotone counter serves
        // both. The two live in different machine fields, so a shared
        // counter never confuses a barrier with a gate.
        let barrier = self.next_gate();
        let built = match self.capture_snapshot(barrier, root, self_root) {
            Ok(image) => {
                self.trust_image(&image);
                self.last_image = Some(image.clone());
                self.machines[vm as usize]
                    .alloc(Object::NativeSnapshot(image.bytes().clone()))
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
    fn build_snapshot_error(
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
                let list = self.machines[vm as usize].alloc(Object::List { items })?;
                // The list holds no root yet, so it stays host-rooted
                // while the kind string allocates.
                let list_ref = list.as_obj().expect("a list is a heap object");
                self.machines[vm as usize].vm.heap.push_host_root(list_ref);
                let text = self.machines[vm as usize].alloc(Object::Str(kind.clone()));
                self.machines[vm as usize].vm.heap.pop_host_root(list_ref);
                let text = text?;
                self.make_instance(vm, self.core.snapshot_resource_active, vec![list, text])
            }
            crate::snapshot::SnapshotFail::Fault(_, message) => {
                let text = self.machines[vm as usize].alloc(Object::Str(message.clone()))?;
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
    fn restore_snapshot(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let target = self.handle_vm(vm, args[0]);
        if target == vm || self.machines[target as usize].active > 0 {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
            return;
        }
        if self.machines[target as usize].vm.state != MachineState::Empty {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is loaded");
            return;
        }
        let bytes = {
            let r = args[1].as_obj().expect("verified snapshot value");
            match self.machines[vm as usize].vm.heap.get(r) {
                Object::NativeSnapshot(image) => image.clone(),
                _ => unreachable!("verified snapshot shape"),
            }
        };
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
        let image = match self.trusted_image(&hash) {
            Some(image) => image,
            None => match self.load_snapshot_bytes(&bytes) {
                Ok(image) => image.world_arc(),
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
        };
        let built = match self.restore_image(vm, target, &image) {
            Ok(root) => self.machines[vm as usize]
                .alloc(Object::NativeVm { vm: root })
                .and_then(|handle| self.make_instance(vm, self.core.result_ok, vec![handle])),
            Err(crate::snapshot::RestoreFail::LimitExceeded) => self
                .make_instance(vm, self.core.restore_limit_exceeded, vec![])
                .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error])),
        };
        self.reply_or_fault(vm, op, built);
    }

    /// Check that the holder still owns the execution of a machine.
    ///
    /// `Proc.Run` transfers ownership to the scheduler and leaves the
    /// original `Vm` handle dormant. Execution and inspection through
    /// it fault until `pause()` returns ownership (specification
    /// 18.2). Table edits stay legal, so revocation still works.
    fn expect_holder_owned(&mut self, vm: VmId, op: u32, target: VmId) -> bool {
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

    /// Check that a controlled machine accepts a continuation method.
    fn expect_asked(&mut self, vm: VmId, op: u32, target: VmId) -> bool {
        if target == vm || self.machines[target as usize].active > 0 {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
            return false;
        }
        if !self.expect_holder_owned(vm, op, target) {
            return false;
        }
        if self.machines[target as usize].vm.state != MachineState::Asked {
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

    // ------------------------------------------------------------
    // Procs, mailboxes, and the scheduler interface.
    // ------------------------------------------------------------

    /// Read one proc reference out of a handle value.
    fn handle_proc(&self, holder: VmId, value: Value) -> (VmId, u32) {
        let r = value.as_obj().expect("verified handle value");
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeHandle { proc, generation } => (*proc, *generation),
            _ => unreachable!("verified handle shape"),
        }
    }

    /// True when the reference names a live machine slot.
    fn proc_alive(&self, proc: VmId, generation: u32) -> bool {
        (proc as usize) < self.machines.len()
            && self.machines[proc as usize].generation == generation
    }

    /// True when the reference names a machine that can still accept
    /// or answer: it exists, its generation matches, and it has not
    /// reached a terminal result.
    fn proc_running(&self, proc: VmId, generation: u32) -> bool {
        self.proc_alive(proc, generation)
            && !matches!(
                self.machines[proc as usize].vm.state,
                MachineState::Done | MachineState::Faulted
            )
    }

    /// Allocate one frozen `Fault` value in `vm`.
    fn make_fault(&mut self, vm: VmId, code: FaultCode, message: &str) -> Result<Value, FaultCode> {
        self.machines[vm as usize].alloc(Object::NativeFault {
            code,
            message: message.to_string(),
            op: None,
        })
    }

    /// Install one built reply, or fault the caller when the build
    /// failed.
    fn reply_or_fault(&mut self, vm: VmId, op: u32, built: Result<Value, FaultCode>) {
        match built {
            Ok(value) => self.install_value_reply(vm, value),
            Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
        }
    }

    /// Block one machine on another machine of this world.
    fn block_machine(&mut self, vm: VmId, block: Block) {
        let m = &mut self.machines[vm as usize];
        m.vm.block = Some(block);
        m.vm.state = MachineState::Blocked;
        self.record(TraceEvent::Block { vm, block });
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
    fn boundary_copy(&mut self, src: VmId, dst: VmId, value: Value) -> Result<Value, FaultCode> {
        if src != dst {
            return self.transfer(src, dst, value);
        }
        let limits = self.machines[dst as usize].config.graph;
        // The heap roots are read before the heap is borrowed: a
        // collection during the copy needs them.
        let roots = self.machines[dst as usize].gc_roots(&[]);
        let heap = &mut self.machines[dst as usize].vm.heap;
        lm_graph::copy_within(heap, &roots, value, &limits)
    }

    /// Execute one proc operation of the machine `vm`.
    fn proc_exec(&mut self, vm: VmId, op: u32, args: Vec<Value>) {
        match op {
            lm_abi::OP_PROC_SPAWN => self.proc_spawn(vm, op, &args),
            lm_abi::OP_PROC_RUN => self.proc_run(vm, op, &args),
            lm_abi::OP_PROC_SEND => self.proc_send(vm, op, &args),
            lm_abi::OP_PROC_CLOSE => self.proc_close(vm, op, &args),
            lm_abi::OP_PROC_RECV => self.proc_recv(vm, op),
            lm_abi::OP_PROC_DONE => self.proc_done(vm, op, &args),
            lm_abi::OP_PROC_PAUSE => self.proc_pause(vm, op, &args),
            lm_abi::OP_PROC_RESUME => self.proc_resume(vm, op, &args),
            _ => unreachable!("every proc slot has a kernel rule"),
        }
    }

    /// `Class.spawn(args...)`: build one proc machine, grant it the
    /// `Proc` group, and transfer its execution to the scheduler.
    ///
    /// The arguments are the constructor closure, the proc body
    /// closure, and the argument tuple. The proc instance is
    /// constructed inside its own machine (specification 18.1).
    fn proc_spawn(&mut self, vm: VmId, op: u32, args: &[Value]) {
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
        self.machines
            .push(Machine::empty_at(child_config, Some(vm), 0));
        // The two closures and every argument cross the boundary. Each
        // result stays rooted while the next value crosses.
        let mut payload: Vec<Value> = vec![args[0], args[1]];
        if let Value::Obj(r) = args[2] {
            let items = match self.machines[vm as usize].vm.heap.get(r) {
                Object::Tuple { items } => items.clone(),
                _ => unreachable!("verified argument view shape"),
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
        let ctor = moved[0].as_obj().expect("a constructor is a closure");
        let body = moved[1].as_obj().expect("a proc body is a closure");
        let ctor_args: Vec<Value> = moved[2..].to_vec();
        let func = match self.machines[child as usize].vm.heap.get(ctor) {
            Object::Closure { func, .. } => *func,
            _ => unreachable!("a constructor is a closure"),
        };
        // The birth grant of specification 18.3. A mailbox-bearing
        // proc needs the `Proc` group to receive, and the spawner
        // already carries `Proc.Spawn`, so it may pass the group.
        let group = lm_abi::group_by_name("Proc").expect("the manifest declares the Proc group");
        let limit = self.machines[child as usize].config.mailbox_limit;
        {
            let m = &mut self.machines[child as usize];
            m.table.group[group as usize] = Some(Action::Pass);
            m.vm.mailbox = Mailbox::new(limit);
            m.start_body = Some(body);
            m.owner = Ownership::Scheduler;
        }
        self.machines[child as usize].load_frame(self.module, func, ctor_args, Some(ctor));
        let generation = self.machines[child as usize].generation;
        let built = self.machines[vm as usize].alloc(Object::NativeHandle {
            proc: child,
            generation,
        });
        self.record(TraceEvent::Spawn {
            parent: vm,
            proc: child,
            generation,
        });
        self.reply_or_fault(vm, op, built);
    }

    /// `sys.proc.run(vm)`: transfer one loaded machine to the
    /// scheduler. The launch carries no mailbox, so the handle takes
    /// the bottom message type.
    fn proc_run(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let target = self.handle_vm(vm, args[0]);
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
        self.machines[target as usize].owner = Ownership::Scheduler;
        let generation = self.machines[target as usize].generation;
        let built = self.machines[vm as usize].alloc(Object::NativeHandle {
            proc: target,
            generation,
        });
        self.record(TraceEvent::Spawn {
            parent: vm,
            proc: target,
            generation,
        });
        self.reply_or_fault(vm, op, built);
    }

    /// `h.send(message)`.
    ///
    /// The mailbox limit is checked before the copy, so a refused
    /// message never enters the target heap.
    fn proc_send(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let (proc, generation) = self.handle_proc(vm, args[0]);
        if !self.proc_running(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the target proc is dead")
                .and_then(|fault| self.make_instance(vm, self.core.send_fault, vec![fault]));
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
            let built = self.make_instance(vm, self.core.send_closed, vec![]);
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
            mailbox.queue.push_back(moved);
            mailbox.accepted += 1;
        }
        self.record(TraceEvent::Send {
            from: vm,
            to: proc,
            accepted: true,
        });
        let built = self.make_instance(vm, self.core.send_sent, vec![]);
        self.reply_or_fault(vm, op, built);
    }

    /// `h.close()`. A successful close returns `Sent`; a repeat
    /// returns `Closed` (specification 18.4).
    fn proc_close(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let (proc, generation) = self.handle_proc(vm, args[0]);
        if !self.proc_running(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the target proc is dead")
                .and_then(|fault| self.make_instance(vm, self.core.send_fault, vec![fault]));
            self.reply_or_fault(vm, op, built);
            return;
        }
        let first = !self.machines[proc as usize].vm.mailbox.closed;
        self.machines[proc as usize].vm.mailbox.closed = true;
        self.record(TraceEvent::Close { proc, first });
        let arm = if first {
            self.core.send_sent
        } else {
            self.core.send_closed
        };
        let built = self.make_instance(vm, arm, vec![]);
        self.reply_or_fault(vm, op, built);
    }

    /// `self.receive()` inside a proc.
    ///
    /// The host answers only for a scheduler-owned machine, so the
    /// rule of specification 18.5 fails closed everywhere else.
    fn proc_recv(&mut self, vm: VmId, op: u32) {
        if self.machines[vm as usize].owner != Ownership::Scheduler {
            self.fault_caller(
                vm,
                op,
                FaultCode::InvalidVmState,
                "receive is valid only on a scheduler-owned proc",
            );
            return;
        }
        let message = self.machines[vm as usize].vm.mailbox.queue.pop_front();
        match message {
            Some(value) => {
                self.machines[vm as usize].vm.mailbox.delivered += 1;
                self.record(TraceEvent::Receive {
                    proc: vm,
                    closed: false,
                });
                let built = self.make_instance(vm, self.core.recv_msg, vec![value]);
                self.reply_or_fault(vm, op, built);
            }
            None if self.machines[vm as usize].vm.mailbox.closed => {
                self.record(TraceEvent::Receive {
                    proc: vm,
                    closed: true,
                });
                let built = self.make_instance(vm, self.core.recv_closed, vec![]);
                self.reply_or_fault(vm, op, built);
            }
            // The mailbox is open and empty: wait for a message or a
            // close.
            None => self.block_machine(vm, Block::Receive),
        }
    }

    /// `h.done()`. The holder blocks until the proc is terminal.
    fn proc_done(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let (proc, generation) = self.handle_proc(vm, args[0]);
        if !self.proc_alive(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the proc reference is stale")
                .and_then(|fault| self.make_instance(vm, self.core.proc_fault, vec![fault]));
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

    /// Build and install `ProcResult` for one terminal proc.
    fn publish_terminal(&mut self, vm: VmId, op: u32, proc: VmId) {
        enum T {
            Done(Value),
            Fault(FaultRec),
        }
        let t = match &self.machines[proc as usize].vm.terminal {
            Some(Terminal::Done(value)) => T::Done(*value),
            Some(Terminal::Fault(rec)) => T::Fault(rec.clone()),
            None => unreachable!("a terminal machine stores its result"),
        };
        let built = match t {
            T::Done(value) => match self.transfer(proc, vm, value) {
                Ok(value) => self.make_instance(vm, self.core.proc_done, vec![value]),
                Err(code) => self
                    .make_fault(vm, code, "the terminal value did not cross the boundary")
                    .and_then(|fault| self.make_instance(vm, self.core.proc_fault, vec![fault])),
            },
            T::Fault(rec) => self.machines[vm as usize]
                .alloc(Object::NativeFault {
                    code: rec.code,
                    message: rec.message.clone(),
                    op: rec.op,
                })
                .and_then(|fault| self.make_instance(vm, self.core.proc_fault, vec![fault])),
        };
        self.reply_or_fault(vm, op, built);
    }

    /// `h.pause()`: take execution ownership back from the scheduler.
    fn proc_pause(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let (proc, generation) = self.handle_proc(vm, args[0]);
        let arm = if !self.proc_running(proc, generation) {
            self.core.proc_error_dead
        } else if self.machines[proc as usize].paused {
            self.core.proc_error_already_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core.proc_error_in_use
        } else {
            self.machines[proc as usize].owner = Ownership::Holder;
            self.machines[proc as usize].paused = true;
            self.record(TraceEvent::Pause { proc });
            let built = self.machines[vm as usize]
                .alloc(Object::NativeVm { vm: proc })
                .and_then(|handle| self.make_instance(vm, self.core.result_ok, vec![handle]));
            self.reply_or_fault(vm, op, built);
            return;
        };
        let built = self
            .make_instance(vm, arm, vec![])
            .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error]));
        self.reply_or_fault(vm, op, built);
    }

    /// `h.resume()`: give execution ownership back to the scheduler.
    fn proc_resume(&mut self, vm: VmId, op: u32, args: &[Value]) {
        let (proc, generation) = self.handle_proc(vm, args[0]);
        let arm = if !self.proc_running(proc, generation) {
            self.core.proc_error_dead
        } else if !self.machines[proc as usize].paused {
            self.core.proc_error_not_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core.proc_error_in_use
        } else {
            self.machines[proc as usize].owner = Ownership::Scheduler;
            self.machines[proc as usize].paused = false;
            self.record(TraceEvent::Resume { proc });
            let built = self.make_instance(vm, self.core.result_ok, vec![Value::Unit]);
            self.reply_or_fault(vm, op, built);
            return;
        };
        let built = self
            .make_instance(vm, arm, vec![])
            .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error]));
        self.reply_or_fault(vm, op, built);
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
        let target = match self.machines[vm as usize].vm.heap.get(table) {
            Object::NativeTable { vm } => *vm,
            _ => unreachable!("verified table handle shape"),
        };
        let entry = match action {
            0 => Some(Action::Pass),
            1 => Some(Action::Block),
            2 => {
                let closure = mock.expect("a mock edit carries its handler");
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
        let (rv, ordinal) = match self.machines[vm as usize].vm.heap.get(request) {
            Object::NativeRequest { vm, ordinal } => (*vm, *ordinal),
            _ => unreachable!("verified request shape"),
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
    /// `value.digest()` executed by `vm`.
    ///
    /// The digest mode requires a frozen graph and rejects a live
    /// holder-local value with `BoundaryViolation`. A frozen object
    /// never changes, so the heap caches the result.
    fn handle_digest(&mut self, vm: VmId, value: ObjRef) {
        // The machine that asks for the digest pays for the walk.
        let limits = self.machines[vm as usize].config.graph;
        let loaded = self.loaded;
        let built = match loaded.identity() {
            Ok(identity) => {
                let codes = ModuleCodes { identity };
                let heap = &mut self.machines[vm as usize].vm.heap;
                lm_graph::digest_value(heap, Value::Obj(value), &codes, &limits)
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

    fn handle_call_args(&mut self, vm: VmId, call: ObjRef) {
        let (cv, ordinal, op) = match self.machines[vm as usize].vm.heap.get(call) {
            Object::NativeCall { vm, ordinal, op } => (*vm, *ordinal, *op),
            _ => unreachable!("verified call shape"),
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
        let source_args: Vec<Value> = self.machines[cv as usize]
            .vm
            .pending
            .as_ref()
            .expect("an asked machine has a pending perform")
            .args
            .clone();
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

    /// True when the block of `vm` can complete now.
    fn block_ready(&self, vm: VmId) -> bool {
        let Some(block) = self.machines[vm as usize].vm.block else {
            return false;
        };
        match block {
            Block::Receive => {
                let mailbox = &self.machines[vm as usize].vm.mailbox;
                !mailbox.queue.is_empty() || mailbox.closed
            }
            Block::Send { target, generation } => {
                !self.proc_running(target, generation)
                    || self.machines[target as usize].vm.mailbox.closed
                    || self.machines[target as usize].vm.mailbox.accepts()
            }
            Block::Done { target, generation } => {
                !self.proc_alive(target, generation)
                    || matches!(
                        self.machines[target as usize].vm.state,
                        MachineState::Done | MachineState::Faulted
                    )
            }
        }
    }

    /// Complete every block that can complete now, in machine order.
    /// Return the number of machines the call released.
    pub fn poll_blocked(&mut self) -> usize {
        let ready: Vec<VmId> = (0..self.machines.len() as VmId)
            .filter(|vm| {
                // A machine behind a world gate makes no move at all,
                // not even a completed block. The first run, step, or
                // drive of the restored root opens the gate
                // (specification 17.5).
                self.machines[*vm as usize].gate == 0
                    && self.machines[*vm as usize].vm.state == MachineState::Blocked
                    && self.block_ready(*vm)
            })
            .collect();
        for vm in &ready {
            let op = self
                .pending_op(*vm)
                .expect("a blocked machine holds its pending perform");
            self.machines[*vm as usize].vm.block = None;
            self.machines[*vm as usize].vm.state = MachineState::Ready;
            self.record(TraceEvent::Unblock { vm: *vm });
            let args: Vec<Value> = self.machines[*vm as usize]
                .vm
                .pending
                .as_ref()
                .expect("a blocked machine holds its pending perform")
                .args
                .clone();
            self.proc_exec(*vm, op, args);
        }
        ready.len()
    }

    /// The scheduler-owned machines that can retire an instruction
    /// now, in ascending identifier order.
    pub fn runnable_procs(&self) -> Vec<VmId> {
        (0..self.machines.len() as VmId)
            .filter(|vm| {
                let m = &self.machines[*vm as usize];
                // A machine with a suspended stack holds the execution
                // references of that stack, so its own base activation
                // is the one that resumes it. Every other machine of
                // the stack stays out of the run set.
                let resumable = self.suspended.contains_key(vm);
                // A running machine is one that a suspended stack
                // left mid flight. Only its own base activation may
                // pick it up again.
                let state_ok = match m.vm.state {
                    MachineState::Ready | MachineState::Waiting => true,
                    MachineState::Running => resumable,
                    _ => false,
                };
                m.owner == Ownership::Scheduler
                    && (m.active == 0 || resumable)
                    && !m.paused
                    && m.barrier.is_none()
                    && m.gate == 0
                    && state_ok
            })
            .collect()
    }

    /// True when one machine still holds a suspended activation stack.
    pub fn is_suspended(&self, vm: VmId) -> bool {
        self.suspended.contains_key(&vm)
    }

    /// The depth of the suspended activation stack of one machine.
    ///
    /// Zero means the machine holds none. A depth of one is a machine
    /// that blocked on its own base activation, and the scheduler
    /// rebuilds that activation when the block clears.
    pub fn suspended_len(&self, vm: VmId) -> usize {
        self.suspended.get(&vm).map(Vec::len).unwrap_or(0)
    }

    /// Drop the suspended activation stack of one machine.
    ///
    /// A restored world holds no driver stack, so a snapshot that
    /// copied a blocked machine restores it with none. The scheduler
    /// builds a fresh activation when the block clears.
    pub fn drop_suspended(&mut self, vm: VmId) {
        self.suspended.remove(&vm);
    }

    /// Drive one scheduler-owned proc until it blocks, waits, or
    /// reaches a terminal result.
    pub fn drive_proc(&mut self, vm: VmId) -> ProcStop {
        debug_assert_eq!(self.machines[vm as usize].owner, Ownership::Scheduler);
        let event = if self.suspended.contains_key(&vm) {
            self.resume_stack(vm)
        } else {
            self.control(vm, StopMode::RunToTerminal, Family::Run)
        };
        match event {
            RootEvent::Blocked => ProcStop::Blocked,
            RootEvent::Waiting => ProcStop::Waiting,
            RootEvent::Done(_) | RootEvent::Fault(_) => {
                let faulted = self.machines[vm as usize].vm.state == MachineState::Faulted;
                self.record(TraceEvent::Terminal { proc: vm, faulted });
                ProcStop::Terminal
            }
            _ => unreachable!("a proc slice runs to a terminal, a block, or a wait"),
        }
    }

    /// Fault one blocked machine, for a scheduler that cannot make
    /// the block complete.
    pub fn fail_blocked(&mut self, vm: VmId, message: &str) {
        let op = self.pending_op(vm);
        self.machines[vm as usize].vm.block = None;
        self.machines[vm as usize].set_fault(FaultCode::HostFault, message, op);
        self.suspended.remove(&vm);
    }

    /// Every machine that is blocked on another machine, in ascending
    /// identifier order.
    pub fn blocked_machines(&self) -> Vec<VmId> {
        (0..self.machines.len() as VmId)
            .filter(|vm| self.machines[*vm as usize].vm.state == MachineState::Blocked)
            .collect()
    }

    /// The mailbox counters of one machine.
    pub fn mailbox_metrics(&self, vm: VmId) -> MailboxMetrics {
        let mailbox = &self.machines[vm as usize].vm.mailbox;
        MailboxMetrics {
            limit: mailbox.limit,
            queued: mailbox.queue.len() as u32,
            accepted: mailbox.accepted,
            delivered: mailbox.delivered,
            closed: mailbox.closed,
            frozen: mailbox.frozen,
        }
    }

    /// The execution owner of one machine.
    pub fn owner_of(&self, vm: VmId) -> Ownership {
        self.machines[vm as usize].owner
    }

    // ------------------------------------------------------------
    // The barrier interface.
    // ------------------------------------------------------------

    /// The barrier that holds one machine.
    pub fn barrier_of(&self, vm: VmId) -> Option<u32> {
        self.machines[vm as usize].barrier
    }

    /// Give one machine to a barrier, or give it back.
    ///
    /// The scheduler never drives a machine a barrier holds, so the
    /// machine stays at the instruction boundary the barrier found.
    pub fn set_barrier(&mut self, vm: VmId, barrier: Option<u32>) {
        self.machines[vm as usize].barrier = barrier;
    }

    /// Freeze or thaw mailbox acceptance of one machine.
    ///
    /// A frozen mailbox accepts no message. A send that reaches one
    /// blocks the sender, so the accepted queue at the cut is exactly
    /// what a snapshot would copy.
    pub fn freeze_mailbox(&mut self, vm: VmId, frozen: bool) {
        self.machines[vm as usize].vm.mailbox.frozen = frozen;
    }

    /// The next mailbox cut marker of this world.
    pub fn next_cut(&mut self) -> u64 {
        self.cut += 1;
        self.cut
    }

    /// The next world-gate marker of this world.
    pub fn next_gate(&mut self) -> u32 {
        self.gate += 1;
        self.gate
    }

    /// The world gate one machine sits behind, or zero.
    pub fn gate_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].gate
    }

    /// Open the world gate of one machine, and of every machine
    /// behind the same gate.
    ///
    /// The first `run`, `step`, or `drive` of a restored root calls
    /// it, so a restored world starts as one world, never as a set of
    /// procs that drift apart before the holder resumes them.
    pub fn open_gate(&mut self, vm: VmId) {
        let gate = self.machines[vm as usize].gate;
        if gate == 0 {
            return;
        }
        for machine in &mut self.machines {
            if machine.gate == gate {
                machine.gate = 0;
            }
        }
    }

    /// The number of whole-image structural checks this world ran.
    pub fn snapshot_checks(&self) -> u64 {
        self.checks
    }

    /// Record one whole-image structural check.
    pub(crate) fn record_snapshot_check(&mut self) {
        self.checks += 1;
    }

    /// Remember one image this world trusts.
    pub fn trust_image(&mut self, image: &crate::snapshot::SnapshotImage) {
        let hash = image.hash();
        if self.trusted.iter().any(|(h, _)| *h == hash) {
            return;
        }
        self.trusted.insert(0, (hash, image.world_arc()));
        self.trusted.truncate(TRUSTED_IMAGES);
    }

    /// The trusted image with this container hash.
    fn trusted_image(&self, hash: &[u8; 32]) -> Option<std::sync::Arc<crate::snapshot::Image>> {
        self.trusted
            .iter()
            .find(|(h, _)| h == hash)
            .map(|(_, image)| image.clone())
    }

    /// Install one external snapshot container into this world.
    ///
    /// This is the external byte path of specification 17.8. It runs
    /// the whole structural checklist once and remembers the checked
    /// image, so a later restore of the same bytes repeats nothing.
    /// The trusted in-process path is `capture_snapshot`, and the two
    /// never share an entry point.
    pub fn load_snapshot_bytes(
        &mut self,
        bytes: &[u8],
    ) -> Result<crate::snapshot::SnapshotImage, crate::snapshot::ImageError> {
        let limits = crate::snapshot::LoadLimits::default();
        self.record_snapshot_check();
        let image = crate::snapshot::codec::load_external(bytes, self.loaded, limits)?;
        self.trust_image(&image);
        Ok(image)
    }

    /// The number of machines a barrier may reach.
    pub fn machine_ids(&self) -> Vec<VmId> {
        (0..self.machines.len() as VmId).collect()
    }

    /// True when one machine holds a loaded or terminal state, so a
    /// barrier must stop it.
    pub fn is_live_machine(&self, vm: VmId) -> bool {
        self.machines[vm as usize].vm.state != MachineState::Empty
    }

    /// Every machine one machine names in its reachable state.
    ///
    /// Five native shapes name a machine: a machine handle, a proc
    /// handle, a policy-table handle, a request token, and a typed
    /// call token. The walk reports all five, so the barrier set
    /// closes over every machine reference a heap can hold.
    ///
    /// The walk starts at the snapshot roots, which cover the frame
    /// closures, the locals, the operands, the pending arguments, the
    /// terminal result, the accepted mailbox queue, the proc body, and
    /// the interned literals. It excludes the policy table, because
    /// specification 17.2 excludes policy tables from a snapshot. A
    /// machine that only a table-held mock closure names is therefore
    /// not part of the world.
    ///
    /// The result is in canonical object order, first encounter first.
    /// The machine ordinals of an image read that order, so they never
    /// depend on a scheduler identifier.
    pub fn machine_references(&mut self, vm: VmId) -> Result<Vec<VmId>, FaultCode> {
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let order = {
            let m = &mut self.machines[vm as usize];
            lm_graph::snapshot_ordinals(&mut m.vm.heap, &roots, &limits)?
        };
        let heap = &self.machines[vm as usize].vm.heap;
        let mut out: Vec<VmId> = Vec::new();
        for r in order {
            let target = match heap.get(r) {
                Object::NativeVm { vm }
                | Object::NativeTable { vm }
                | Object::NativeRequest { vm, .. }
                | Object::NativeCall { vm, .. } => Some(*vm),
                Object::NativeHandle { proc, .. } => Some(*proc),
                _ => None,
            };
            if let Some(target) = target {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        Ok(out)
    }

    /// The slot generation of one machine.
    pub fn generation_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].generation
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

    /// The number of live host resources one machine holds.
    pub fn resource_count(&self, vm: VmId) -> usize {
        self.machines[vm as usize].resources.live_count()
    }

    /// The number of child machines one machine reserved.
    pub fn child_count(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].children
    }

    /// Preflight one machine for a snapshot.
    ///
    /// The check reads the resource registry and the guest graph, as
    /// specification 25.5 requires. A live host attachment on either
    /// side blocks the copy. On success the call returns the number of
    /// objects the canonical snapshot traversal ordered.
    ///
    /// The walk reads the snapshot roots, so it covers exactly the
    /// objects the encoder writes.
    pub fn snapshot_preflight(&mut self, vm: VmId) -> Result<usize, FaultCode> {
        if self.machines[vm as usize]
            .resources
            .live_attachment()
            .is_some()
        {
            return Err(FaultCode::BoundaryViolation);
        }
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let m = &mut self.machines[vm as usize];
        lm_graph::snapshot_ordinals(&mut m.vm.heap, &roots, &limits).map(|order| order.len())
    }

    /// The kind name of one live host attachment this machine holds.
    pub fn live_attachment_kind(&self, vm: VmId) -> Option<String> {
        self.machines[vm as usize]
            .resources
            .live_attachment()
            .map(|record| match record.kind {
                crate::ResourceKind::PendingOperation => {
                    format!("a pending {}", lm_abi::op_name(record.op))
                }
            })
    }

    /// The number of live activation references to one machine.
    pub fn active_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].active
    }

    /// The verified semantic identity of the loaded program.
    pub fn identity(&self) -> Result<&lm_bytecode::identity::ModuleIdentity, FaultCode> {
        self.loaded.identity()
    }

    /// The loaded program.
    pub fn module(&self) -> &Module {
        self.module
    }

    /// The resource limits of one machine.
    pub fn config_of(&self, vm: VmId) -> VmConfig {
        self.machines[vm as usize].config
    }

    /// Transfer one value from `src` into `dst` through the graph
    /// engine.
    ///
    /// The transfer mode accepts scalars, first-class operation
    /// values, and deeply frozen graphs of every sendable shape. It
    /// preserves cycles and sharing. A holder-local shape or a
    /// mutable object faults `UnsendableValue`, and a graph past the
    /// published limits faults `BoundaryLimit`.
    /// Transfer several values from `src` into `dst`.
    ///
    /// Each result stays rooted in the destination while the next
    /// value crosses. A destination collection during a later copy
    /// frees every object its roots do not reach, and a copied value
    /// that no machine field holds yet is one of those.
    pub(crate) fn transfer_all(
        &mut self,
        src: VmId,
        dst: VmId,
        values: &[Value],
    ) -> Result<Vec<Value>, FaultCode> {
        let mut moved: Vec<Value> = Vec::with_capacity(values.len());
        let mut result = Ok(());
        for value in values {
            match self.transfer(src, dst, *value) {
                Ok(value) => {
                    if let Some(r) = value.as_obj() {
                        self.machines[dst as usize].vm.heap.push_host_root(r);
                    }
                    moved.push(value);
                }
                Err(code) => {
                    result = Err(code);
                    break;
                }
            }
        }
        // Unroot in LIFO order. The caller stores the results in a
        // machine field before the next allocation.
        for value in moved.iter().rev() {
            if let Some(r) = value.as_obj() {
                self.machines[dst as usize].vm.heap.pop_host_root(r);
            }
        }
        result?;
        Ok(moved)
    }

    pub(crate) fn transfer(
        &mut self,
        src: VmId,
        dst: VmId,
        value: Value,
    ) -> Result<Value, FaultCode> {
        // The copy allocates in the destination, so the destination
        // limits govern the walk.
        let limits = self.machines[dst as usize].config.graph;
        let (src_m, dst_m) = self.two(src, dst);
        // The destination roots are read before the heap is borrowed:
        // a destination collection during the copy needs them.
        let dst_roots = dst_m.gc_roots(&[]);
        lm_graph::transfer(
            &mut src_m.vm.heap,
            &mut dst_m.vm.heap,
            &dst_roots,
            value,
            &limits,
        )
    }
}

/// The message of one failed boundary copy.
///
/// A copy fails for two different reasons. The graph may hold a value
/// that never crosses, or the copy may pass a limit. A limit failure
/// is not a sendability failure, so the two texts differ. The stable
/// fault code does not change.
fn copy_failure(code: FaultCode, what: &str) -> String {
    match code {
        FaultCode::HeapLimit => format!("the {what} copy exceeded the heap limit"),
        FaultCode::BoundaryLimit => format!("the {what} copy exceeded the boundary limit"),
        _ => format!("the {what} is not sendable"),
    }
}

/// One resolution of a policy walk.
enum Resolution {
    Denied,
    /// The pass-through chain reached a parent machine that is gone.
    /// The request fails closed (specification 18.6).
    DeadParent,
    Mock {
        owner: VmId,
        closure: ObjRef,
    },
    Root,
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
        self.show_value_inner(&self.machines[vm as usize].vm.heap, value, 0, &mut visited)
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
                    Object::Map { entries, .. } => {
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
                    Object::NativeDigest(bytes) => render_digest(bytes),
                    Object::NativeHandle { proc, generation } => {
                        format!("<proc {proc}.{generation}>")
                    }
                    Object::NativeSnapshot(image) => {
                        format!("<snapshot {} bytes>", image.len())
                    }
                }
            }
        }
    }

    /// The report label of one function value: every name that binds
    /// it, or the code label when no name binds it.
    ///
    /// Two modules with equal bodies share one function value, so a
    /// single label would hide one of the two names. A closure body
    /// and the entry take no binding and keep their code label.
    fn func_label(&self, func: u32) -> String {
        let keys: Vec<&str> = self
            .module
            .bindings
            .iter()
            .filter(|b| b.func == func)
            .map(|b| b.key.as_str())
            .collect();
        if keys.is_empty() {
            self.module.funcs[func as usize].name.clone()
        } else {
            keys.join(", ")
        }
    }

    /// Render the live root-machine state: outcome, heap statistics,
    /// frame count, and every live object in slot order.
    pub fn dump_live(&self, outcome: &Outcome) -> String {
        use std::fmt::Write as _;
        let m = &self.machines[0];
        let mut out = String::new();
        let _ = writeln!(out, "outcome: {}", self.show_outcome(outcome));
        let s = m.vm.heap.stats();
        let _ = writeln!(
            out,
            "heap: live={} slots={} pages={} free={} used_bytes={} cap_bytes={} collections={}",
            s.live, s.slots, s.pages, s.free, s.used_bytes, s.cap_bytes, s.collections
        );
        let _ = writeln!(out, "frames: {} active", m.vm.frames.len());
        for frame in &m.vm.frames {
            let _ = writeln!(
                out,
                "  frame {} block {} ip {}",
                self.func_label(frame.func),
                frame.block,
                frame.ip
            );
        }
        let _ = writeln!(out, "objects:");
        m.vm.heap.for_each_live(|r, frozen, _object| {
            let state = if frozen { "frozen" } else { "mutable" };
            let mut visited = Vec::new();
            let object = m.vm.heap.get(r);
            let _ = writeln!(
                out,
                "  obj {} gen {} {} {} {}",
                r.slot,
                r.generation,
                object.shape().name,
                state,
                self.show_value_inner(&m.vm.heap, Value::Obj(r), 0, &mut visited)
            );
        });
        out
    }
}

/// Render one proc trace event as one stable line.
pub(crate) fn show_trace_event(event: &TraceEvent) -> String {
    match event {
        TraceEvent::Spawn {
            parent,
            proc,
            generation,
        } => format!("spawn parent {parent} proc {proc} gen {generation}"),
        TraceEvent::Send { from, to, accepted } => {
            format!("send from {from} to {to} accepted {accepted}")
        }
        TraceEvent::Receive { proc, closed } => format!("receive proc {proc} closed {closed}"),
        TraceEvent::Close { proc, first } => format!("close proc {proc} first {first}"),
        TraceEvent::Block { vm, block } => {
            let what = match block {
                Block::Receive => "receive".to_string(),
                Block::Send { target, .. } => format!("send target {target}"),
                Block::Done { target, .. } => format!("done target {target}"),
            };
            format!("block vm {vm} on {what}")
        }
        TraceEvent::Unblock { vm } => format!("unblock vm {vm}"),
        TraceEvent::Pause { proc } => format!("pause proc {proc}"),
        TraceEvent::Resume { proc } => format!("resume proc {proc}"),
        TraceEvent::Terminal { proc, faulted } => {
            format!("terminal proc {proc} faulted {faulted}")
        }
    }
}

/// Render one canonical graph digest as lower-case hexadecimal.
pub(crate) fn render_digest(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
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
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
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
        world.machines[0].vm.pending = Some(Pending {
            op,
            args,
            ordinal: 1,
        });
    }

    #[test]
    fn control_of_an_active_machine_faults_the_caller_only() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.vm.state = MachineState::Ready;
        child.active = 1;
        world.machines.push(child);
        arm_pending(&mut world, lm_abi::OP_VM_RUN, vec![], 1);
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_RUN);
        assert_eq!(world.machines[0].vm.state, MachineState::Faulted);
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => assert_eq!(rec.code, FaultCode::InvalidVmState),
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine did not change.
        assert_eq!(world.machines[1].vm.state, MachineState::Ready);
        assert_eq!(world.machines[1].active, 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn a_stale_token_faults_the_caller_and_keeps_the_request() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.vm.state = MachineState::Asked;
        child.vm.pending = Some(Pending {
            op: lm_abi::OP_CLOCK_NOW,
            args: vec![],
            ordinal: 7,
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
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine keeps its state and its request.
        assert_eq!(world.machines[1].vm.state, MachineState::Asked);
        let pending = world.machines[1]
            .vm
            .pending
            .as_ref()
            .expect("still pending");
        assert_eq!(pending.ordinal, 7);
    }

    #[test]
    fn a_cross_machine_token_faults_the_caller() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        for _ in 0..2 {
            let mut child = Machine::empty(VmConfig::default(), Some(0));
            child.vm.state = MachineState::Asked;
            child.vm.pending = Some(Pending {
                op: lm_abi::OP_CLOCK_NOW,
                args: vec![],
                ordinal: 1,
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
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        assert_eq!(world.machines[1].vm.state, MachineState::Asked);
        assert_eq!(world.machines[2].vm.state, MachineState::Asked);
    }

    /// A proc may hold its own handle, so a send can name the sending
    /// machine. The message stays in one heap, and it copies there.
    ///
    /// Without the one-heap copy the sender and the mailbox would
    /// share one mutable graph.
    #[test]
    fn a_self_send_copies_inside_one_heap() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        // A scalar carries no reference.
        assert_eq!(world.boundary_copy(0, 0, Value::Int(7)), Ok(Value::Int(7)));
        // A mutable graph copies, so the message is a second object.
        let mutable = world.machines[0]
            .alloc(Object::List {
                items: vec![Value::Int(1)],
            })
            .expect("the list allocates");
        let moved = world
            .boundary_copy(0, 0, mutable)
            .expect("a mutable message copies");
        assert_ne!(moved, mutable);
        let source = mutable.as_obj().expect("the source is an object");
        let copy = moved.as_obj().expect("the copy is an object");
        assert!(!world.machines[0].vm.heap.is_frozen(copy));
        // A later write through the source misses the copy.
        if let Object::List { items } = world.machines[0].vm.heap.get_mut(source) {
            items.push(Value::Int(2));
        }
        world.machines[0].vm.heap.recharge(source);
        match world.machines[0].vm.heap.get(copy) {
            Object::List { items } => assert_eq!(items, &vec![Value::Int(1)]),
            other => panic!("expected a list, got {other:?}"),
        }
    }

    /// The self send applies the shape rule, so it accepts exactly
    /// what a cross-heap copy accepts.
    ///
    /// A machine handle is born frozen and holder local. The frozen
    /// bit alone would let it into a mailbox, and a cross-heap send
    /// of the same value rejects it.
    #[test]
    fn a_self_send_rejects_a_holder_local_value() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        world
            .machines
            .push(Machine::empty(VmConfig::default(), None));
        let handle = world.machines[0]
            .alloc(Object::NativeVm { vm: 1 })
            .expect("the handle allocates");
        // The frozen bit is set, so only the shape rule can refuse it.
        let r = handle.as_obj().expect("a handle is a heap object");
        assert!(world.machines[0].vm.heap.is_frozen(r));
        assert_eq!(
            world.boundary_copy(0, 0, handle),
            Err(FaultCode::UnsendableValue)
        );
        // The cross-heap path gives the same answer.
        assert_eq!(
            world.boundary_copy(0, 1, handle),
            Err(FaultCode::UnsendableValue)
        );
        // A holder-local object inside a frozen container is refused
        // just as it is at the top of a message.
        let wrapper = world.machines[0]
            .alloc(Object::Tuple {
                items: vec![handle],
            })
            .expect("the tuple allocates");
        assert_eq!(
            world.boundary_copy(0, 0, wrapper),
            Err(FaultCode::UnsendableValue)
        );
        assert_eq!(
            world.boundary_copy(0, 1, wrapper),
            Err(FaultCode::UnsendableValue)
        );
    }

    /// A failed copy reports its cause. A limit failure must not read
    /// like a sendability failure.
    #[test]
    fn a_copy_failure_names_its_cause() {
        assert_eq!(
            copy_failure(FaultCode::UnsendableValue, "message"),
            "the message is not sendable"
        );
        assert_eq!(
            copy_failure(FaultCode::HeapLimit, "message"),
            "the message copy exceeded the heap limit"
        );
        assert_eq!(
            copy_failure(FaultCode::BoundaryLimit, "message"),
            "the message copy exceeded the boundary limit"
        );
    }

    /// A stale generation names a dead proc, never the machine that
    /// took the slot later.
    ///
    /// A proc slot is not reused today, so no guest program reaches
    /// this state. The rule is the defense that keeps it that way, so
    /// it is tested at the record level.
    #[test]
    fn a_stale_generation_names_a_dead_proc() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut proc = Machine::empty_at(VmConfig::default(), Some(0), 3);
        proc.vm.state = MachineState::Ready;
        proc.owner = crate::machine::Ownership::Scheduler;
        world.machines.push(proc);
        assert!(world.proc_alive(1, 3));
        assert!(world.proc_running(1, 3));
        // A reference minted before the slot moved on is stale.
        assert!(!world.proc_alive(1, 2));
        assert!(!world.proc_running(1, 2));
        // A reference past the table is stale as well, and the bound
        // check runs before the generation read.
        assert!(!world.proc_alive(9, 0));
    }

    /// A retired mock slot takes a new generation, so a reference to
    /// the retired record never names its replacement.
    #[test]
    fn a_retired_slot_takes_a_new_generation() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        world
            .machines
            .push(Machine::empty(VmConfig::default(), None));
        assert_eq!(world.generation_of(1), 0);
        world.retire_mock(1);
        assert_eq!(world.generation_of(1), 1);
        assert!(!world.proc_alive(1, 0));
        assert!(world.proc_alive(1, 1));
    }

    /// A mock start that fails returns its machine slot at once.
    ///
    /// The slot is taken before the handler and the arguments cross,
    /// so both failure paths must retire it. Without that, one failed
    /// mock start per perform grows the machine table without bound.
    #[test]
    fn a_failed_mock_start_returns_its_machine_slot() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        // A machine handle is holder-local, so it never crosses a
        // boundary. It stands in for any unsendable handler here.
        let handle = world.machines[0]
            .alloc(Object::NativeVm { vm: 0 })
            .expect("the handle allocates");
        let unsendable = handle.as_obj().expect("a handle is a heap object");
        let before = world.machine_count();
        let mut stack = Vec::new();
        for ordinal in 1..4 {
            world.machines[0].vm.terminal = None;
            world.machines[0].vm.state = MachineState::Ready;
            world.machines[0].vm.pending = Some(Pending {
                op: lm_abi::OP_CLOCK_NOW,
                args: vec![],
                ordinal,
            });
            world.start_mock(&mut stack, 0, 0, unsendable);
            match &world.machines[0].vm.terminal {
                Some(Terminal::Fault(rec)) => {
                    assert_eq!(rec.code, FaultCode::UnsendableValue);
                }
                other => panic!("expected a fault, got {other:?}"),
            }
            // One slot exists and waits on the free list. A later
            // failed start reuses it instead of adding another.
            assert_eq!(world.machine_count(), before + 1, "start {ordinal}");
            assert_eq!(world.mock_free.len(), 1, "start {ordinal}");
            assert!(stack.is_empty(), "start {ordinal}");
        }
    }
}
