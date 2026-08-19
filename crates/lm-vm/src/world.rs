//! The world: every machine, the one driver loop, policy resolution,
//! the boundary transfer, and the host completion channel.
//!
//! The driver executes nested machines through an explicit activation
//! stack. Machine records are data; the Rust stack never grows with
//! guest call depth or with nested VM depth. `run`, `step`, and
//! `drive` are stop modes of this one loop.

use crate::host::{
    CoreCtor, Host, HostArg, HostCompletion, HostOpenOptions, HostSeekFrom, HostStart, HostValue,
};
use crate::machine::{
    Action, Block, ExecOutcome, FaultRec, Machine, MachineState, Mailbox, Ownership, Pending,
    PolicyCursor, RoutedRequest, Terminal, VmId, WaitEntry, WaitSource, MAX_LIVE_WAITS,
};
use crate::schedule::{
    ActiveProcs, CompletionKey, ScheduleEvents, SliceExit, TaskKey, TaskStatus, WaitSetKey,
    WaitSourceKey, WakeKey,
};
use crate::{FaultCode, LoadedModule, Outcome, VmConfig, WorldLimits};
use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{BcClassKind, Module};
use lm_heap::{Heap, HeapBudget, Object};
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
    /// A bounded drive turn. It answers the same `DriveEvent` as
    /// `Drive`, wrapped in `Option`. `None` reports that the turn
    /// spent its instruction bound and the machine can run again.
    DriveFor,
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
    /// The guest instructions this activation may still retire.
    /// `None` means no bound. `Vm.DriveFor` sets it, and the bound
    /// covers every activation above this one.
    fuel: Option<u32>,
}

/// Why one scheduler task saved its activation stack.
#[derive(Debug, Clone, Copy)]
enum SuspendReason {
    Yielded,
    Blocked {
        machine: VmId,
        wake: WakeKey,
    },
    Waiting {
        machine: VmId,
        completion: CompletionKey,
    },
    Parked {
        machine: VmId,
        wait: WaitSetKey,
    },
}

/// One saved activation stack and its scheduler condition.
#[derive(Debug)]
struct SuspendedStack {
    activations: Vec<Activation>,
    reason: SuspendReason,
}

/// The machines held behind one restored execution gate.
#[derive(Debug)]
pub(crate) struct GateGroup {
    id: u32,
    members: Vec<VmId>,
}

/// Why one activation exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitKind {
    Terminal,
    Ran,
    Waiting,
    /// A bounded drive turn spent its instructions. The machine can
    /// run again, so the holder receives no event.
    Bounded,
}

/// How one policy dispatch handles a nested VM execution operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchMode {
    /// Continue nested execution in the current driver call.
    Continue,
    /// Record nested execution for the next control call.
    DeferNested,
}

/// One validated destination for a single operation reply.
///
/// The value lives on the Rust stack. It grants no authority beyond
/// the request and route that `reply_sink` already validated.
#[derive(Debug, Clone, Copy)]
struct ReplySink {
    surface: VmId,
    target: VmId,
    ordinal: u64,
    op: u32,
    cursor: PolicyCursor,
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

/// The verified semantic identity of the loaded code, for the
/// canonical digest.
///
/// A closure holds a numeric function slot and an instance holds a
/// numeric class slot. Both slots belong to this linked program only,
/// so the digest encoder reads the definition hash instead.
struct ModuleCodes<'m> {
    identity: &'m lm_bytecode::identity::ModuleIdentity,
}

/// The aggregate ledgers of one root VM and its spawned procs.
struct WorldBudget {
    limits: WorldLimits,
    heap: HeapBudget,
    resources: crate::resource::ResourceBudget,
    fuel: u64,
}

/// One successful restore reply held before restore commit.
struct PreparedRestoreReply {
    value: Value,
    handle: ObjRef,
    reply: ObjRef,
}

#[derive(Debug, Clone, Copy)]
enum ResourceBacking {
    Host(u64),
    Driver(VmId),
}

#[derive(Debug, Clone, Copy)]
struct BoundResource {
    owner: VmId,
    kind: crate::ResourceKind,
    backing: ResourceBacking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitLeaf {
    Receive,
    Drive { target: VmId },
}

#[derive(Debug)]
struct WaitLeafPath {
    leaf: WaitLeaf,
    /// False selects `Choice.First`. True selects `Choice.Second`.
    path: Vec<bool>,
}

impl WorldBudget {
    fn new(mut limits: WorldLimits) -> WorldBudget {
        limits.max_machines = limits.max_machines.max(1);
        WorldBudget {
            heap: HeapBudget::new(limits.max_heap_bytes, limits.max_heap_objects),
            resources: crate::resource::ResourceBudget::new(limits.max_resources),
            fuel: limits.fuel,
            limits,
        }
    }
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
    pub(crate) module: &'m Module,
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
    suspended: std::collections::BTreeMap<VmId, SuspendedStack>,
    /// Scheduler-owned procs that have not reached a terminal state.
    scheduler_procs: ActiveProcs,
    /// Batched task and wake changes for `lm-proc`.
    schedule_events: ScheduleEvents,
    /// Ready host replies that another task does not await.
    host_completions: std::collections::BTreeMap<CompletionKey, HostCompletion>,
    /// Active restored execution gates and their exact members.
    gate_groups: Vec<GateGroup>,
    /// The closed type table and the type environment table of this
    /// world (`docs/specs/sidecar/snapshot-image-admission.md` section 5.6).
    ///
    /// A frame, a closure, an instance, and a machine store one index
    /// into it. The table belongs to one world, so an untrusted
    /// restore never grows shared module state.
    pub(crate) envs: lm_bytecode::closed::TypeEnvs,
    host: Box<dyn Host>,
    /// Open external resources, keyed by unforgeable world identifiers.
    bound_resources: std::collections::BTreeMap<u64, BoundResource>,
    /// The next resource identifier. Zero marks a closed handle.
    next_resource: u64,
    config: VmConfig,
    /// Aggregate limits and current resource charges.
    budget: WorldBudget,
    /// True when each machine heap charges the aggregate ledger.
    ///
    /// One machine needs only its local counters. The world attaches
    /// the shared ledger before it creates another machine.
    heap_shared: bool,
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
    /// True after one restore committed a machine into this world.
    ///
    /// The boundary check of `docs/specs/sidecar/snapshot-image-admission.md`
    /// section 5.2 proves that a value carries the type its receiving
    /// code expects. Ordinary execution builds every value through
    /// verified code, and the verifier already proved those types, so
    /// the check answers a question that is already settled.
    ///
    /// A restore is the one path that states a value the verifier
    /// never saw. Until a restore commits, therefore, no value of this
    /// world can carry a type its code did not prove, and the check
    /// costs work without adding a rule.
    ///
    /// The flag names the whole world rather than one machine. A
    /// machine-level rule must follow the source of each value, and a
    /// value reaches a boundary from a heap, a mailbox, or a host
    /// reply, so the source needs its own record. The world flag needs
    /// none, and it holds for every program that restores nothing.
    restored_any: bool,
    /// The number of whole-image admissions this world ran.
    ///
    /// The count instruments the rule of specification 17.8: external
    /// bytes are admitted once, and a later restore repeats nothing.
    checks: u64,
    /// The admitted images this execution already holds, newest first.
    ///
    /// A guest holds a snapshot as container bytes. A restore looks
    /// the bytes up by container hash: a hit is an image this process
    /// captured or already admitted, so the restore reads the admitted
    /// state and repeats no check. A miss runs the external loader
    /// once. The table stores `SnapshotImage`, so the type system
    /// records the admission fact; a bare `Image` can never enter it.
    /// A byte budget bounds retained decoded graphs. It never rejects
    /// an image. An evicted image runs admission again at its next
    /// restore.
    trusted: Vec<([u8; 32], crate::snapshot::SnapshotImage, usize)>,
    /// The canonical byte size charged by the trusted image cache.
    trusted_bytes: usize,
    /// The last image a guest capture produced in this world.
    ///
    /// `lm snapshot save` writes it, so a program states in its own
    /// source which world a checkpoint holds.
    last_image: Option<crate::snapshot::SnapshotImage>,
    /// The reusable buffers of the boundary type check.
    ///
    /// One world runs one check at a time, so one buffer set serves
    /// every boundary. A scalar reply touches none of it.
    check: crate::typecheck::BoundaryScratch,
}

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
        kind: TraceBlock,
        target: VmId,
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

/// One compact block kind in a scheduler trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceBlock {
    Receive,
    Send,
    Done,
    Wait,
    Snapshot,
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
        World::new_with_limits(loaded, config, WorldLimits::default(), host)
    }

    /// Create a world with exact aggregate limits.
    pub fn new_with_limits(
        loaded: &'m LoadedModule,
        config: VmConfig,
        limits: WorldLimits,
        host: Box<dyn Host>,
    ) -> World<'m> {
        let module = loaded.module();
        let budget = WorldBudget::new(limits);
        let local_heap_is_aggregate = config.heap_bytes <= budget.limits.max_heap_bytes
            && config.heap_bytes / lm_heap::MIN_OBJECT_COST <= budget.limits.max_heap_objects;
        let mut root = if local_heap_is_aggregate {
            Machine::empty_with_resource_budget(config, None, 0, budget.resources.clone())
        } else {
            Machine::empty_with_budgets(
                config,
                None,
                0,
                budget.heap.clone(),
                budget.resources.clone(),
            )
        };
        // The entry function of a program takes no type argument, so
        // the root frame carries the empty environment.
        root.load_frame(
            module,
            module.entry,
            Vec::new(),
            None,
            lm_value::TypeEnvId::EMPTY,
        );
        World {
            loaded,
            module,
            dispatch: loaded.dispatch(),
            core: loaded.core_layout(),
            machines: vec![root],
            mock_free: Vec::new(),
            suspended: std::collections::BTreeMap::new(),
            scheduler_procs: ActiveProcs::new(1),
            schedule_events: ScheduleEvents::default(),
            host_completions: std::collections::BTreeMap::new(),
            gate_groups: Vec::new(),
            envs: lm_bytecode::closed::TypeEnvs::new(config.max_closed_types, config.max_type_envs),
            host,
            bound_resources: std::collections::BTreeMap::new(),
            next_resource: 1,
            config,
            budget,
            heap_shared: !local_heap_is_aggregate,
            trace: None,
            cut: 0,
            gate: 0,
            restored_any: false,
            checks: 0,
            trusted: Vec::new(),
            trusted_bytes: 0,
            last_image: None,
            check: crate::typecheck::BoundaryScratch::default(),
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
            if trace.len() < self.budget.limits.max_trace_events {
                trace.push(event);
            }
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

    /// The live heap bytes of all machines.
    pub fn world_heap_bytes(&self) -> usize {
        if self.heap_shared {
            self.budget.heap.used_bytes()
        } else {
            self.machines
                .iter()
                .map(|machine| machine.vm.heap.used_bytes())
                .sum()
        }
    }

    /// The live heap objects of all machines.
    pub fn world_heap_objects(&self) -> usize {
        if self.heap_shared {
            self.budget.heap.live_objects()
        } else {
            self.machines
                .iter()
                .map(|machine| machine.vm.heap.live_count())
                .sum()
        }
    }

    /// The live host resources of all machines.
    pub fn world_resource_count(&self) -> usize {
        self.budget.resources.used()
    }

    /// The instruction budget that remains for this world.
    pub fn world_fuel(&self) -> u64 {
        self.budget.fuel
    }

    /// The decoded image storage held by the trusted cache.
    pub fn trusted_image_bytes(&self) -> usize {
        self.trusted_bytes
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
                let key = TaskKey {
                    vm: 0,
                    generation: self.machines[0].generation,
                };
                self.fail_blocked_task(key, "no scheduler drives this world");
                match self.terminal_root_event(0) {
                    RootEvent::Fault(rec) => Outcome::Fault(rec.code),
                    _ => Outcome::Fault(FaultCode::HostFault),
                }
            }
            // `run` waits out completions inside the loop, so it
            // exits at a terminal alone. Any other exit is a state
            // this build does not run, and it reads as a fault.
            _ => Outcome::Fault(FaultCode::MalformedState),
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
        self.resume_stack_with_quantum(vm, None)
    }

    /// Resume one saved stack under an optional scheduler quantum.
    fn resume_stack_with_quantum(&mut self, vm: VmId, quantum: Option<u32>) -> RootEvent {
        let Some(saved) = self.suspended.remove(&vm) else {
            return self.fault_event(vm, "the machine holds no suspended stack");
        };
        let mut stack = saved.activations;
        if stack.is_empty() {
            return self.fault_event(vm, "the suspended stack holds no activation");
        }
        self.drive_stack(&mut stack, quantum)
    }

    /// Fault one machine and answer its terminal event.
    ///
    /// The call answers a driver entry that reached a state this build
    /// does not run. It stops the machine and leaves the world alive.
    fn fault_event(&mut self, vm: VmId, message: &str) -> RootEvent {
        self.machines[vm as usize].set_fault(FaultCode::MalformedState, message, None);
        self.terminal_root_event(vm)
    }

    /// Retire one root instruction with automatic policy.
    pub fn step_root(&mut self) -> RootEvent {
        self.control(0, StopMode::OneStep, Family::Step)
    }

    /// Wait for reachable procs to release live snapshot resources.
    ///
    /// `fuel` counts proc instructions. This call never runs the held
    /// root. It never waits for an unavailable host completion.
    pub fn snapshot_wait(
        &mut self,
        root: VmId,
        mut fuel: u64,
    ) -> Result<crate::snapshot::SnapshotImage, crate::snapshot::SnapshotFail> {
        let mut last_task = None;
        loop {
            let barrier = self.next_gate();
            let active = match self.capture_snapshot(barrier, root, false) {
                Ok(image) => return Ok(image),
                Err(fail @ crate::snapshot::SnapshotFail::ResourceActive { .. }) => fail,
                Err(fail) => return Err(fail),
            };
            if fuel == 0 {
                return Err(active);
            }
            let machines = self.controlled_machines(root).map_err(|code| {
                crate::snapshot::SnapshotFail::Fault(
                    code,
                    "the snapshot wait could not inspect the machine world".to_string(),
                )
            })?;

            let completed = self.poll_host_completion(|key| machines.contains(&key.machine.vm));
            if completed.is_some() {
                continue;
            }

            let ready: Vec<TaskKey> = self
                .scheduler_seeds(false)
                .into_iter()
                .filter_map(|(key, status)| {
                    (machines.contains(&key.vm) && status == TaskStatus::Ready).then_some(key)
                })
                .collect();
            let before = self.budget.fuel;
            if ready.is_empty() {
                // No proc of this world can move. Advance the held
                // machine itself, so a resource that the held machine
                // owns also reaches its release point.
                if !matches!(
                    self.machines[root as usize].vm.state,
                    MachineState::Ready | MachineState::Waiting
                ) {
                    return Err(active);
                }
                let _ = self.control(root, StopMode::OneStep, Family::Step);
                let retired = before.saturating_sub(self.budget.fuel);
                if retired == 0 {
                    let barrier = self.next_gate();
                    return self.capture_snapshot(barrier, root, false);
                }
                fuel = fuel.saturating_sub(retired);
                continue;
            }
            let next = last_task
                .and_then(|last| ready.iter().position(|key| *key > last))
                .unwrap_or(0);
            let key = ready[next];
            last_task = Some(key);

            let exit = self.drive_slice(key, 1);
            if exit == Some(SliceExit::Terminal) {
                self.retire_scheduler_task(key);
            }
            let retired = before.saturating_sub(self.budget.fuel);
            if retired == 0 {
                let barrier = self.next_gate();
                return self.capture_snapshot(barrier, root, false);
            }
            fuel = fuel.saturating_sub(retired);
        }
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
        if let Some(route) = self.machines[vm as usize].vm.routed {
            return match self.machines[route.target as usize].vm.pending.as_ref() {
                Some(pending) => RootEvent::Asked(pending.ordinal),
                None => self.fault_event(vm, "the routed machine holds no request"),
            };
        }
        if self.suspended.contains_key(&vm) {
            return self.resume_stack(vm);
        }
        match self.machines[vm as usize].vm.state {
            MachineState::Blocked => RootEvent::Blocked,
            MachineState::Asked => match self.machines[vm as usize].vm.pending.as_ref() {
                Some(pending) => RootEvent::Asked(pending.ordinal),
                None => self.fault_event(vm, "the asked machine holds no request"),
            },
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
        let machine = self.empty_machine(config, Some(parent), 0);
        self.machines.push(machine);
        Some(id)
    }

    /// The table grants that the declared row of one function names.
    ///
    /// A row entry is text: either one exact operation name or one
    /// group name. The launch paths pass exactly this row to a new
    /// machine, and each caller charges the same row, so a launch
    /// creates no authority.
    fn declared_grants(&self, func: u32) -> (Vec<u32>, Vec<u32>) {
        let mut ops: Vec<u32> = Vec::new();
        let mut groups: Vec<u32> = Vec::new();
        let Some(entry) = self.module.funcs.get(func as usize) else {
            return (ops, groups);
        };
        for elem in &entry.row {
            let lm_bytecode::BcRow::Op(idx) = elem else {
                continue;
            };
            let Some(text) = self.module.strings.get(*idx as usize) else {
                continue;
            };
            if let Some(op) = lm_abi::op_by_name(text) {
                ops.push(op);
            } else if let Some(group) = lm_abi::group_by_name(text) {
                groups.push(group);
            }
        }
        (ops, groups)
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
                // The callers above answer `Empty`, `Asked`, and
                // `Blocked` before this call, and a machine that is
                // `Running` holds an execution reference. A state that
                // reaches here belongs to no driver entry, so the
                // machine stops instead of running under a wrong mode.
                _ => return self.fault_event(vm, "the machine is not ready to run"),
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
                fuel: None,
            },
        );
        self.drive_stack(&mut stack, None)
    }

    /// The stored terminal event of one machine.
    ///
    /// A terminal machine stores its result: `set_done` and
    /// `set_fault` are the one path into either state, and admission
    /// keeps the rule for a restored machine. A machine that reaches
    /// this call without one stops as a fault instead of a panic.
    fn terminal_root_event(&self, vm: VmId) -> RootEvent {
        match &self.machines[vm as usize].vm.terminal {
            Some(Terminal::Done(value)) => RootEvent::Done(*value),
            Some(Terminal::Fault(rec)) => RootEvent::Fault(rec.clone()),
            None => RootEvent::Fault(FaultRec {
                code: FaultCode::MalformedState,
                message: "the terminal machine stores no result".to_string(),
                op: None,
            }),
        }
    }

    fn push_activation(&mut self, stack: &mut Vec<Activation>, act: Activation) {
        // A restored world runs behind one gate until its root moves.
        self.open_gate(act.vm);
        self.machines[act.vm as usize].active += 1;
        if act.mode == StopMode::DriveToAsk {
            debug_assert!(!self.machines[act.vm as usize].driven);
            self.machines[act.vm as usize].driven = true;
        }
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

    /// Leave the running state of one stack that stops now.
    ///
    /// A stored stack executes nothing, so its machines are at a
    /// boundary. They keep their execution references, so no control
    /// call reaches them, and a barrier can copy them. The driver loop
    /// makes each one running again when the stack resumes.
    fn park_stack(&mut self, stack: &[Activation]) {
        for act in stack {
            let machine = &mut self.machines[act.vm as usize];
            if machine.vm.state == MachineState::Running {
                machine.vm.state = MachineState::Ready;
            }
        }
    }

    /// Release the execution references of one removed activation.
    fn release_activation(&mut self, act: Activation) {
        self.machines[act.vm as usize].active -= 1;
        if act.mode == StopMode::DriveToAsk {
            self.machines[act.vm as usize].driven = false;
        }
        if let Some(parent) = act.reply_to {
            self.machines[parent as usize].active -= 1;
        }
        let machine = &mut self.machines[act.vm as usize];
        if machine.vm.state == MachineState::Running {
            machine.vm.state = MachineState::Ready;
        }
    }

    /// The one driver loop over the activation stack.
    fn drive_stack(&mut self, stack: &mut Vec<Activation>, mut quantum: Option<u32>) -> RootEvent {
        loop {
            let Some(top_idx) = stack.len().checked_sub(1) else {
                return RootEvent::Ran;
            };
            let act = stack[top_idx];
            // A descendant of this driven surface parked a request
            // while this stack waited on another task. Deliver it
            // before the loop reads the surface state, because the
            // surface can still be blocked on its own work.
            if act.mode == StopMode::DriveToAsk
                && self.machines[act.vm as usize].vm.routed.is_some()
            {
                if let Some(event) = self.deliver_parked_route(stack, top_idx) {
                    return event;
                }
                continue;
            }
            // A bounded drive turn spent its instructions. Unwind to
            // that activation and tell its holder that the machine can
            // run again.
            if let Some(at) = stack.iter().position(|a| a.fuel == Some(0)) {
                while stack.len() > at + 1 {
                    let popped = stack.pop().expect("the activation index is in the stack");
                    self.release_activation(popped);
                }
                if let Some(event) = self.finish(stack, ExitKind::Bounded) {
                    return event;
                }
                continue;
            }
            let state = self.machines[act.vm as usize].vm.state;
            match state {
                MachineState::Blocked => {
                    if let Some(Block::Wait { token }) = self.machines[act.vm as usize].vm.block {
                        if self.complete_ready_wait(act.vm, token) {
                            continue;
                        }
                        let wait = WaitSetKey {
                            owner: TaskKey {
                                vm: act.vm,
                                generation: self.machines[act.vm as usize].generation,
                            },
                            token,
                        };
                        let base = stack[0].vm;
                        self.suspended.insert(
                            base,
                            SuspendedStack {
                                activations: std::mem::take(stack),
                                reason: SuspendReason::Parked {
                                    machine: act.vm,
                                    wait,
                                },
                            },
                        );
                        return RootEvent::Blocked;
                    }
                    if self.block_ready(act.vm) {
                        self.complete_blocked_machine(act.vm);
                        continue;
                    }
                    let Some(wake) = self.block_wake_key(act.vm) else {
                        self.machines[act.vm as usize].set_fault(
                            FaultCode::MalformedState,
                            "the blocked machine has no wake condition",
                            None,
                        );
                        continue;
                    };
                    // The whole stack stops. Every activation keeps
                    // its execution reference, so no control call can
                    // reach a machine of the stopped stack.
                    let base = stack[0].vm;
                    self.park_stack(stack);
                    self.suspended.insert(
                        base,
                        SuspendedStack {
                            activations: std::mem::take(stack),
                            reason: SuspendReason::Blocked {
                                machine: act.vm,
                                wake,
                            },
                        },
                    );
                    return RootEvent::Blocked;
                }
                MachineState::Done | MachineState::Faulted => {
                    // This machine reached its terminal state inside
                    // another task's slice, so no slice exit reports
                    // it. Wake the tasks that wait on this machine
                    // here, or a `done` or a full-mailbox `send` on it
                    // never becomes runnable again.
                    self.note_terminal(act.vm);
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
                        let Some(completion) = self.completion_key(act.vm) else {
                            self.machines[act.vm as usize].set_fault(
                                FaultCode::MalformedState,
                                "the waiting machine has no completion key",
                                None,
                            );
                            continue;
                        };
                        let _ = self.poll_host_completion(|key| key == completion);
                        if self.machines[act.vm as usize].vm.state == MachineState::Waiting {
                            if let Some(event) = self.finish(stack, ExitKind::Waiting) {
                                return event;
                            }
                        }
                    } else if quantum.is_some() {
                        let Some(completion) = self.completion_key(act.vm) else {
                            self.machines[act.vm as usize].set_fault(
                                FaultCode::MalformedState,
                                "the waiting machine has no completion key",
                                None,
                            );
                            continue;
                        };
                        let base = stack[0].vm;
                        self.park_stack(stack);
                        self.suspended.insert(
                            base,
                            SuspendedStack {
                                activations: std::mem::take(stack),
                                reason: SuspendReason::Waiting {
                                    machine: act.vm,
                                    completion,
                                },
                            },
                        );
                        return RootEvent::Waiting;
                    } else {
                        let Some(completion) = self.completion_key(act.vm) else {
                            self.machines[act.vm as usize].set_fault(
                                FaultCode::MalformedState,
                                "the waiting machine has no completion key",
                                None,
                            );
                            continue;
                        };
                        if self.wait_host_completion(|key| key == completion).is_none() {
                            self.machines[act.vm as usize].set_fault(
                                FaultCode::HostFault,
                                "the host returned no pending completion",
                                None,
                            );
                        }
                    }
                }
                // A machine on the driver stack holds an execution
                // reference, and `push_activation` takes one from a
                // machine that is ready or waiting alone. Neither
                // state below can reach the loop, so a machine that
                // holds one stops instead of running under a mode its
                // state does not accept.
                // A machine parks at `Asked` when it surfaces a request
                // to a driver on another task. Its own activation ends
                // here, and the driver answer makes it ready again.
                MachineState::Asked => {
                    if let Some(event) = self.finish(stack, ExitKind::Ran) {
                        return event;
                    }
                }
                MachineState::Empty => {
                    self.machines[act.vm as usize].set_fault(
                        FaultCode::MalformedState,
                        "the machine left the driver stack state set",
                        None,
                    );
                }
                MachineState::Ready => {
                    self.machines[act.vm as usize].vm.state = MachineState::Running;
                }
                MachineState::Running => {
                    if self.machines[act.vm as usize].vm.nested.is_some() {
                        self.resume_nested(stack, act.vm);
                        continue;
                    }
                    if act.mode == StopMode::OneStep && act.retired {
                        if let Some(event) = self.finish(stack, ExitKind::Ran) {
                            return event;
                        }
                        continue;
                    }
                    if matches!(quantum, Some(0)) {
                        // A base activation keeps its continuation in
                        // machine state. Release its driver record at
                        // this scheduler safepoint.
                        if stack.len() == 1 {
                            if let Some(event) = self.finish(stack, ExitKind::Ran) {
                                return event;
                            }
                            continue;
                        }
                        let base = stack[0].vm;
                        self.park_stack(stack);
                        self.suspended.insert(
                            base,
                            SuspendedStack {
                                activations: std::mem::take(stack),
                                reason: SuspendReason::Yielded,
                            },
                        );
                        return RootEvent::Ran;
                    }
                    if self.budget.fuel == 0 {
                        self.machines[act.vm as usize].set_fault(FaultCode::OutOfFuel, "", None);
                        continue;
                    }
                    let requested = match quantum {
                        Some(_) if act.mode == StopMode::OneStep => 1,
                        Some(remaining) => remaining,
                        None if act.mode == StopMode::OneStep => 1,
                        None => u32::MAX,
                    };
                    let available = self.budget.fuel.min(u64::from(u32::MAX)) as u32;
                    // A bounded drive turn caps this batch, so the turn
                    // never retires past the bound its holder named.
                    let turn = stack
                        .iter()
                        .filter_map(|a| a.fuel)
                        .min()
                        .unwrap_or(u32::MAX)
                        .max(1);
                    let limit = requested.min(available).min(turn);
                    let module = self.module;
                    let dispatch = self.dispatch;
                    let envs = &mut self.envs;
                    let machine = &mut self.machines[act.vm as usize];
                    let (outcome, retired) =
                        machine.exec_for_quantum(module, dispatch, envs, limit);
                    self.budget.fuel -= u64::from(retired);
                    if let Some(remaining) = &mut quantum {
                        *remaining = remaining.saturating_sub(retired);
                    }
                    stack[top_idx].retired |= retired > 0;
                    for a in stack.iter_mut() {
                        if let Some(left) = &mut a.fuel {
                            *left = left.saturating_sub(retired);
                        }
                    }
                    match outcome {
                        Err(code) => {
                            self.machines[act.vm as usize].set_fault(code, "", None);
                        }
                        Ok(None) | Ok(Some(ExecOutcome::Continue)) => {}
                        Ok(Some(ExecOutcome::Terminal(value))) => {
                            // A launched proc runs two frames: the
                            // constructor, then the proc body over the
                            // constructed instance.
                            if self.machines[act.vm as usize].start_body.is_some() {
                                self.enter_proc_body(act.vm, value);
                            } else {
                                self.machines[act.vm as usize].set_done(value);
                            }
                        }
                        Ok(Some(ExecOutcome::Perform { op, args })) => {
                            if let Some(event) = self.handle_perform(stack, act.vm, op, args) {
                                return event;
                            }
                        }
                        Ok(Some(ExecOutcome::TableEdit {
                            table,
                            action,
                            kind,
                            slot,
                            mock,
                        })) => self.handle_table_edit(act.vm, table, action, kind, slot, mock),
                        Ok(Some(ExecOutcome::AsCall { request, op })) => {
                            self.handle_as_call(act.vm, request, op)
                        }
                        Ok(Some(ExecOutcome::RequestOp { request })) => {
                            self.handle_request_op(act.vm, request)
                        }
                        Ok(Some(ExecOutcome::CallArgs { call })) => {
                            self.handle_call_args(act.vm, call)
                        }
                        Ok(Some(ExecOutcome::Digest { value })) => {
                            self.handle_digest(act.vm, value)
                        }
                    }
                }
            }
        }
    }

    /// Push the child of one reified nested VM control operation.
    fn resume_nested(&mut self, stack: &mut Vec<Activation>, parent: VmId) {
        let Some(target) = self.machines[parent as usize].vm.nested else {
            return;
        };
        let Some(op) = self.machines[parent as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| pending.op)
        else {
            self.machines[parent as usize].set_fault(
                FaultCode::MalformedState,
                "the nested control edge has no pending operation",
                None,
            );
            return;
        };
        let (mode, family) = match op {
            lm_abi::OP_VM_RUN => (StopMode::RunToTerminal, Family::Run),
            lm_abi::OP_VM_STEP => (StopMode::OneStep, Family::Step),
            lm_abi::OP_VM_DRIVE => (StopMode::DriveToAsk, Family::Drive),
            _ => {
                self.machines[parent as usize].set_fault(
                    FaultCode::MalformedState,
                    "a deferred bounded drive has no instruction bound",
                    Some(op),
                );
                return;
            }
        };
        if target == parent || self.machines[target as usize].active > 0 {
            self.machines[parent as usize].set_fault(
                FaultCode::InvalidVmState,
                "the nested machine is in use",
                Some(op),
            );
            return;
        }
        if self.machines[target as usize].owner != Ownership::Holder {
            self.machines[parent as usize].set_fault(
                FaultCode::InvalidVmState,
                "the nested machine belongs to the scheduler",
                Some(op),
            );
            return;
        }
        self.push_activation(
            stack,
            Activation {
                vm: target,
                mode,
                family,
                reply_to: Some(parent),
                retired: false,
                fuel: None,
            },
        );
    }

    /// Wake the tasks that wait on one terminal machine.
    ///
    /// The scheduler reports a terminal slice exit for the task it
    /// drove. A machine that reaches its terminal state inside another
    /// task's slice produces no such exit, so the world reports it.
    /// A repeated call is safe: one event batch coalesces equal keys,
    /// and a wake only re-reads live task state.
    fn note_terminal(&mut self, vm: VmId) {
        let Some(key) = self.task_key(vm) else {
            return;
        };
        self.emit_wake(WakeKey::Done(key));
        self.emit_wake(WakeKey::Send(key));
    }

    /// The completion key of one waiting machine.
    fn completion_key(&self, vm: VmId) -> Option<CompletionKey> {
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
            if accepts(completion.key) {
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
            if accepts(completion.key) {
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
    fn take_host_completion(
        &mut self,
        accepts: &mut impl FnMut(CompletionKey) -> bool,
    ) -> Option<HostCompletion> {
        let key = self
            .host_completions
            .keys()
            .copied()
            .find(|key| accepts(*key))?;
        self.host_completions.remove(&key)
    }

    /// Remove replies for requests that no longer wait.
    fn prune_host_completions(&mut self) {
        let machines = &self.machines;
        self.host_completions.retain(|key, _| {
            machines
                .get(key.machine.vm as usize)
                .is_some_and(|machine| {
                    machine.generation == key.machine.generation
                        && machine.vm.state == MachineState::Waiting
                        && machine
                            .vm
                            .pending
                            .as_ref()
                            .is_some_and(|pending| pending.ordinal == key.ordinal)
                })
        });
    }

    /// True when one completion still names its waiting request.
    fn completion_is_current(&self, key: CompletionKey) -> bool {
        self.machines
            .get(key.machine.vm as usize)
            .is_some_and(|machine| {
                machine.generation == key.machine.generation
                    && machine.vm.state == MachineState::Waiting
                    && machine
                        .vm
                        .pending
                        .as_ref()
                        .is_some_and(|pending| pending.ordinal == key.ordinal)
            })
    }

    /// Install one host completion when its machine still waits.
    fn install_host_completion(&mut self, completion: HostCompletion) -> Option<CompletionKey> {
        let key = completion.key;
        if !self.completion_is_current(key) {
            return None;
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
    fn finish(&mut self, stack: &mut Vec<Activation>, kind: ExitKind) -> Option<RootEvent> {
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
    fn deliver_event(&mut self, act: Activation, parent: VmId, kind: ExitKind) {
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
            ExitKind::Ran => self.make_instance(parent, self.core.step_ran, vec![]),
            ExitKind::Waiting => self.make_instance(parent, self.core.step_waiting, vec![]),
            ExitKind::Bounded => self.make_instance(parent, self.core.option_none, vec![]),
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
    fn retire_mock(&mut self, mock: VmId) {
        debug_assert!(self.machines[mock as usize].active == 0);
        // The slot takes a new generation, so a reference minted for
        // the retired record names a dead machine, never the next one.
        let generation = self.machines[mock as usize].generation.wrapping_add(1);
        self.machines[mock as usize] = self.empty_machine(self.config, None, generation);
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
            None => T::Fault(FaultRec {
                code: FaultCode::MalformedState,
                message: "the terminal machine stores no result".to_string(),
                op: None,
            }),
        };
        let built = match t {
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
        };
        self.wrap_turn(parent, family, built)
    }

    /// Wrap one drive event for a bounded turn.
    ///
    /// `Vm.DriveFor` answers `Option[DriveEvent[T]]`, so an event of a
    /// bounded turn arrives as `Some`.
    fn wrap_turn(
        &mut self,
        parent: VmId,
        family: Family,
        built: Result<Value, FaultCode>,
    ) -> Result<Value, FaultCode> {
        if family != Family::DriveFor {
            return built;
        }
        let some = self.core.option_some;
        built.and_then(|value| self.make_instance(parent, some, vec![value]))
    }

    /// The `Done` arm of one event family.
    ///
    /// `deliver_event` answers a mock exit before it reads an arm, so
    /// the mock family reaches neither call. `None` here becomes a
    /// machine fault at `make_instance`.
    fn done_arm(&self, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core.run_done,
            Family::Step => self.core.step_done,
            Family::Drive | Family::DriveFor => self.core.drive_done,
            Family::Mock => None,
        }
    }

    fn fault_arm(&self, family: Family) -> Option<u32> {
        match family {
            Family::Run => self.core.run_fault,
            Family::Step => self.core.step_fault,
            Family::Drive | Family::DriveFor => self.core.drive_fault,
            Family::Mock => None,
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
    /// without every arm. The arm slot is therefore present. A module
    /// that reaches this call without one faults the machine.
    fn make_instance(
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
            fields,
            env: lm_value::Witness::EMPTY,
        })
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
    fn check_reply(&mut self, vm: VmId, value: Value) -> Result<(), FaultCode> {
        // Every value of a world that restored nothing came out of
        // verified code, so the check states a rule the verifier
        // already proved. The field doc of `restored_any` carries the
        // argument.
        if !self.restored_any {
            return Ok(());
        }
        let module = self.module;
        let machine = &self.machines[vm as usize];
        let frame = machine.vm.frames.last().ok_or(FaultCode::MalformedState)?;
        // The perform moved the counter past its own instruction, so
        // the instruction before the counter is that perform.
        let at = frame.ip.checked_sub(1).ok_or(FaultCode::MalformedState)?;
        let instr = module
            .funcs
            .get(frame.func as usize)
            .and_then(|code| code.blocks.get(frame.block as usize))
            .and_then(|block| block.get(at as usize))
            .ok_or(FaultCode::MalformedState)?;
        let reply_ty = match instr {
            lm_bytecode::Instr::Perform { reply_ty, .. }
            | lm_bytecode::Instr::PerformValue { reply_ty, .. } => *reply_ty,
            _ => return Err(FaultCode::MalformedState),
        };
        let env = frame.env;
        crate::typecheck::check_boundary_value(
            module,
            &machine.vm.heap,
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
    /// A spawn and a `Vm.FromFn` both copy values into another
    /// machine and load them as the first local slots of a frame. The
    /// declared parameter types come from verified code, and the
    /// closure states the environment its creator frame held.
    fn check_frame_args(
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
        let module = self.module;
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
                module,
                &machine.vm.heap,
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
    fn install_value_reply(&mut self, vm: VmId, value: Value) {
        self.install_value_reply_with_file_close(vm, value, true);
    }

    /// Install one reply and apply a successful file close.
    fn install_value_reply_with_file_close(&mut self, vm: VmId, value: Value, close_host: bool) {
        let closing = match self.pending_op(vm) {
            Some(lm_abi::OP_FS_CLOSE) if self.value_is_result_ok(vm, value) => {
                self.pending_file_resource(vm)
            }
            Some(lm_abi::OP_TCP_CLOSE) if self.value_is_result_ok(vm, value) => {
                self.pending_tcp_resource(vm)
            }
            _ => None,
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
        let m = &mut self.machines[vm as usize];
        // A completed request closes the host attachment it opened.
        if let Some(pending) = &m.vm.pending {
            let ordinal = pending.ordinal;
            m.resources.close_by_ordinal(ordinal);
        }
        m.vm.pending = None;
        if let Err(code) = m.push(value) {
            m.set_fault(code, "", None);
        } else if m.vm.state != MachineState::Running {
            m.vm.state = MachineState::Ready;
        }
        if let Some(resource) = closing {
            self.retire_resource(resource, close_host);
        }
        // A machine that parked at `Asked` for its driver leaves the
        // run set. This reply makes it runnable again, so the scheduler
        // needs the event.
        self.notify_task_state(vm);
    }

    /// Report the current schedulability of one machine.
    fn notify_task_state(&mut self, vm: VmId) {
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
    fn install_host_reply(&mut self, vm: VmId, reply: HostValue) {
        match self.build_host_value(vm, &reply) {
            Ok(value) => self.install_value_reply_with_file_close(vm, value, false),
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
        if self.machines[vm as usize].vm.state == MachineState::Faulted {
            self.close_resources_for_machine(vm);
        }
    }

    fn build_host_value(&mut self, vm: VmId, value: &HostValue) -> Result<Value, FaultCode> {
        match value {
            HostValue::Unit => Ok(Value::Unit),
            HostValue::Int(v) => Ok(Value::Int(*v)),
            HostValue::Str(s) => self.machines[vm as usize].alloc(Object::Str(s.clone())),
            HostValue::Bytes(bytes) => {
                self.machines[vm as usize].alloc(Object::Bytes(bytes.clone()))
            }
            HostValue::File(token) => self.build_host_file(vm, *token),
            HostValue::List(values) => {
                let mut items = Vec::with_capacity(values.len());
                for value in values {
                    items.push(self.build_host_value(vm, value)?);
                }
                self.machines[vm as usize].alloc(Object::List { items })
            }
            HostValue::SocketAddress(address) => self.build_host_address(vm, *address),
            HostValue::TcpStream(token) => {
                self.build_host_tcp(vm, *token, crate::ResourceKind::TcpStream)
            }
            HostValue::TcpListener(token) => {
                self.build_host_tcp(vm, *token, crate::ResourceKind::TcpListener)
            }
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
                    CoreCtor::FsErrorClosed => self.core.fs_error_closed,
                    CoreCtor::FsErrorFailed => self.core.fs_error_failed,
                    CoreCtor::Pair => self.core.pair,
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
                };
                self.make_instance(vm, class, fields)
            }
        }
    }

    /// Build one portable socket address inside a machine.
    fn build_host_address(
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
    fn build_host_tcp(
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

    /// Register one host file and build its guest handle.
    fn build_host_file(&mut self, vm: VmId, token: u64) -> Result<Value, FaultCode> {
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

    /// Remove one file resource. Close its host token when requested.
    fn retire_file(&mut self, resource: u64, close_host: bool) -> bool {
        self.retire_resource(resource, close_host)
    }

    /// Remove one bound resource. Close its host token when requested.
    fn retire_resource(&mut self, resource: u64, close_host: bool) -> bool {
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
                    crate::ResourceKind::PendingOperation => {}
                },
                ResourceBacking::Driver(_) => {}
            }
        }
        if let Some(machine) = self.machines.get_mut(bound.owner as usize) {
            machine.resources.close_kind(bound.kind, resource);
        }
        true
    }

    /// Close every external resource owned or serviced by one machine.
    fn close_resources_for_machine(&mut self, machine: VmId) {
        let pending: Vec<u64> = self.machines[machine as usize]
            .resources
            .pending_scopes()
            .collect();
        for token in pending {
            self.host.cancel(token);
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
    fn pending_file_resource(&self, vm: VmId) -> Option<u64> {
        let machine = self.machines.get(vm as usize)?;
        let value = *machine.vm.pending.as_ref()?.args.first()?;
        let reference = value.as_obj()?;
        match machine.vm.heap.get(reference) {
            Object::NativeFileHandle { resource } => Some(*resource),
            _ => None,
        }
    }

    /// Return the TCP resource in the first pending argument.
    fn pending_tcp_resource(&self, vm: VmId) -> Option<u64> {
        let machine = self.machines.get(vm as usize)?;
        let value = *machine.vm.pending.as_ref()?.args.first()?;
        let reference = value.as_obj()?;
        match machine.vm.heap.get(reference) {
            Object::NativeTcpStream { resource } | Object::NativeTcpListener { resource } => {
                Some(*resource)
            }
            _ => None,
        }
    }

    /// Return the external resource in the first pending argument.
    fn pending_bound_resource(&self, vm: VmId) -> Option<u64> {
        self.pending_file_resource(vm)
            .or_else(|| self.pending_tcp_resource(vm))
    }

    fn file_handle_resource(&self, holder: VmId, value: Value) -> Option<u64> {
        let reference = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(reference) {
            Object::NativeFileHandle { resource }
            | Object::NativeTcpStream { resource }
            | Object::NativeTcpListener { resource } => Some(*resource),
            _ => None,
        }
    }

    fn resource_control(&self, holder: VmId, value: Value) -> Option<(VmId, u64)> {
        let reference = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(reference) {
            Object::NativeResourceHandle { surface, resource } => Some((*surface, *resource)),
            _ => None,
        }
    }

    fn value_is_result_ok(&self, holder: VmId, value: Value) -> bool {
        value.as_obj().is_some_and(|reference| {
            matches!(
                self.machines[holder as usize].vm.heap.get(reference),
                Object::Instance { class, .. } if Some(*class) == self.core.result_ok
            )
        })
    }

    fn build_resource_control(
        &mut self,
        holder: VmId,
        surface: VmId,
        resource: u64,
    ) -> Result<Value, FaultCode> {
        self.machines[holder as usize].alloc(Object::NativeResourceHandle { surface, resource })
    }

    /// Find every machine in one controlled machine world.
    fn controlled_machines(&mut self, root: VmId) -> Result<Vec<VmId>, FaultCode> {
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
    fn controlled_file_resources(&mut self, root: VmId) -> Result<Vec<u64>, FaultCode> {
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
                    _ => continue,
                };
                if self.bound_resources.contains_key(&resource) {
                    resources.insert(resource);
                }
            }
        }
        Ok(resources.into_iter().collect())
    }

    fn build_resource_list(
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
        let list = self.machines[holder as usize].alloc(Object::List { items });
        for root in roots {
            self.machines[holder as usize].vm.heap.pop_host_root(root);
        }
        list
    }

    fn register_driver_file(&mut self, owner: VmId, driver: VmId) -> Result<u64, FaultCode> {
        self.register_driver_resource(owner, driver, crate::ResourceKind::File, lm_abi::OP_FS_OPEN)
    }

    fn register_driver_resource(
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
    fn closed_file_reply(&mut self, vm: VmId) -> Result<Value, FaultCode> {
        let closed = self.make_instance(vm, self.core.fs_error_closed, vec![])?;
        self.make_instance(vm, self.core.result_err, vec![closed])
    }

    /// Build one ordinary file-service failure.
    fn failed_file_reply(&mut self, vm: VmId, message: &str) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.to_string().into()))?;
        let error = self.make_instance(vm, self.core.fs_error_failed, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
    }

    /// Build the ordinary result for a closed network operation.
    fn closed_net_reply(&mut self, vm: VmId) -> Result<Value, FaultCode> {
        let closed = self.make_instance(vm, self.core.net_closed, vec![])?;
        self.make_instance(vm, self.core.result_err, vec![closed])
    }

    /// Build one successful unit network result.
    fn net_unit_reply(&mut self, vm: VmId) -> Result<Value, FaultCode> {
        self.make_instance(vm, self.core.result_ok, vec![Value::Unit])
    }

    /// Build one ordinary network-service failure.
    fn failed_net_reply(&mut self, vm: VmId, message: &str) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.to_string().into()))?;
        let error = self.make_instance(vm, self.core.net_failed, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
    }

    /// Build one ordinary invalid network argument.
    fn invalid_net_reply(&mut self, vm: VmId, message: &str) -> Result<Value, FaultCode> {
        let message = self.machines[vm as usize].alloc(Object::Str(message.to_string().into()))?;
        let error = self.make_instance(vm, self.core.net_invalid_input, vec![message])?;
        self.make_instance(vm, self.core.result_err, vec![error])
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
    fn deliver_asked(&mut self, child: VmId, parent: VmId, ordinal: u64) {
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
    fn deliver_routed_asked(&mut self, target: VmId, parent: VmId, ordinal: u64) {
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
    fn recover_routed_asked(&mut self, surface: VmId, parent: VmId, control_op: u32) {
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
    fn deliver_parked_route(
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
    fn park_routed_request(&mut self, surface: VmId, target: VmId, cursor: PolicyCursor) {
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
    fn route_cursor_to(&self, vm: VmId, op: u32, surface: VmId) -> Option<PolicyCursor> {
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
            if matches!(
                self.machines[parent as usize].vm.state,
                MachineState::Done | MachineState::Faulted
            ) {
                return None;
            }
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
    fn promote_next_route(&mut self, surface: VmId, served: VmId) {
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
    fn route_request(
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
    fn resolve_and_dispatch(
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
            Resolution::Driver { surface, cursor } => {
                return self.route_request(stack, surface, vm, cursor, dispatch_mode);
            }
            Resolution::Root => {
                if lm_abi::op(op).kind == lm_abi::OpKind::VmControl {
                    self.kernel_exec(stack, vm, op, dispatch_mode);
                } else {
                    if matches!(
                        op,
                        lm_abi::OP_FS_READ
                            | lm_abi::OP_FS_WRITE
                            | lm_abi::OP_FS_SEEK
                            | lm_abi::OP_FS_FLUSH
                            | lm_abi::OP_FS_CLOSE
                    ) && self
                        .pending_file_resource(vm)
                        .is_none_or(|resource| !self.bound_resources.contains_key(&resource))
                    {
                        let built = self.closed_file_reply(vm);
                        self.reply_or_fault(vm, op, built);
                        return None;
                    }
                    if matches!(
                        op,
                        lm_abi::OP_TCP_ACCEPT
                            | lm_abi::OP_TCP_READ
                            | lm_abi::OP_TCP_WRITE
                            | lm_abi::OP_TCP_SHUTDOWN
                            | lm_abi::OP_TCP_LOCAL_ADDRESS
                            | lm_abi::OP_TCP_PEER_ADDRESS
                    ) && self
                        .pending_tcp_resource(vm)
                        .is_none_or(|resource| !self.bound_resources.contains_key(&resource))
                    {
                        let built = self.closed_net_reply(vm);
                        self.reply_or_fault(vm, op, built);
                        return None;
                    }
                    if op == lm_abi::OP_TCP_CLOSE
                        && self
                            .pending_tcp_resource(vm)
                            .is_none_or(|resource| !self.bound_resources.contains_key(&resource))
                    {
                        let built = self.net_unit_reply(vm);
                        self.reply_or_fault(vm, op, built);
                        return None;
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
                        let built = if self.pending_file_resource(vm).is_some() {
                            self.failed_file_reply(vm, &message)
                        } else {
                            self.failed_net_reply(vm, &message)
                        };
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
    fn host_args(&self, vm: VmId) -> Result<Vec<HostArg>, FaultCode> {
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

    /// Convert one live TCP resource to an opaque host token.
    fn host_tcp_arg(
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
    fn host_socket_address(
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
    fn resolve_policy(&self, cursor: PolicyCursor, op: u32) -> Resolution {
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
                    }
                }
            }
        }
    }

    /// Run one mock handler in an ephemeral machine on the same loop.
    fn start_mock(&mut self, stack: &mut Vec<Activation>, vm: VmId, owner: VmId, closure: ObjRef) {
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
            self.module,
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

    /// Reserve one child machine from the budget of `parent`.
    ///
    /// The parent holds a child budget. Each reservation charges one
    /// unit to the parent and hands the rest of the budget to the
    /// child, so the machine tower can never grow deeper than the
    /// budget the root minted. The reservation happens before any
    /// machine record exists, so a refusal changes nothing.
    ///
    /// The local budget bounds tower depth per branch. `WorldBudget`
    /// bounds the total machine count and shared resources.
    pub(crate) fn reserve_child(&mut self, parent: VmId) -> Option<VmConfig> {
        if !self.share_heap_budget() {
            return None;
        }
        if !self.has_machine_room(1) || self.machines.try_reserve(1).is_err() {
            return None;
        }
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

    /// True when the machine table can add `count` records.
    pub(crate) fn has_machine_room(&self, count: usize) -> bool {
        self.machines
            .len()
            .checked_add(count)
            .is_some_and(|total| total <= self.budget.limits.max_machines as usize)
    }

    /// Attach the aggregate heap ledger before a second machine exists.
    pub(crate) fn share_heap_budget(&mut self) -> bool {
        if self.heap_shared {
            return true;
        }
        if self.machines.len() != 1 {
            return false;
        }
        if !self.machines[0]
            .vm
            .heap
            .attach_budget(self.budget.heap.clone())
        {
            return false;
        }
        self.heap_shared = true;
        true
    }

    /// Create one detached machine with the world ledgers.
    pub(crate) fn empty_machine(
        &self,
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
    ) -> Machine {
        debug_assert!(self.heap_shared);
        Machine::empty_with_budgets(
            config,
            parent,
            generation,
            self.budget.heap.clone(),
            self.budget.resources.clone(),
        )
    }

    /// Enter the proc body after the constructor frame returned.
    fn enter_proc_body(&mut self, vm: VmId, instance: Value) {
        let Some(body) = self.machines[vm as usize].start_body.take() else {
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the machine stores no proc body",
                None,
            );
            return;
        };
        let (func, env) = match self.machines[vm as usize].vm.heap.get(body) {
            Object::Closure { func, env, .. } => (*func, env.env()),
            _ => {
                self.machines[vm as usize].set_fault(
                    FaultCode::TypeMismatch,
                    "the proc body is not a closure",
                    None,
                );
                return;
            }
        };
        self.machines[vm as usize].load_frame(self.module, func, vec![instance], Some(body), env);
    }

    /// Read one machine handle out of a holder value.
    ///
    /// The argument comes from the pending record of the machine, and
    /// a restored machine states that record, so the read tests the
    /// shape. `None` faults the caller at its use site.
    fn handle_vm(&self, holder: VmId, value: Value) -> Option<VmId> {
        let r = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeVm { vm } => Some(*vm),
            _ => None,
        }
    }

    /// The machine one argument names, or a fault on the caller.
    fn vm_arg(&mut self, vm: VmId, op: u32, value: Value) -> Option<VmId> {
        match self.handle_vm(vm, value) {
            Some(target) => Some(target),
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::TypeMismatch,
                    "the receiver is not a machine handle",
                );
                None
            }
        }
    }

    /// Execute one VM control operation of the machine `vm`.
    fn kernel_exec(
        &mut self,
        stack: &mut Vec<Activation>,
        vm: VmId,
        op: u32,
        dispatch_mode: DispatchMode,
    ) {
        let stored: Vec<Value> = match self.machines[vm as usize].vm.pending.as_ref() {
            Some(pending) => pending.args.clone(),
            None => {
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::MalformedState,
                    "the kernel found no pending request",
                );
                return;
            }
        };
        // A restored machine states its own argument list. `arg` reads
        // a missing position as the uninitialized marker, and every
        // shape test below rejects that marker, so a short list faults
        // the caller instead of indexing past the list.
        let args = Args(&stored);
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
                let machine = self.empty_machine(child_config, Some(vm), 0);
                self.machines.push(machine);
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
            lm_abi::OP_VM_FROM_FN => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
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
                let Some(closure_ref) = program.as_obj() else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the program value is not a closure",
                    );
                    return;
                };
                let mut locals = Vec::new();
                if let Value::Obj(r) = args[2] {
                    let items = match self.machines[vm as usize].vm.heap.get(r) {
                        Object::Tuple { items } => items.clone(),
                        _ => {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::TypeMismatch,
                                "the argument view is not a tuple",
                            );
                            return;
                        }
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
                // The program closure carries the environment of the
                // frame that built it, so a machine whose entry
                // function is generic records the arguments that frame
                // applied.
                let (func, env) = match self.machines[target as usize].vm.heap.get(closure_ref) {
                    Object::Closure { func, env, .. } => (*func, env.env()),
                    _ => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::TypeMismatch,
                            "the program value is not a closure",
                        );
                        return;
                    }
                };
                // The arguments cross a machine boundary, so they meet
                // the parameter types of the program before the frame
                // loads them.
                if let Err(code) = self.check_frame_args(target, func, env, &locals) {
                    self.fault_caller(vm, op, code, "an argument does not carry its declared type");
                    return;
                }
                self.machines[target as usize].load_frame(
                    self.module,
                    func,
                    locals,
                    Some(closure_ref),
                    env,
                );
                match self.machines[vm as usize].alloc(Object::NativeVm { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_RUN
            | lm_abi::OP_VM_STEP
            | lm_abi::OP_VM_DRIVE
            | lm_abi::OP_VM_DRIVE_FOR => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                // `Vm.DriveFor` bounds the turn. A bound below one
                // retires nothing, so it reads as one instruction.
                let turn_fuel = if op == lm_abi::OP_VM_DRIVE_FOR {
                    match args[1] {
                        Value::Int(n) => Some((n.max(1)).min(u32::MAX as i64) as u32),
                        _ => {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::TypeMismatch,
                                "`Vm.DriveFor` needs an instruction count",
                            );
                            return;
                        }
                    }
                } else {
                    None
                };
                let (mode, family) = match op {
                    lm_abi::OP_VM_RUN => (StopMode::RunToTerminal, Family::Run),
                    lm_abi::OP_VM_STEP => (StopMode::OneStep, Family::Step),
                    lm_abi::OP_VM_DRIVE_FOR => (StopMode::DriveToAsk, Family::DriveFor),
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
                if self.machines[target as usize].vm.routed.is_some() {
                    if matches!(op, lm_abi::OP_VM_DRIVE | lm_abi::OP_VM_DRIVE_FOR) {
                        self.recover_routed_asked(target, vm, op);
                    } else {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine holds a routed request; drive it",
                        );
                    }
                    return;
                }
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
                        if matches!(op, lm_abi::OP_VM_DRIVE | lm_abi::OP_VM_DRIVE_FOR) {
                            // Token recovery: the same semantic request
                            // with a fresh holder token.
                            if self.machines[target as usize].vm.pending.is_none() {
                                self.fault_caller(
                                    vm,
                                    op,
                                    FaultCode::MalformedState,
                                    "the asked machine holds no request",
                                );
                                return;
                            }
                            let fresh = match self.machines[target as usize].take_request_ordinal()
                            {
                                Ok(ordinal) => ordinal,
                                Err(code) => {
                                    self.machines[target as usize].set_fault(
                                        code,
                                        "the request ordinal is exhausted",
                                        Some(op),
                                    );
                                    let built =
                                        self.build_terminal_event(target, vm, Family::Drive);
                                    self.reply_or_fault(vm, op, built);
                                    return;
                                }
                            };
                            if let Some(pending) =
                                self.machines[target as usize].vm.pending.as_mut()
                            {
                                pending.ordinal = fresh;
                            }
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
                    // A blocked machine takes this path too. It waits
                    // on another machine of this world, exactly as a
                    // waiting machine waits on the host. The driver
                    // loop suspends the whole stack at its next turn,
                    // and the scheduler parks this holder on the same
                    // wake condition. The holder resumes its control
                    // call when the condition clears.
                    MachineState::Ready | MachineState::Waiting | MachineState::Blocked => {
                        if self.machines[vm as usize].vm.nested.is_some() {
                            self.fault_caller(
                                vm,
                                op,
                                FaultCode::InvalidVmState,
                                "the machine already waits on nested control",
                            );
                            return;
                        }
                        self.machines[vm as usize].vm.nested = Some(target);
                        if dispatch_mode == DispatchMode::DeferNested {
                            self.machines[vm as usize].vm.state = MachineState::Ready;
                        } else {
                            self.push_activation(
                                stack,
                                Activation {
                                    vm: target,
                                    mode,
                                    family,
                                    reply_to: Some(vm),
                                    retired: false,
                                    fuel: turn_fuel,
                                },
                            );
                        }
                    }
                    // A running machine holds an execution reference,
                    // and the guard above already refused one.
                    MachineState::Running => {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::InvalidVmState,
                            "the machine is in use",
                        );
                    }
                }
            }
            lm_abi::OP_VM_DRIVE_WAIT => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                if self.machines[target as usize].vm.state == MachineState::Empty {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is empty");
                    return;
                }
                self.create_wait(vm, op, WaitSource::Drive { target });
            }
            lm_abi::OP_VM_TABLE => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                match self.machines[vm as usize].alloc(Object::NativeTable { vm: target }) {
                    Ok(handle) => self.install_value_reply(vm, handle),
                    Err(code) => self.machines[vm as usize].set_fault(code, "", Some(op)),
                }
            }
            lm_abi::OP_VM_HANDLES => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let built = self
                    .controlled_file_resources(target)
                    .and_then(|resources| self.build_resource_list(vm, target, &resources));
                self.reply_or_fault(vm, op, built);
            }
            lm_abi::OP_VM_RESOURCE => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let Some(resource) = self.file_handle_resource(vm, args[1]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not an external resource",
                    );
                    return;
                };
                let built = self.build_resource_control(vm, target, resource);
                self.reply_or_fault(vm, op, built);
            }
            lm_abi::OP_VM_SERVE_FILE => {
                let Some(surface) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) =
                    self.reply_sink(vm, op, surface, token.0, token.1, Some(lm_abi::OP_FS_OPEN))
                else {
                    return;
                };
                let resource = match self.register_driver_file(sink.target, vm) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeFileHandle { resource })
                    .and_then(|handle| {
                        self.make_instance(sink.target, self.core.result_ok, vec![handle])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_file(resource, false);
                        self.fault_caller(vm, op, code, "the file reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_file(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_file(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match Fs.Open",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_SERVE_TCP_STREAM => {
                let Some(surface) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                if token.2 != lm_abi::OP_TCP_CONNECT && token.2 != lm_abi::OP_TCP_ACCEPT {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::InvalidRequestToken,
                        "the call token is not for Tcp.Connect or Tcp.Accept",
                    );
                    return;
                }
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, Some(token.2))
                else {
                    return;
                };
                let peer = match self.transfer(vm, sink.target, args[2]) {
                    Ok(peer) => peer,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the peer address is not sendable");
                        return;
                    }
                };
                let resource = match self.register_driver_resource(
                    sink.target,
                    vm,
                    crate::ResourceKind::TcpStream,
                    token.2,
                ) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeTcpStream { resource })
                    .and_then(|stream| {
                        if token.2 == lm_abi::OP_TCP_ACCEPT {
                            self.make_instance(sink.target, self.core.pair, vec![stream, peer])
                        } else {
                            Ok(stream)
                        }
                    })
                    .and_then(|value| {
                        self.make_instance(sink.target, self.core.result_ok, vec![value])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the TCP reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_resource(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match the TCP call",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_SERVE_TCP_LISTENER => {
                let Some(surface) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|reference| {
                    match self.machines[vm as usize].vm.heap.get(reference) {
                        Object::NativeCall {
                            vm,
                            ordinal,
                            op: call_op,
                        } => Some((*vm, *ordinal, *call_op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(
                    vm,
                    op,
                    surface,
                    token.0,
                    token.1,
                    Some(lm_abi::OP_TCP_LISTEN),
                ) else {
                    return;
                };
                let resource = match self.register_driver_resource(
                    sink.target,
                    vm,
                    crate::ResourceKind::TcpListener,
                    lm_abi::OP_TCP_LISTEN,
                ) {
                    Ok(resource) => resource,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the resource limit is full");
                        return;
                    }
                };
                let built = self.machines[sink.target as usize]
                    .alloc(Object::NativeTcpListener { resource })
                    .and_then(|listener| {
                        self.make_instance(sink.target, self.core.result_ok, vec![listener])
                    });
                let reply = match built {
                    Ok(reply) => reply,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the TCP reply allocation failed");
                        return;
                    }
                };
                let control = match self.build_resource_control(vm, surface, resource) {
                    Ok(control) => control,
                    Err(code) => {
                        self.retire_resource(resource, false);
                        self.fault_caller(vm, op, code, "the resource control allocation failed");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                if self.machines[sink.target as usize].vm.state == MachineState::Faulted {
                    self.retire_resource(resource, false);
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the minted reply did not match Tcp.Listen",
                    );
                    return;
                }
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, control);
            }
            lm_abi::OP_VM_RESOURCE_SAME => {
                let Some((_left_surface, left)) = self.resource_control(vm, args[0]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the receiver is not a resource control",
                    );
                    return;
                };
                let Some((_right_surface, right)) = self.resource_control(vm, args[1]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a resource control",
                    );
                    return;
                };
                let same = left == right && self.bound_resources.contains_key(&left);
                self.install_value_reply(vm, Value::Bool(same));
            }
            lm_abi::OP_VM_RESOURCE_IS_OPEN
            | lm_abi::OP_VM_RESOURCE_CLOSE
            | lm_abi::OP_VM_RESOURCE_KIND => {
                let Some((_surface, resource)) = self.resource_control(vm, args[0]) else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the receiver is not a resource control",
                    );
                    return;
                };
                match op {
                    lm_abi::OP_VM_RESOURCE_IS_OPEN => {
                        let open = self.bound_resources.contains_key(&resource);
                        self.install_value_reply(vm, Value::Bool(open));
                    }
                    lm_abi::OP_VM_RESOURCE_CLOSE => {
                        let closed = self.retire_resource(resource, true);
                        self.install_value_reply(vm, Value::Bool(closed));
                    }
                    _ => {
                        let name = self
                            .bound_resources
                            .get(&resource)
                            .map(|bound| match bound.kind {
                                crate::ResourceKind::File => "file",
                                crate::ResourceKind::TcpStream => "tcp-stream",
                                crate::ResourceKind::TcpListener => "tcp-listener",
                                crate::ResourceKind::PendingOperation => "pending-operation",
                            })
                            .unwrap_or("closed");
                        let built = self.machines[vm as usize].alloc(Object::Str(name.into()));
                        self.reply_or_fault(vm, op, built);
                    }
                }
            }
            lm_abi::OP_VM_ANSWER => {
                let Some(surface) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|r| {
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeCall { vm, ordinal, op } => Some((*vm, *ordinal, *op)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a call token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, Some(token.2))
                else {
                    return;
                };
                let reply = match self.transfer(vm, sink.target, args[2]) {
                    Ok(value) => value,
                    Err(code) => {
                        self.fault_caller(vm, op, code, "the reply is not sendable");
                        return;
                    }
                };
                self.install_value_reply(sink.target, reply);
                self.consume_reply_sink(sink);
                self.install_value_reply(vm, Value::Unit);
            }
            lm_abi::OP_VM_REJECT | lm_abi::OP_VM_DISPATCH => {
                let Some(surface) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                let found = args[1].as_obj().and_then(|r| {
                    match self.machines[vm as usize].vm.heap.get(r) {
                        Object::NativeRequest { vm, ordinal } => Some((*vm, *ordinal)),
                        _ => None,
                    }
                });
                let Some(token) = found else {
                    self.fault_caller(
                        vm,
                        op,
                        FaultCode::TypeMismatch,
                        "the argument is not a request token",
                    );
                    return;
                };
                let Some(sink) = self.reply_sink(vm, op, surface, token.0, token.1, None) else {
                    return;
                };
                if op == lm_abi::OP_VM_REJECT {
                    let built = args[2].as_obj().and_then(|r| {
                        match self.machines[vm as usize].vm.heap.get(r) {
                            Object::NativeFault { code, message, op } => Some(FaultRec {
                                code: *code,
                                message: message.clone(),
                                op: *op,
                            }),
                            _ => None,
                        }
                    });
                    let Some(rec) = built else {
                        self.fault_caller(
                            vm,
                            op,
                            FaultCode::TypeMismatch,
                            "the argument is not a fault value",
                        );
                        return;
                    };
                    let pending_op = self.pending_op(sink.target);
                    self.machines[sink.target as usize].set_fault(
                        rec.code,
                        rec.message,
                        pending_op,
                    );
                    self.consume_reply_sink(sink);
                    self.install_value_reply(vm, Value::Unit);
                } else {
                    // The caller's reply installs before policy can
                    // stack a mock run above it.
                    self.consume_reply_sink(sink);
                    self.install_value_reply(vm, Value::Unit);
                    let _ = self.resolve_and_dispatch(
                        stack,
                        sink.target,
                        sink.cursor,
                        DispatchMode::DeferNested,
                    );
                }
            }
            lm_abi::OP_VM_SNAPSHOT_HELD => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
                    return;
                };
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                self.take_snapshot(vm, op, target, false);
            }
            lm_abi::OP_VM_SNAPSHOT_WAIT_HELD => {
                let Some(target) = self.vm_arg(vm, op, args[0]) else {
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
                if target == vm || self.machines[target as usize].active > 0 {
                    self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
                    return;
                }
                if !self.expect_holder_owned(vm, op, target) {
                    return;
                }
                let result = self.snapshot_wait(target, fuel.max(0) as u64);
                self.install_snapshot_result(vm, op, result);
            }
            lm_abi::OP_VM_SNAPSHOT_SELF => {
                // The performing machine is the root of its own world.
                // The capture runs while `Vm.SnapshotSelf` is pending,
                // so the restored root holds that request
                // (specification 17.6).
                self.take_snapshot(vm, op, vm, true);
            }
            lm_abi::OP_VM_LOAD_SNAPSHOT => {
                // This build has no guest snapshot decoder.
                self.fault_caller(
                    vm,
                    op,
                    FaultCode::InvalidVmState,
                    "Vm.LoadSnapshot is not available in this build",
                );
            }
            lm_abi::OP_VM_RESTORE => self.restore_snapshot(vm, op, args),
            lm_abi::OP_PROC_RECV_WAIT
            | lm_abi::OP_WAIT_WAIT
            | lm_abi::OP_WAIT_CHOOSE
            | lm_abi::OP_WAIT_CANCEL => self.wait_exec(vm, op, args),
            lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_SPAWN
            | lm_abi::OP_PROC_SEND
            | lm_abi::OP_PROC_CLOSE
            | lm_abi::OP_PROC_RECV
            | lm_abi::OP_PROC_DONE
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME
            | lm_abi::OP_PROC_SNAPSHOT_WAIT => self.proc_exec(vm, op, stored),
            // Every `VmControl` slot of the manifest has an arm above.
            // A slot without one names a manifest this build does not
            // hold, so the caller faults.
            _ => self.fault_caller(
                vm,
                op,
                FaultCode::MalformedState,
                "the operation has no kernel rule",
            ),
        }
    }

    // ------------------------------------------------------------
    // Typed waits.
    // ------------------------------------------------------------

    fn allocate_wait(&mut self, vm: VmId, source: WaitSource) -> Result<(u64, Value), FaultCode> {
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

    fn create_wait(&mut self, vm: VmId, op: u32, source: WaitSource) {
        match self.allocate_wait(vm, source) {
            Ok((_, value)) => self.install_value_reply(vm, value),
            Err(code) => self.fault_caller(vm, op, code, "the wait limit is full"),
        }
    }

    fn wait_token(&mut self, vm: VmId, op: u32, value: Value) -> Option<u64> {
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

    fn wait_root_is_current(&mut self, vm: VmId, op: u32, token: u64) -> bool {
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

    fn wait_tree(&self, vm: VmId, root: u64) -> Result<(Vec<WaitLeafPath>, Vec<u64>), FaultCode> {
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

    fn retire_wait_tree(&mut self, vm: VmId, tokens: &[u64]) {
        for token in tokens {
            self.machines[vm as usize].vm.waits.remove(token);
        }
    }

    fn wait_exec(&mut self, vm: VmId, op: u32, args: Args<'_>) {
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

    fn validate_wait_leases(&mut self, vm: VmId, op: u32, leaves: &[WaitLeafPath]) -> bool {
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

    fn receive_wait_ready(&self, vm: VmId) -> bool {
        let mailbox = &self.machines[vm as usize].vm.mailbox;
        !mailbox.queue.is_empty() || mailbox.closed
    }

    fn drive_wait_ready(&self, target: VmId) -> bool {
        self.machines[target as usize].vm.routed.is_some()
            || matches!(
                self.machines[target as usize].vm.state,
                MachineState::Asked | MachineState::Done | MachineState::Faulted
            )
    }

    fn complete_ready_wait(&mut self, vm: VmId, token: u64) -> bool {
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

    fn take_receive_wait_value(&mut self, vm: VmId) -> Result<Value, FaultCode> {
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

    fn take_drive_wait_value(&mut self, vm: VmId, surface: VmId) -> Result<Value, FaultCode> {
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

    fn fresh_asked_wait_value(&mut self, vm: VmId, target: VmId) -> Result<Value, FaultCode> {
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

    fn wrap_wait_choice(
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

    /// Capture one machine world and install the typed result.
    fn take_snapshot(&mut self, vm: VmId, op: u32, root: VmId, self_root: bool) {
        // A barrier identifier and a world gate both need one number
        // this world never repeats, and one monotone counter serves
        // both. The two live in different machine fields, so a shared
        // counter never confuses a barrier with a gate.
        let barrier = self.next_gate();
        let result = self.capture_snapshot(barrier, root, self_root);
        self.install_snapshot_result(vm, op, result);
    }

    fn install_snapshot_result(
        &mut self,
        vm: VmId,
        op: u32,
        result: Result<crate::snapshot::SnapshotImage, crate::snapshot::SnapshotFail>,
    ) {
        let built = match result {
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
    fn restore_snapshot(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some(target) = self.vm_arg(vm, op, args[0]) else {
            return;
        };
        if target == vm || self.machines[target as usize].active > 0 {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is in use");
            return;
        }
        if self.machines[target as usize].vm.state != MachineState::Empty {
            self.fault_caller(vm, op, FaultCode::InvalidVmState, "the machine is loaded");
            return;
        }
        let found =
            args[1]
                .as_obj()
                .and_then(|r| match self.machines[vm as usize].vm.heap.get(r) {
                    Object::NativeSnapshot(image) => Some(image.clone()),
                    _ => None,
                });
        let Some(bytes) = found else {
            self.fault_caller(
                vm,
                op,
                FaultCode::TypeMismatch,
                "the argument is not a snapshot value",
            );
            return;
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
        let reply = match self.prepare_restore_reply(vm, target) {
            Ok(reply) => reply,
            Err(code) => {
                self.machines[vm as usize].set_fault(code, "", Some(op));
                return;
            }
        };
        if let Err(code) = self.check_reply(vm, reply.value) {
            self.discard_restore_reply(vm, reply);
            self.machines[vm as usize].set_fault(
                code,
                "the reply does not carry the type of its perform",
                Some(op),
            );
            return;
        }
        if let Err(code) = self.reserve_restore_reply_slot(vm) {
            self.discard_restore_reply(vm, reply);
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
                self.make_instance(vm, self.core.restore_limit_exceeded, vec![])
                    .and_then(|error| self.make_instance(vm, self.core.result_err, vec![error]))
            }
            // The check above already answered this case, so the guest
            // never reaches it. Restore states the rule again for
            // every caller, and a mismatch here is a boundary fault.
            Err(crate::snapshot::RestoreFail::OtherProgram) => {
                self.discard_restore_reply(vm, reply);
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
    fn prepare_restore_reply(
        &mut self,
        vm: VmId,
        target: VmId,
    ) -> Result<PreparedRestoreReply, FaultCode> {
        let class = self.core.result_ok.ok_or(FaultCode::MalformedState)?;
        let handle = Object::NativeVm { vm: target };
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
    fn discard_restore_reply(&mut self, vm: VmId, reply: PreparedRestoreReply) {
        let heap = &mut self.machines[vm as usize].vm.heap;
        heap.free(reply.reply);
        heap.free(reply.handle);
    }

    /// Reserve the operand slot for one prepared restore reply.
    fn reserve_restore_reply_slot(&mut self, vm: VmId) -> Result<(), FaultCode> {
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
    fn install_prepared_restore_reply(&mut self, vm: VmId, reply: PreparedRestoreReply) {
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

    /// Validate one direct or routed continuation token once.
    fn reply_sink(
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
    fn consume_reply_sink(&mut self, sink: ReplySink) {
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
    fn fault_caller(&mut self, vm: VmId, op: u32, code: FaultCode, message: &str) {
        self.machines[vm as usize].set_fault(code, message, Some(op));
    }

    // ------------------------------------------------------------
    // Procs, mailboxes, and the scheduler interface.
    // ------------------------------------------------------------

    /// Read one proc reference out of a handle value.
    ///
    /// The argument comes from the pending record, so the read tests
    /// the shape and the caller faults on `None`.
    fn handle_proc(&self, holder: VmId, value: Value) -> Option<(VmId, u32)> {
        let r = value.as_obj()?;
        match self.machines[holder as usize].vm.heap.get(r) {
            Object::NativeHandle { proc, generation } => Some((*proc, *generation)),
            _ => None,
        }
    }

    /// The proc one argument names, or a fault on the caller.
    fn proc_arg(&mut self, vm: VmId, op: u32, value: Value) -> Option<(VmId, u32)> {
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
    fn boundary_copy(&mut self, src: VmId, dst: VmId, value: Value) -> Result<Value, FaultCode> {
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
    fn proc_exec(&mut self, vm: VmId, op: u32, stored: Vec<Value>) {
        // A restored machine states its own argument list, so a short
        // list reads as the uninitialized marker and every shape test
        // below rejects it.
        let args = Args(&stored);
        match op {
            lm_abi::OP_PROC_SPAWN => self.proc_spawn(vm, op, args),
            lm_abi::OP_PROC_RUN => self.proc_run(vm, op, args),
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
    fn proc_spawn(&mut self, vm: VmId, op: u32, args: Args<'_>) {
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
        let machine = self.empty_machine(child_config, Some(vm), 0);
        self.machines.push(machine);
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
        let Some(group) = lm_abi::group_by_name("Proc") else {
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
        let (birth_ops, birth_groups) = self.declared_grants(body_func);
        let limit = self.machines[child as usize].config.mailbox_limit;
        {
            let m = &mut self.machines[child as usize];
            m.table.group[group as usize] = Some(Action::Pass);
            // The birth grant also passes the declared row of the proc
            // body. The spawner charges the same row, so this creates
            // no authority. A proc that drives therefore needs no
            // pause and no table edit before it runs.
            for op in &birth_ops {
                if let Some(action) = m.table.exact.get_mut(*op as usize) {
                    *action = Some(Action::Pass);
                }
            }
            for group in &birth_groups {
                if let Some(action) = m.table.group.get_mut(*group as usize) {
                    *action = Some(Action::Pass);
                }
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
        self.machines[child as usize].load_frame(
            self.module,
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
    fn proc_run(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some(target) = self.vm_arg(vm, op, args[0]) else {
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

    /// `h.send(message)`.
    ///
    /// The mailbox limit is checked before the copy, so a refused
    /// message never enters the target heap.
    fn proc_send(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
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
        let built = self.make_instance(vm, self.core.send_sent, vec![]);
        self.reply_or_fault(vm, op, built);
    }

    /// `h.close()`. A successful close returns `Sent`; a repeat
    /// returns `Closed` (specification 18.4).
    fn proc_close(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        if !self.proc_running(proc, generation) {
            let built = self
                .make_fault(vm, FaultCode::DeadProc, "the target proc is dead")
                .and_then(|fault| self.make_instance(vm, self.core.send_fault, vec![fault]));
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
    fn proc_done(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
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

    /// `h.snapshot_wait(fuel)`: capture the proc at its next safe boundary.
    fn proc_snapshot_wait(&mut self, vm: VmId, op: u32, args: Args<'_>, remaining: Option<u64>) {
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

    fn snapshot_target_can_progress(&mut self, caller: VmId, root: VmId) -> bool {
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

    /// Build and install `ProcResult` for one terminal proc.
    fn publish_terminal(&mut self, vm: VmId, op: u32, proc: VmId) {
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
            }),
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
    fn proc_pause(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        let arm = if !self.proc_running(proc, generation) {
            self.core.proc_error_dead
        } else if self.machines[proc as usize].paused {
            self.core.proc_error_already_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core.proc_error_in_use
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
    fn proc_resume(&mut self, vm: VmId, op: u32, args: Args<'_>) {
        let Some((proc, generation)) = self.proc_arg(vm, op, args[0]) else {
            return;
        };
        let arm = if !self.proc_running(proc, generation) {
            self.core.proc_error_dead
        } else if !self.machines[proc as usize].paused {
            self.core.proc_error_not_paused
        } else if self.machines[proc as usize].active > 0 {
            self.core.proc_error_in_use
        } else {
            if let Err(code) = self.prepare_scheduler_proc(proc) {
                self.fault_caller(vm, op, code, "the scheduler has no task capacity");
                return;
            }
            self.machines[proc as usize].owner = Ownership::Scheduler;
            self.machines[proc as usize].paused = false;
            self.activate_scheduler_proc_prepared(proc);
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
        let t = &mut self.machines[target as usize].table;
        // The verifier bounds every table-edit slot, so the store
        // reaches a live entry. A slot outside the table drops the
        // edit instead of indexing past it.
        let cell = if kind == 0 {
            t.exact.get_mut(slot as usize)
        } else {
            t.group.get_mut(slot as usize)
        };
        match cell {
            Some(cell) => *cell = entry,
            None => {
                self.machines[vm as usize].set_fault(
                    FaultCode::MalformedState,
                    "the table edit names no policy slot",
                    None,
                );
                return;
            }
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
    fn handle_request_op(&mut self, vm: VmId, request: ObjRef) {
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
        let built = self.machines[vm as usize].alloc(Object::Str(lm_abi::op_name(op).into()));
        match built.and_then(|value| self.machines[vm as usize].push(value).map(|_| ())) {
            Ok(()) => {}
            Err(code) => self.machines[vm as usize].set_fault(code, "", None),
        }
    }

    /// The operation identity test of a `Call` pattern, run by `vm`.
    fn handle_as_call(&mut self, vm: VmId, request: ObjRef, op: u32) {
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
            Block::Snapshot {
                target,
                generation,
                remaining,
                retry,
            } => !self.proc_alive(target, generation) || remaining == 0 || retry,
            Block::Wait { .. } => false,
        }
    }

    /// The wake condition stored by one blocked machine.
    fn block_wake_key(&self, vm: VmId) -> Option<WakeKey> {
        let machine = self.machines.get(vm as usize)?;
        let own = TaskKey {
            vm,
            generation: machine.generation,
        };
        match machine.vm.block? {
            Block::Receive => Some(WakeKey::Receive(own)),
            Block::Send { target, generation } => Some(WakeKey::Send(TaskKey {
                vm: target,
                generation,
            })),
            Block::Done { target, generation } => Some(WakeKey::Done(TaskKey {
                vm: target,
                generation,
            })),
            Block::Snapshot {
                target, generation, ..
            } => Some(WakeKey::Snapshot(TaskKey {
                vm: target,
                generation,
            })),
            Block::Wait { .. } => None,
        }
    }

    /// Complete one ready proc block.
    fn complete_blocked_machine(&mut self, vm: VmId) {
        let found = self.machines[vm as usize]
            .vm
            .pending
            .as_ref()
            .map(|pending| (pending.op, pending.args.clone()));
        let Some((op, args)) = found else {
            let pending_op = self.pending_op(vm);
            self.machines[vm as usize].vm.block = None;
            self.machines[vm as usize].set_fault(
                FaultCode::MalformedState,
                "the blocked machine holds no request",
                pending_op,
            );
            return;
        };
        let snapshot_remaining = match self.machines[vm as usize].vm.block {
            Some(Block::Snapshot { remaining, .. }) => Some(remaining),
            _ => None,
        };
        self.machines[vm as usize].vm.block = None;
        self.machines[vm as usize].vm.state = MachineState::Ready;
        self.record(TraceEvent::Unblock { vm });
        if op == lm_abi::OP_PROC_SNAPSHOT_WAIT {
            self.proc_snapshot_wait(vm, op, Args(&args), snapshot_remaining);
        } else {
            self.proc_exec(vm, op, args);
        }
    }

    fn release_saved_stack(&mut self, base: VmId) {
        let Some(saved) = self.suspended.remove(&base) else {
            return;
        };
        if let SuspendReason::Parked { wait, .. } = saved.reason {
            let mut seen = Vec::new();
            self.quiesce_wait_leases(wait.owner.vm, wait.token, &mut seen);
        }
        for activation in saved.activations.into_iter().rev() {
            self.release_activation(activation);
        }
    }

    fn quiesce_wait_leases(&mut self, vm: VmId, token: u64, seen: &mut Vec<VmId>) {
        if seen.contains(&vm) || seen.len() >= self.machines.len() {
            return;
        }
        seen.push(vm);
        let drives: Vec<VmId> = self
            .wait_tree(vm, token)
            .map(|(leaves, _)| {
                leaves
                    .into_iter()
                    .filter_map(|leaf| match leaf.leaf {
                        WaitLeaf::Drive { target } => Some(target),
                        WaitLeaf::Receive => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for target in drives {
            if let Some(saved) = self.suspended.get(&target) {
                if let SuspendReason::Parked { wait, .. } = saved.reason {
                    self.quiesce_wait_leases(wait.owner.vm, wait.token, seen);
                }
            }
            self.release_saved_stack(target);
        }
        seen.pop();
    }

    fn mark_wait_stack_ready(&mut self, base: VmId, wait: WaitSetKey) {
        if let Some(saved) = self.suspended.get_mut(&base) {
            if matches!(saved.reason, SuspendReason::Parked { wait: held, .. } if held == wait) {
                saved.reason = SuspendReason::Yielded;
            }
        }
    }

    fn service_wait_slice(&mut self, base: VmId, wait: WaitSetKey, quantum: u32) -> SliceExit {
        if self.complete_ready_wait(wait.owner.vm, wait.token) {
            self.mark_wait_stack_ready(base, wait);
            return SliceExit::Yielded;
        }
        let leaves = match self.wait_tree(wait.owner.vm, wait.token) {
            Ok((leaves, _)) => leaves,
            Err(_) => {
                let op = self.pending_op(wait.owner.vm);
                self.machines[wait.owner.vm as usize].set_fault(
                    FaultCode::MalformedState,
                    "the wait tree is malformed",
                    op,
                );
                self.mark_wait_stack_ready(base, wait);
                return SliceExit::Yielded;
            }
        };
        let mut selected = None;
        for leaf in leaves {
            let WaitLeaf::Drive { target } = leaf.leaf else {
                continue;
            };
            let mut sources = Vec::new();
            let mut seen = vec![wait];
            if self.append_drive_sources(target, &mut sources, &mut seen) {
                selected = Some(target);
                break;
            }
        }
        let Some(target) = selected else {
            return match self.wait_set_status(wait) {
                TaskStatus::Parked(wait) => SliceExit::Parked(wait),
                _ => SliceExit::Yielded,
            };
        };
        let _ = self.drive_holder_slice(target, quantum.max(1));
        if self.complete_ready_wait(wait.owner.vm, wait.token) {
            self.mark_wait_stack_ready(base, wait);
        }
        match self.wait_set_status(wait) {
            TaskStatus::Parked(wait) => SliceExit::Parked(wait),
            _ => SliceExit::Yielded,
        }
    }

    fn drive_holder_slice(&mut self, target: VmId, quantum: u32) -> SliceExit {
        if let Some(saved) = self.suspended.get(&target) {
            if let SuspendReason::Parked { wait, .. } = saved.reason {
                return self.service_wait_slice(target, wait, quantum);
            }
        }
        if self.machines[target as usize].vm.routed.is_some()
            || matches!(
                self.machines[target as usize].vm.state,
                MachineState::Asked | MachineState::Done | MachineState::Faulted
            )
        {
            return SliceExit::Terminal;
        }
        if self.machines[target as usize].vm.state == MachineState::Blocked
            && !self.suspended.contains_key(&target)
        {
            if matches!(
                self.machines[target as usize].vm.block,
                Some(Block::Wait { .. })
            ) {
                let Some(Block::Wait { token }) = self.machines[target as usize].vm.block else {
                    unreachable!("the guard selects a wait block")
                };
                let wait = WaitSetKey {
                    owner: TaskKey {
                        vm: target,
                        generation: self.machines[target as usize].generation,
                    },
                    token,
                };
                return self.service_wait_slice(target, wait, quantum);
            }
            if self.block_ready(target) {
                self.complete_blocked_machine(target);
            }
        }
        let event = if self.suspended.contains_key(&target) {
            self.resume_stack_with_quantum(target, Some(quantum.max(1)))
        } else {
            let mut stack = Vec::new();
            self.push_activation(
                &mut stack,
                Activation {
                    vm: target,
                    mode: StopMode::DriveToAsk,
                    family: Family::Drive,
                    reply_to: None,
                    retired: false,
                    fuel: None,
                },
            );
            self.drive_stack(&mut stack, Some(quantum.max(1)))
        };
        match event {
            RootEvent::Blocked => self
                .suspended
                .get(&target)
                .and_then(|saved| match saved.reason {
                    SuspendReason::Blocked { wake, .. } => Some(SliceExit::Blocked(wake)),
                    SuspendReason::Parked { wait, .. } => Some(SliceExit::Parked(wait)),
                    _ => None,
                })
                .unwrap_or(SliceExit::Yielded),
            RootEvent::Waiting => self
                .suspended
                .get(&target)
                .and_then(|saved| match saved.reason {
                    SuspendReason::Waiting { completion, .. } => {
                        Some(SliceExit::Waiting(completion))
                    }
                    _ => None,
                })
                .unwrap_or(SliceExit::Yielded),
            RootEvent::Ran => SliceExit::Yielded,
            RootEvent::Asked(_) | RootEvent::Done(_) | RootEvent::Fault(_) => SliceExit::Terminal,
        }
    }

    /// The stable identity of one current machine record.
    pub fn task_key(&self, vm: VmId) -> Option<TaskKey> {
        self.machines.get(vm as usize).map(|machine| TaskKey {
            vm,
            generation: machine.generation,
        })
    }

    fn wait_set_status(&self, wait: WaitSetKey) -> TaskStatus {
        let mut sources = Vec::new();
        let mut seen = Vec::new();
        if self.append_wait_sources(wait, &mut sources, &mut seen) {
            return TaskStatus::Ready;
        }
        sources.sort_unstable();
        sources.dedup();
        if sources.is_empty() {
            TaskStatus::Ready
        } else {
            TaskStatus::Parked(wait)
        }
    }

    /// The current scheduler sources of one parked typed wait.
    pub fn wait_sources(&self, wait: WaitSetKey) -> Vec<WaitSourceKey> {
        let mut sources = Vec::new();
        let mut seen = Vec::new();
        if !self.append_wait_sources(wait, &mut sources, &mut seen) {
            sources.sort_unstable();
            sources.dedup();
        } else {
            sources.clear();
        }
        sources
    }

    /// Add unavailable sources. Return true when one source can run.
    fn append_wait_sources(
        &self,
        wait: WaitSetKey,
        sources: &mut Vec<WaitSourceKey>,
        seen: &mut Vec<WaitSetKey>,
    ) -> bool {
        if seen.contains(&wait) || seen.len() >= self.machines.len() {
            return true;
        }
        let Some(machine) = self.machines.get(wait.owner.vm as usize) else {
            return true;
        };
        if machine.generation != wait.owner.generation
            || machine.vm.block != Some(Block::Wait { token: wait.token })
        {
            return true;
        }
        let Ok((leaves, _)) = self.wait_tree(wait.owner.vm, wait.token) else {
            return true;
        };
        seen.push(wait);
        for leaf in leaves {
            match leaf.leaf {
                WaitLeaf::Receive => {
                    if self.receive_wait_ready(wait.owner.vm) {
                        seen.pop();
                        return true;
                    }
                    sources.push(WaitSourceKey::Wake(WakeKey::Receive(wait.owner)));
                }
                WaitLeaf::Drive { target } => {
                    if self.append_drive_sources(target, sources, seen) {
                        seen.pop();
                        return true;
                    }
                }
            }
        }
        seen.pop();
        false
    }

    fn append_drive_sources(
        &self,
        target: VmId,
        sources: &mut Vec<WaitSourceKey>,
        seen: &mut Vec<WaitSetKey>,
    ) -> bool {
        let Some(machine) = self.machines.get(target as usize) else {
            return true;
        };
        if machine.vm.routed.is_some()
            || matches!(
                machine.vm.state,
                MachineState::Asked | MachineState::Done | MachineState::Faulted
            )
        {
            return true;
        }
        if let Some(saved) = self.suspended.get(&target) {
            return match saved.reason {
                SuspendReason::Yielded => true,
                SuspendReason::Blocked {
                    machine: blocked,
                    wake,
                } => {
                    if self.block_ready(blocked) {
                        true
                    } else {
                        sources.push(WaitSourceKey::Wake(wake));
                        false
                    }
                }
                SuspendReason::Waiting {
                    machine: waiting,
                    completion,
                } => {
                    if self.machines[waiting as usize].vm.state == MachineState::Waiting {
                        sources.push(WaitSourceKey::Completion(completion));
                        false
                    } else {
                        true
                    }
                }
                SuspendReason::Parked { wait, .. } => self.append_wait_sources(wait, sources, seen),
            };
        }
        match machine.vm.state {
            MachineState::Ready | MachineState::Running | MachineState::Empty => true,
            MachineState::Waiting => match self.completion_key(target) {
                Some(completion) => {
                    sources.push(WaitSourceKey::Completion(completion));
                    false
                }
                None => true,
            },
            MachineState::Blocked => match machine.vm.block {
                Some(Block::Wait { token }) => self.append_wait_sources(
                    WaitSetKey {
                        owner: TaskKey {
                            vm: target,
                            generation: machine.generation,
                        },
                        token,
                    },
                    sources,
                    seen,
                ),
                _ if self.block_ready(target) => true,
                _ => match self.block_wake_key(target) {
                    Some(wake) => {
                        sources.push(WaitSourceKey::Wake(wake));
                        false
                    }
                    None => true,
                },
            },
            MachineState::Asked | MachineState::Done | MachineState::Faulted => true,
        }
    }

    /// The scheduler view of one task without a machine-table scan.
    pub fn task_status(&self, key: TaskKey) -> TaskStatus {
        let Some(machine) = self.machines.get(key.vm as usize) else {
            return TaskStatus::Dormant;
        };
        if machine.generation != key.generation {
            return TaskStatus::Dormant;
        }
        if matches!(machine.vm.state, MachineState::Done | MachineState::Faulted) {
            return TaskStatus::Terminal;
        }
        let root = key.vm == 0;
        if !root
            && (!self.scheduler_procs.contains(key)
                || machine.owner != Ownership::Scheduler
                || machine.paused)
        {
            return TaskStatus::Dormant;
        }
        if machine.barrier.is_some() || machine.gate != 0 {
            return TaskStatus::Dormant;
        }
        if let Some(saved) = self.suspended.get(&key.vm) {
            // A descendant parked a request on a surface this stack
            // drives. The task must run, whatever it waited for, so
            // the driver can answer.
            if saved.activations.iter().any(|act| {
                act.mode == StopMode::DriveToAsk
                    && self.machines[act.vm as usize].vm.routed.is_some()
            }) {
                return TaskStatus::Ready;
            }
            return match saved.reason {
                SuspendReason::Yielded => TaskStatus::Ready,
                SuspendReason::Blocked {
                    machine: blocked,
                    wake,
                } => {
                    if self
                        .machines
                        .get(blocked as usize)
                        .is_some_and(|machine| machine.vm.state == MachineState::Blocked)
                        && !self.block_ready(blocked)
                    {
                        TaskStatus::Blocked(wake)
                    } else {
                        TaskStatus::Ready
                    }
                }
                SuspendReason::Waiting {
                    machine: waiting,
                    completion,
                } => {
                    if self
                        .machines
                        .get(waiting as usize)
                        .is_some_and(|machine| machine.vm.state == MachineState::Waiting)
                    {
                        TaskStatus::Waiting(completion)
                    } else {
                        TaskStatus::Ready
                    }
                }
                SuspendReason::Parked { wait, .. } => self.wait_set_status(wait),
            };
        }
        if machine.active > 0 {
            return TaskStatus::Dormant;
        }
        match machine.vm.state {
            MachineState::Ready => TaskStatus::Ready,
            MachineState::Blocked if matches!(machine.vm.block, Some(Block::Wait { .. })) => {
                let Some(Block::Wait { token }) = machine.vm.block else {
                    unreachable!("the guard selects a wait block")
                };
                self.wait_set_status(WaitSetKey { owner: key, token })
            }
            MachineState::Blocked if self.block_ready(key.vm) => TaskStatus::Ready,
            MachineState::Blocked => self
                .block_wake_key(key.vm)
                .map(TaskStatus::Blocked)
                .unwrap_or(TaskStatus::Ready),
            MachineState::Waiting => self
                .completion_key(key.vm)
                .map(TaskStatus::Waiting)
                .unwrap_or(TaskStatus::Ready),
            // An asked machine parked with a request for its driver.
            // The driver answers it; the scheduler must not run it.
            MachineState::Asked => TaskStatus::Dormant,
            MachineState::Empty | MachineState::Running => TaskStatus::Ready,
            MachineState::Done | MachineState::Faulted => TaskStatus::Terminal,
        }
    }

    /// The root and active proc states for a new scheduler run.
    pub fn scheduler_seeds(&self, include_root: bool) -> Vec<(TaskKey, TaskStatus)> {
        let mut keys = self.scheduler_procs.entries().to_vec();
        keys.sort_unstable();
        let mut out = Vec::with_capacity(keys.len() + usize::from(include_root));
        if include_root {
            let root = TaskKey {
                vm: 0,
                generation: self.machines[0].generation,
            };
            out.push((root, self.task_status(root)));
        }
        out.extend(keys.into_iter().map(|key| (key, self.task_status(key))));
        out
    }

    /// Swap pending changes into a reusable scheduler buffer.
    pub fn swap_schedule_events(&mut self, events: &mut ScheduleEvents) -> bool {
        if self.schedule_events.is_empty() {
            return false;
        }
        std::mem::swap(&mut self.schedule_events, events);
        true
    }

    fn emit_ready(&mut self, key: TaskKey) {
        self.schedule_events.push_ready(key);
    }

    fn emit_removed(&mut self, key: TaskKey) {
        self.schedule_events.push_removed(key);
    }

    fn emit_wake(&mut self, wake: WakeKey) {
        self.schedule_events.push_wake(wake);
    }

    pub(crate) fn prepare_scheduler_procs(
        &mut self,
        machine_slots: usize,
        added: usize,
    ) -> Result<(), FaultCode> {
        self.scheduler_procs.prepare_batch(machine_slots, added)
    }

    fn prepare_scheduler_proc(&mut self, vm: VmId) -> Result<(), FaultCode> {
        self.scheduler_procs.prepare(vm)
    }

    pub(crate) fn activate_scheduler_proc_prepared(&mut self, vm: VmId) {
        let key = TaskKey {
            vm,
            generation: self.machines[vm as usize].generation,
        };
        self.scheduler_procs.insert_prepared(key);
        if self.task_status(key) == TaskStatus::Ready {
            self.emit_ready(key);
        }
    }

    /// Retire one scheduler-owned proc.
    ///
    /// This batch can still hold an earlier ready event for the same
    /// task. The scheduler drains removals first and ready events
    /// last, and it answers a ready event by reading the live task
    /// status. A retired proc reports `Dormant` there, so the
    /// scheduler drops it again. The stale event needs no removal.
    fn deactivate_scheduler_proc(&mut self, key: TaskKey) {
        if self.scheduler_procs.remove(key) {
            self.emit_removed(key);
        }
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
        self.suspended
            .get(&vm)
            .map(|saved| saved.activations.len())
            .unwrap_or(0)
    }

    /// The execution references of one machine that stored stacks hold.
    ///
    /// `push_activation` charges one reference to the machine of an
    /// activation and one to the machine that receives its exit event.
    /// A stored stack keeps both, so a copy compares the total of one
    /// machine against this count. A larger total means a live stack
    /// still executes the machine.
    pub fn suspended_refs(&self, vm: VmId) -> usize {
        let mut count = 0;
        for saved in self.suspended.values() {
            for act in &saved.activations {
                if act.vm == vm {
                    count += 1;
                }
                if act.reply_to == Some(vm) {
                    count += 1;
                }
            }
        }
        count
    }

    /// Drop the suspended activation stack of one machine.
    ///
    /// A restored world holds no driver stack, so a snapshot that
    /// copied a blocked machine restores it with none. The scheduler
    /// builds a fresh activation when the block clears.
    pub fn drop_suspended(&mut self, vm: VmId) {
        self.suspended.remove(&vm);
    }

    /// Limit a slice to the smallest active snapshot fuel budget.
    pub fn snapshot_wait_quantum(&mut self, task: TaskKey, requested: u32) -> u32 {
        let watchers = self.snapshot_watchers();
        let mut quantum = requested.max(1);
        for (_, target, generation, remaining, retry) in watchers {
            if retry || remaining == 0 || !self.proc_alive(target, generation) {
                continue;
            }
            let contains = self
                .controlled_machines(target)
                .is_ok_and(|set| set.contains(&task.vm));
            if contains {
                let cap = remaining.min(u64::from(u32::MAX)) as u32;
                quantum = quantum.min(cap.max(1));
            }
        }
        quantum
    }

    /// Record progress for every snapshot wait that contains this task.
    pub fn note_scheduler_slice(&mut self, task: TaskKey, retired: u64, changed: bool) {
        if retired == 0 && !changed {
            return;
        }
        let watchers = self.snapshot_watchers();
        let mut wakes = Vec::new();
        for (waiter, target, generation, _, retry) in watchers {
            if retry || !self.proc_alive(target, generation) {
                continue;
            }
            let contains = self
                .controlled_machines(target)
                .is_ok_and(|set| set.contains(&task.vm));
            if !contains {
                continue;
            }
            let Some(Block::Snapshot {
                remaining, retry, ..
            }) = self.machines[waiter as usize].vm.block.as_mut()
            else {
                continue;
            };
            *remaining = remaining.saturating_sub(retired);
            *retry = true;
            wakes.push(WakeKey::Snapshot(TaskKey {
                vm: target,
                generation,
            }));
        }
        for wake in wakes {
            self.emit_wake(wake);
        }
    }

    fn snapshot_watchers(&self) -> Vec<(VmId, VmId, u32, u64, bool)> {
        self.machines
            .iter()
            .enumerate()
            .filter_map(|(vm, machine)| match machine.vm.block {
                Some(Block::Snapshot {
                    target,
                    generation,
                    remaining,
                    retry,
                }) => Some((vm as VmId, target, generation, remaining, retry)),
                _ => None,
            })
            .collect()
    }

    /// Drive one task for at most `quantum` guest instructions.
    pub fn drive_slice(&mut self, key: TaskKey, quantum: u32) -> Option<SliceExit> {
        match self.task_status(key) {
            TaskStatus::Dormant => return None,
            TaskStatus::Terminal => return Some(SliceExit::Terminal),
            TaskStatus::Blocked(wake) => return Some(SliceExit::Blocked(wake)),
            TaskStatus::Waiting(completion) => return Some(SliceExit::Waiting(completion)),
            TaskStatus::Parked(wait) => return Some(SliceExit::Parked(wait)),
            TaskStatus::Ready => {}
        }
        if let Some(wait) = self
            .suspended
            .get(&key.vm)
            .and_then(|saved| match saved.reason {
                SuspendReason::Parked { wait, .. } => Some(wait),
                _ => None,
            })
        {
            return Some(self.service_wait_slice(key.vm, wait, quantum));
        }
        if self.machines[key.vm as usize].vm.state == MachineState::Blocked
            && !self.suspended.contains_key(&key.vm)
        {
            self.complete_blocked_machine(key.vm);
        }
        let event = if self.suspended.contains_key(&key.vm) {
            self.resume_stack_with_quantum(key.vm, Some(quantum.max(1)))
        } else if self.machines[key.vm as usize].vm.state == MachineState::Ready {
            let mut stack = Vec::new();
            self.push_activation(
                &mut stack,
                Activation {
                    vm: key.vm,
                    mode: StopMode::RunToTerminal,
                    family: Family::Run,
                    reply_to: None,
                    retired: false,
                    fuel: None,
                },
            );
            self.drive_stack(&mut stack, Some(quantum.max(1)))
        } else {
            self.fault_event(key.vm, "the scheduler task is not ready to run")
        };
        match event {
            RootEvent::Blocked => {
                self.suspended
                    .get(&key.vm)
                    .and_then(|saved| match saved.reason {
                        SuspendReason::Blocked { wake, .. } => Some(SliceExit::Blocked(wake)),
                        SuspendReason::Parked { wait, .. } => Some(SliceExit::Parked(wait)),
                        _ => None,
                    })
            }
            RootEvent::Waiting => self.suspended.get(&key.vm).and_then(|saved| {
                if let SuspendReason::Waiting { completion, .. } = saved.reason {
                    Some(SliceExit::Waiting(completion))
                } else {
                    None
                }
            }),
            RootEvent::Ran => Some(SliceExit::Yielded),
            RootEvent::Done(_) | RootEvent::Fault(_) => {
                if key.vm != 0 && self.scheduler_procs.contains(key) {
                    let faulted = self.machines[key.vm as usize].vm.state == MachineState::Faulted;
                    self.record(TraceEvent::Terminal {
                        proc: key.vm,
                        faulted,
                    });
                }
                Some(SliceExit::Terminal)
            }
            _ => {
                self.machines[key.vm as usize].set_fault(
                    FaultCode::MalformedState,
                    "the scheduler slice stopped outside its stop set",
                    None,
                );
                Some(SliceExit::Terminal)
            }
        }
    }

    /// Remove one terminal proc from the active scheduler index.
    pub fn retire_scheduler_task(&mut self, key: TaskKey) {
        if key.vm != 0 {
            self.deactivate_scheduler_proc(key);
        }
    }

    /// The stored outcome of one terminal task.
    pub fn task_outcome(&self, key: TaskKey) -> Outcome {
        match self.terminal_root_event(key.vm) {
            RootEvent::Done(value) => Outcome::Done(value),
            RootEvent::Fault(record) => Outcome::Fault(record.code),
            _ => Outcome::Fault(FaultCode::MalformedState),
        }
    }

    /// Fault the machine that blocks one saved scheduler task.
    pub fn fail_blocked_task(&mut self, key: TaskKey, message: &str) {
        let blocked = self
            .suspended
            .get(&key.vm)
            .and_then(|saved| match saved.reason {
                SuspendReason::Blocked { machine, .. } => Some(machine),
                SuspendReason::Parked { machine, .. } => Some(machine),
                _ => None,
            });
        let vm = blocked.unwrap_or(key.vm);
        let op = self.pending_op(vm);
        self.machines[vm as usize].vm.block = None;
        self.machines[vm as usize].set_fault(FaultCode::HostFault, message, op);
        if let Some(saved) = self.suspended.get_mut(&key.vm) {
            saved.reason = SuspendReason::Yielded;
        }
    }

    /// Fault the machine that waits inside one saved scheduler task.
    pub fn fail_waiting_task(&mut self, key: TaskKey, message: &str) {
        let waiting = self
            .suspended
            .get(&key.vm)
            .and_then(|saved| match saved.reason {
                SuspendReason::Waiting { machine, .. } => Some(machine),
                SuspendReason::Parked { machine, .. } => Some(machine),
                _ => None,
            });
        let vm = waiting.unwrap_or(key.vm);
        let op = self.pending_op(vm);
        self.machines[vm as usize].set_fault(FaultCode::HostFault, message, op);
        if let Some(saved) = self.suspended.get_mut(&key.vm) {
            saved.reason = SuspendReason::Yielded;
        }
    }

    /// The ready proc keys for tools and transition tests.
    ///
    /// The call reads the active index. It never scans terminal
    /// machine records.
    pub fn runnable_procs(&self) -> Vec<VmId> {
        let mut ready: Vec<VmId> = self
            .scheduler_procs
            .entries()
            .iter()
            .copied()
            .filter(|key| self.task_status(*key) == TaskStatus::Ready)
            .map(|key| key.vm)
            .collect();
        ready.sort_unstable();
        ready
    }

    /// Complete indexed blocks that became ready.
    ///
    /// Scheduler production uses wake keys. This entry supports tools
    /// that drive proc transitions directly.
    pub fn poll_blocked(&mut self) -> usize {
        let mut keys: Vec<TaskKey> = self.scheduler_procs.entries().to_vec();
        if let Some(root) = self.task_key(0) {
            keys.push(root);
        }
        keys.sort_unstable();
        let mut released = 0;
        for key in keys {
            let blocked = self
                .suspended
                .get(&key.vm)
                .and_then(|saved| match saved.reason {
                    SuspendReason::Blocked { machine, .. } => Some(machine),
                    SuspendReason::Parked { machine, .. } => Some(machine),
                    _ => None,
                });
            let vm = blocked.unwrap_or(key.vm);
            if self
                .machines
                .get(vm as usize)
                .is_some_and(|machine| machine.vm.state == MachineState::Blocked)
                && self.block_ready(vm)
            {
                self.complete_blocked_machine(vm);
                if let Some(saved) = self.suspended.get_mut(&key.vm) {
                    saved.reason = SuspendReason::Yielded;
                }
                released += 1;
            }
        }
        released
    }

    /// Drive one proc with the compatibility quantum.
    pub fn drive_proc(&mut self, vm: VmId) -> SliceExit {
        let Some(key) = self.task_key(vm) else {
            return SliceExit::Terminal;
        };
        let exit = self
            .drive_slice(key, u32::MAX)
            .unwrap_or(SliceExit::Terminal);
        if exit == SliceExit::Terminal {
            self.retire_scheduler_task(key);
        }
        exit
    }

    /// The blocked indexed task bases in stable order.
    pub fn blocked_machines(&self) -> Vec<VmId> {
        let mut blocked: Vec<VmId> = self
            .scheduler_seeds(true)
            .into_iter()
            .filter_map(|(key, status)| matches!(status, TaskStatus::Blocked(_)).then_some(key.vm))
            .collect();
        blocked.sort_unstable();
        blocked
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
    ///
    /// A held machine reports `Dormant`, so an earlier ready event in
    /// this batch is harmless. `deactivate_scheduler_proc` states the
    /// rule.
    pub fn set_barrier(&mut self, vm: VmId, barrier: Option<u32>) {
        self.machines[vm as usize].barrier = barrier;
        let Some(key) = self.task_key(vm) else {
            return;
        };
        if !self.scheduler_procs.contains(key) && vm != 0 {
            return;
        }
        if barrier.is_some() {
            self.emit_removed(key);
        } else {
            self.emit_ready(key);
        }
    }

    /// Freeze or thaw mailbox acceptance of one machine.
    ///
    /// A frozen mailbox accepts no message. A send that reaches one
    /// blocks the sender, so the accepted queue at the cut is exactly
    /// what a snapshot would copy.
    pub fn freeze_mailbox(&mut self, vm: VmId, frozen: bool) {
        self.machines[vm as usize].vm.mailbox.frozen = frozen;
        if !frozen {
            if let Some(key) = self.task_key(vm) {
                self.emit_wake(WakeKey::Send(key));
            }
        }
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

    /// The latest world gate marker.
    pub(crate) fn gate_marker(&self) -> u32 {
        self.gate
    }

    /// Commit one prepared world gate marker.
    pub(crate) fn set_gate_marker(&mut self, gate: u32) {
        self.gate = gate;
    }

    /// Record that one restore committed a machine into this world.
    pub(crate) fn mark_restored(&mut self) {
        self.restored_any = true;
    }

    /// True after one restore committed a machine into this world.
    ///
    /// A test reads it to state that a restore turns the boundary
    /// check on.
    pub fn restored_any(&self) -> bool {
        self.restored_any
    }

    /// Reserve one restored gate record before restore commit.
    pub(crate) fn prepare_gate_group(&mut self) -> Result<(), FaultCode> {
        self.gate_groups
            .try_reserve(1)
            .map_err(|_| FaultCode::HostFault)
    }

    /// Install one prepared restored gate record.
    pub(crate) fn install_gate_group(&mut self, id: u32, members: Vec<VmId>) {
        self.gate_groups.push(GateGroup { id, members });
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
        let Some(at) = self.gate_groups.iter().position(|group| group.id == gate) else {
            self.machines[vm as usize].gate = 0;
            if let Some(key) = self.task_key(vm) {
                self.emit_ready(key);
            }
            return;
        };
        let group = self.gate_groups.swap_remove(at);
        for member in group.members {
            if self
                .machines
                .get(member as usize)
                .is_none_or(|machine| machine.gate != gate)
            {
                continue;
            }
            self.machines[member as usize].gate = 0;
            if let Some(key) = self.task_key(member) {
                if member == 0 || self.scheduler_procs.contains(key) {
                    self.emit_ready(key);
                }
            }
        }
    }

    /// The number of whole-image structural checks this world ran.
    pub fn snapshot_checks(&self) -> u64 {
        self.checks
    }

    /// Record one whole-image structural check.
    pub(crate) fn record_snapshot_check(&mut self) {
        self.checks = self.checks.saturating_add(1);
    }

    /// Remember one admitted image of this world.
    pub fn trust_image(&mut self, image: &crate::snapshot::SnapshotImage) {
        let hash = image.hash();
        if self.trusted.iter().any(|(held, _, _)| *held == hash) {
            return;
        }
        let bytes = image.resident_bytes();
        let limit = self.budget.limits.max_cached_image_bytes;
        if bytes > limit {
            return;
        }
        while self
            .trusted_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > limit)
        {
            let Some((_, _, removed)) = self.trusted.pop() else {
                break;
            };
            self.trusted_bytes = self.trusted_bytes.saturating_sub(removed);
        }
        self.trusted.insert(0, (hash, image.clone(), bytes));
        self.trusted_bytes += bytes;
    }

    /// The admitted image with this container hash.
    fn trusted_image(&self, hash: &[u8; 32]) -> Option<crate::snapshot::SnapshotImage> {
        self.trusted
            .iter()
            .find(|(held, _, _)| held == hash)
            .map(|(_, image, _)| image.clone())
    }

    /// Install one external snapshot container into this world.
    ///
    /// This is the external byte path of specification 17.8. It
    /// decodes and admits the bytes once and remembers the admitted
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
    /// Five native object shapes name a machine. The walk reports all
    /// five shapes.
    ///
    /// A nested edge and a routed request also name machines. The walk
    /// reports both records.
    ///
    /// The walk starts at the snapshot roots, which cover the frame
    /// closures, the locals, the operands, the pending arguments, the
    /// terminal result, the accepted mailbox queue, the proc body, and
    /// the interned literals. It excludes the policy table, because
    /// specification 17.2 excludes policy tables from a snapshot. A
    /// machine that only a table-held mock closure names is therefore
    /// not part of the world.
    ///
    /// Heap references use canonical object order. The nested edge and
    /// routed target follow in that order.
    ///
    /// The image ordinals read this order. They never depend on a
    /// scheduler identifier.
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
                Object::NativeResourceHandle { surface, .. } => Some(*surface),
                _ => None,
            };
            if let Some(target) = target {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        for target in [
            self.machines[vm as usize].vm.nested,
            self.machines[vm as usize]
                .vm
                .routed
                .map(|route| route.target),
        ]
        .into_iter()
        .flatten()
        {
            if !out.contains(&target) {
                out.push(target);
            }
        }
        for entry in self.machines[vm as usize].vm.waits.values() {
            if let crate::machine::WaitSource::Drive { target } = entry.source {
                if !out.contains(&target) {
                    out.push(target);
                }
            }
        }
        if let Some(Block::Snapshot { target, .. }) = self.machines[vm as usize].vm.block {
            if !out.contains(&target) {
                out.push(target);
            }
        }
        Ok(out)
    }

    /// The slot generation of one machine.
    pub fn generation_of(&self, vm: VmId) -> u32 {
        self.machines[vm as usize].generation
    }

    /// Split access to two distinct machines.
    ///
    /// `transfer` routes an equal pair to the one-heap copy, so this
    /// call always receives two machines.
    fn two(&mut self, a: VmId, b: VmId) -> (&mut Machine, &mut Machine) {
        debug_assert_ne!(a, b, "a boundary transfer needs two machines");
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
        let order = self.snapshot_object_order(vm)?;
        let heap = &self.machines[vm as usize].vm.heap;
        for reference in &order {
            let resource = match heap.get(*reference) {
                Object::NativeFileHandle { resource }
                | Object::NativeResourceHandle { resource, .. }
                | Object::NativeTcpStream { resource }
                | Object::NativeTcpListener { resource } => Some(*resource),
                _ => None,
            };
            if resource.is_some_and(|resource| self.bound_resources.contains_key(&resource)) {
                return Err(FaultCode::BoundaryViolation);
            }
        }
        Ok(order.len())
    }

    fn snapshot_object_order(&mut self, vm: VmId) -> Result<Vec<ObjRef>, FaultCode> {
        let roots = self.machines[vm as usize].snapshot_roots();
        let limits = self.machines[vm as usize].config.graph;
        let machine = &mut self.machines[vm as usize];
        lm_graph::snapshot_ordinals(&mut machine.vm.heap, &roots, &limits)
    }

    /// The kind name of one live host attachment this machine holds.
    pub fn live_attachment_kind(&mut self, vm: VmId) -> Option<String> {
        let registered = self.machines[vm as usize]
            .resources
            .live_attachment()
            .map(|record| match record.kind {
                crate::ResourceKind::PendingOperation => {
                    format!("a pending {}", lm_abi::op_name(record.op))
                }
                crate::ResourceKind::File => "file handle".to_string(),
                crate::ResourceKind::TcpStream => "TCP stream".to_string(),
                crate::ResourceKind::TcpListener => "TCP listener".to_string(),
            });
        if registered.is_some() {
            return registered;
        }
        let order = self.snapshot_object_order(vm).ok()?;
        let heap = &self.machines[vm as usize].vm.heap;
        order.into_iter().find_map(|reference| {
            let resource = match heap.get(reference) {
                Object::NativeFileHandle { resource }
                | Object::NativeResourceHandle { resource, .. }
                | Object::NativeTcpStream { resource }
                | Object::NativeTcpListener { resource } => *resource,
                _ => return None,
            };
            self.bound_resources
                .get(&resource)
                .map(|bound| match bound.kind {
                    crate::ResourceKind::File => "file handle".to_string(),
                    crate::ResourceKind::TcpStream => "TCP stream".to_string(),
                    crate::ResourceKind::TcpListener => "TCP listener".to_string(),
                    crate::ResourceKind::PendingOperation => "pending operation".to_string(),
                })
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
        if let Some(result) = scalar_copy(value) {
            return result;
        }
        // A restored world can name one machine on both sides of a
        // crossing. The rule is the same rule, so the call runs the
        // one-heap copy there and never splits one machine in two.
        if src == dst {
            return self.boundary_copy(src, dst, value);
        }
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

/// Copy one value that needs no heap traversal.
#[inline]
fn scalar_copy(value: Value) -> Option<Result<Value, FaultCode>> {
    match value {
        Value::Unit | Value::Bool(_) | Value::Int(_) | Value::Char(_) | Value::Op(_) => {
            Some(Ok(value))
        }
        Value::Uninit => Some(Err(FaultCode::BoundaryViolation)),
        Value::Obj(_) => None,
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

/// The stored argument list of one pending request.
///
/// A restored machine states its own list, so a kernel rule must read
/// a position the list may not hold. The index answers the
/// uninitialized marker there, and every shape test of a kernel rule
/// rejects that marker.
#[derive(Clone, Copy)]
struct Args<'a>(&'a [Value]);

impl std::ops::Index<usize> for Args<'_> {
    type Output = Value;

    fn index(&self, at: usize) -> &Value {
        self.0.get(at).unwrap_or(&Value::Uninit)
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
    /// The table owner has an active manual driver.
    Driver {
        surface: VmId,
        cursor: PolicyCursor,
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
            Value::Char(value) => format!("{value:?}"),
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
                    Object::Substring(text) => render_string(text),
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
                    Object::Instance { class, fields, .. } => {
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
                    Object::StrBuilder(buf) => match buf.byte_len() {
                        Some(len) => format!("<StringBuilder length {len}>"),
                        None => "<finished StringBuilder>".to_string(),
                    },
                    Object::ByteBuf(bytes) => match bytes.len() {
                        Some(len) => format!("<ByteBuffer length {len}>"),
                        None => "<finished ByteBuffer>".to_string(),
                    },
                    Object::Bytes(bytes) => format!("<Bytes len {}>", bytes.len()),
                    Object::NativeFileHandle { resource } => {
                        if *resource == 0 {
                            "<file closed>".to_string()
                        } else {
                            format!("<file {resource}>")
                        }
                    }
                    Object::NativeResourceHandle { surface, resource } => {
                        format!("<resource {resource} of machine {surface}>")
                    }
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
                    Object::NativeWait { owner, token } => {
                        format!("<wait {token} of machine {owner}>")
                    }
                    Object::NativeTcpStream { resource } => {
                        if *resource == 0 {
                            "<TCP stream closed>".to_string()
                        } else {
                            format!("<TCP stream {resource}>")
                        }
                    }
                    Object::NativeTcpListener { resource } => {
                        if *resource == 0 {
                            "<TCP listener closed>".to_string()
                        } else {
                            format!("<TCP listener {resource}>")
                        }
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
        TraceEvent::Block { vm, kind, target } => {
            let what = match kind {
                TraceBlock::Receive => "receive".to_string(),
                TraceBlock::Send => format!("send target {target}"),
                TraceBlock::Done => format!("done target {target}"),
                TraceBlock::Wait => "wait".to_string(),
                TraceBlock::Snapshot => format!("snapshot target {target}"),
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
    use crate::{load, VmConfig, WorldLimits};
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
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_RUN, DispatchMode::Continue);
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
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_ANSWER, DispatchMode::Continue);
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
        world.kernel_exec(
            &mut stack,
            0,
            lm_abi::OP_VM_DISPATCH,
            DispatchMode::Continue,
        );
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

    #[test]
    fn scalar_boundary_copies_need_no_heap_graph() {
        assert_eq!(scalar_copy(Value::Int(7)), Some(Ok(Value::Int(7))));
        assert_eq!(
            scalar_copy(Value::Uninit),
            Some(Err(FaultCode::BoundaryViolation))
        );
        assert_eq!(
            scalar_copy(Value::Obj(ObjRef {
                slot: 3,
                generation: 1,
            })),
            None
        );
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
        assert!(world.share_heap_budget());
        let mock = world.empty_machine(VmConfig::default(), None, 0);
        world.machines.push(mock);
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
        assert!(world.share_heap_budget());
        let mock = world.empty_machine(VmConfig::default(), None, 0);
        world.machines.push(mock);
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

    #[test]
    fn the_world_machine_limit_bounds_sibling_growth() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            max_machines: 2,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        assert!(world.new_child(0).is_some());
        assert!(world.new_child(0).is_none());
        assert_eq!(world.machine_count(), 2);
        assert_eq!(world.child_count(0), 1);
    }

    #[test]
    fn two_machine_heaps_share_one_world_limit() {
        let loaded = trivial_loaded();
        let object = Object::Str("one".into());
        let limits = WorldLimits {
            max_heap_bytes: object.cost(),
            max_heap_objects: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        world.machines[0]
            .alloc(object.clone())
            .expect("the first heap charge fits");
        let child = world.new_child(0).expect("the child record fits");
        assert_eq!(
            world.machines[child as usize].alloc(object),
            Err(FaultCode::HeapLimit)
        );
        assert_eq!(world.world_heap_objects(), 1);
    }

    #[test]
    fn a_second_machine_attaches_the_aggregate_heap_ledger() {
        let loaded = trivial_loaded();
        let object = Object::Str("one".into());
        let bytes = object.cost();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));

        assert!(!world.heap_shared);
        world.machines[0]
            .alloc(object.clone())
            .expect("the root allocation fits");
        assert_eq!(world.world_heap_bytes(), bytes);
        assert_eq!(world.world_heap_objects(), 1);

        let child = world.new_child(0).expect("the child record fits");
        assert!(world.heap_shared);
        assert_eq!(world.world_heap_bytes(), bytes);
        assert_eq!(world.world_heap_objects(), 1);

        world.machines[child as usize]
            .alloc(object)
            .expect("the child allocation fits");
        assert_eq!(world.world_heap_bytes(), bytes * 2);
        assert_eq!(world.world_heap_objects(), 2);
    }

    #[test]
    fn all_machines_share_one_instruction_budget() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            fuel: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        assert_eq!(world.run_root(), Outcome::Fault(FaultCode::OutOfFuel));
        assert_eq!(world.world_fuel(), 0);
    }

    #[test]
    fn the_proc_trace_stops_at_its_world_limit() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            max_trace_events: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        world.trace_procs();
        world.record(TraceEvent::Pause { proc: 0 });
        world.record(TraceEvent::Resume { proc: 0 });
        assert_eq!(world.trace(), &[TraceEvent::Pause { proc: 0 }]);
    }

    #[test]
    fn a_failed_restore_reply_leaves_no_handle_object() {
        let loaded = trivial_loaded();
        let handle_bytes = Object::NativeVm { vm: 1 }.cost();
        let config = VmConfig {
            heap_bytes: handle_bytes,
            ..VmConfig::default()
        };
        let mut world = World::new(&loaded, config, Box::new(NullHost));
        world.core.result_ok = Some(0);
        assert!(matches!(
            world.prepare_restore_reply(0, 1),
            Err(FaultCode::HeapLimit)
        ));
        assert_eq!(world.machines[0].vm.heap.live_count(), 0);
        assert_eq!(world.world_heap_bytes(), 0);
    }
}
