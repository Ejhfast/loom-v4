//! World construction, root driving, and the activation stack.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_WORLD_ID: AtomicU64 = AtomicU64::new(1);

fn next_world_id() -> u64 {
    let id = NEXT_WORLD_ID.fetch_add(1, Ordering::Relaxed);
    assert_ne!(id, 0, "the world identity space is not exhausted");
    id
}

fn execution_fault_message(code: FaultCode) -> &'static str {
    match code {
        FaultCode::MutableMapKey => {
            "freeze the key before insertion, or declare a suitable `frozen class`"
        }
        _ => "",
    }
}

impl World {
    /// Create a world with the entry loaded into the root machine.
    pub fn new(loaded: &LoadedModule, config: VmConfig, host: Box<dyn Host>) -> World {
        World::new_with_limits(loaded, config, WorldLimits::default(), host)
    }

    /// Create a world with exact aggregate limits.
    pub fn new_with_limits(
        loaded: &LoadedModule,
        config: VmConfig,
        limits: WorldLimits,
        host: Box<dyn Host>,
    ) -> World {
        let module = loaded.module_store();
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
        root.table.set_bundle(loaded.bundle().clone());
        root.load_frame(
            &module,
            module.entry,
            Vec::new(),
            None,
            lm_value::TypeEnvId::EMPTY,
        );
        let dispatch = loaded.dispatch_store();
        let execution_code = Arc::new(crate::executor::ExecutionCode::new(
            module.clone(),
            dispatch.clone(),
        ));
        World {
            world_id: next_world_id(),
            base_loaded: loaded.clone(),
            loaded: loaded.clone(),
            module,
            dispatch,
            execution_code,
            core: loaded.core_layout(),
            base_slot_count: loaded.module().slots.len(),
            installations: Vec::new(),
            machines: vec![root.into()],
            vm_images: Vec::new(),
            vm_image_free: Vec::new(),
            mock_free: Vec::new(),
            vm_free: Vec::new(),
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
            trusted: std::collections::VecDeque::new(),
            trusted_index: std::collections::HashMap::new(),
            trusted_bytes: 0,
            images: Vec::new(),
            image_free: Vec::new(),
            last_image: None,
            check: crate::typecheck::BoundaryScratch::default(),
            metrics: WorldMetrics::default(),
            poisoned: false,
        }
    }

    /// The current scheduler measurement counters.
    pub fn metrics(&self) -> WorldMetrics {
        self.metrics
    }

    /// Reset the scheduler measurement counters.
    pub fn reset_metrics(&mut self) {
        self.metrics = WorldMetrics::default();
        self.envs.reset_metrics();
    }

    /// The current closed-type counters.
    pub fn type_metrics(&self) -> crate::TypeEnvMetrics {
        self.envs.metrics()
    }

    /// Add the clock-free counters from every machine.
    pub fn execution_metrics(&self) -> crate::MachineExecutionMetrics {
        let mut total = crate::MachineExecutionMetrics::default();
        for machine in &self.machines {
            let metrics = machine.execution_metrics();
            total.native_calls = total.native_calls.saturating_add(metrics.native_calls);
            total.collections = total.collections.saturating_add(metrics.collections);
        }
        total
    }

    /// The aggregate live heap bytes in this world.
    pub fn aggregate_heap_bytes(&self) -> usize {
        self.budget.heap.used_bytes()
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

    pub(super) fn record(&mut self, event: TraceEvent) {
        if let Some(trace) = &mut self.trace {
            if trace.len() < self.budget.limits.max_trace_events {
                trace.push(event);
            }
        }
    }

    /// Grant one root policy target by name: an exact operation such
    /// as `Io.Write`, or a whole group such as `Clock`.
    pub fn allow(&mut self, name: &str) -> Result<(), String> {
        let bundle = self.loaded.bundle();
        let table = &mut self.machines[0].table;
        if let Some(op) = bundle.op_by_name(name) {
            table.set_exact(op, Some(Action::Pass));
            return Ok(());
        }
        if let Some(group) = bundle.group_by_name(name) {
            table.set_group(group, Some(Action::Pass));
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
        self.budget.fuel.remaining()
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
    pub(super) fn resume_stack(&mut self, vm: VmId) -> RootEvent {
        self.resume_stack_with_quantum(vm, None)
    }

    /// Resume one saved stack under an optional scheduler quantum.
    pub(super) fn resume_stack_with_quantum(
        &mut self,
        vm: VmId,
        quantum: Option<u32>,
    ) -> RootEvent {
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
    pub(super) fn fault_event(&mut self, vm: VmId, message: &str) -> RootEvent {
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
            let before = self.budget.fuel.remaining();
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
                let retired = before.saturating_sub(self.budget.fuel.remaining());
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
            let retired = before.saturating_sub(self.budget.fuel.remaining());
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
        Some(self.install_child(config, parent))
    }

    /// Install one new child record, in a free slot when one exists.
    ///
    /// A reclaimed slot keeps the generation the collector left, so a
    /// key minted for the freed record names a dead machine and never
    /// the new one.
    pub(crate) fn install_child(&mut self, config: VmConfig, parent: VmId) -> VmId {
        match self.vm_free.pop() {
            Some(id) => {
                let generation = self.machines[id as usize].generation;
                self.machines[id as usize] =
                    self.empty_machine(config, Some(parent), generation).into();
                id
            }
            None => {
                let id = self.machines.len() as VmId;
                let machine = self.empty_machine(config, Some(parent), 0);
                self.machines.push(machine.into());
                id
            }
        }
    }

    /// The table grants that the declared row of one function names.
    ///
    /// A row entry is text: either one exact operation name or one
    /// group name. The launch paths pass exactly this row to a new
    /// machine, and each caller charges the same row, so a launch
    /// creates no authority.
    pub(super) fn declared_grants(&self, func: u32) -> (Vec<u32>, Vec<u32>) {
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
            if let Some(op) = self.loaded.bundle().op_by_name(text) {
                ops.push(op);
            } else if let Some(group) = self.loaded.bundle().group_by_name(text) {
                groups.push(group);
            }
        }
        (ops, groups)
    }

    /// Grant one root policy target to one machine, for tools.
    pub fn allow_on(&mut self, vm: VmId, name: &str) -> Result<(), String> {
        let bundle = self.loaded.bundle();
        let table = &mut self.machines[vm as usize].table;
        if let Some(op) = bundle.op_by_name(name) {
            table.set_exact(op, Some(Action::Pass));
            return Ok(());
        }
        if let Some(group) = bundle.group_by_name(name) {
            table.set_group(group, Some(Action::Pass));
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
        self.machines[vm as usize].table.entry_count()
    }

    /// True when the table of one machine passes one group by name.
    pub fn table_passes_group(&self, vm: VmId, name: &str) -> bool {
        let Some(group) = self.loaded.bundle().group_by_name(name) else {
            return false;
        };
        matches!(
            self.machines[vm as usize].table.group_action(group),
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
        match &self.machines.get(vm as usize)?.vm.terminal {
            Some(Terminal::Fault(rec)) => Some(rec),
            _ => None,
        }
    }

    /// Drive one machine with one stop mode. The public entry for the
    /// world caller; guest holders drive through `Vm.*` performs.
    pub(super) fn control(&mut self, vm: VmId, mode: StopMode, family: Family) -> RootEvent {
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
    pub(super) fn terminal_root_event(&self, vm: VmId) -> RootEvent {
        match &self.machines[vm as usize].vm.terminal {
            Some(Terminal::Done(value)) => RootEvent::Done(*value),
            Some(Terminal::Fault(rec)) => RootEvent::Fault(rec.clone()),
            None => RootEvent::Fault(FaultRec {
                code: FaultCode::MalformedState,
                message: "the terminal machine stores no result".to_string(),
                op: None,
                trace: Vec::new(),
            }),
        }
    }

    pub(super) fn push_activation(&mut self, stack: &mut Vec<Activation>, act: Activation) {
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
    pub(super) fn park_stack(&mut self, stack: &[Activation]) {
        for act in stack {
            let machine = &mut self.machines[act.vm as usize];
            if machine.vm.state == MachineState::Running {
                machine.vm.state = MachineState::Ready;
            }
        }
    }

    /// Release the execution references of one removed activation.
    pub(super) fn release_activation(&mut self, act: Activation) {
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

    fn execute_inline(&mut self, vm: VmId, limit: u32) -> crate::executor::InlineExecutionReport {
        let image = self.machines[vm as usize].image;
        let slots = image.and_then(|key| {
            self.vm_images.get(key.image as usize).and_then(|record| {
                (record.live && record.generation == key.generation)
                    .then_some(record.slots.as_slice())
            })
        });
        crate::executor::execute_inline(
            &mut self.machines[vm as usize],
            self.module.as_ref(),
            self.dispatch.as_ref(),
            &mut self.envs,
            slots,
            limit,
        )
    }

    pub(super) fn commit_execution_stop(
        &mut self,
        stack: &mut Vec<Activation>,
        top_idx: usize,
        vm: VmId,
        quantum: &mut Option<u32>,
        commit: crate::executor::ExecutionCommit,
    ) -> Option<RootEvent> {
        let crate::executor::ExecutionCommit {
            stop,
            retired,
            reached_boundary,
            charge_fuel,
        } = commit;
        if reached_boundary {
            self.metrics.boundary_exits = self.metrics.boundary_exits.saturating_add(1);
        }
        if charge_fuel {
            if let Some(fuel) = Arc::get_mut(&mut self.budget.fuel) {
                fuel.charge_unique(retired);
            } else {
                self.budget.fuel.charge(retired);
            }
        }
        if let Some(remaining) = quantum {
            *remaining = remaining.saturating_sub(retired);
        }
        stack[top_idx].retired |= retired > 0;
        for activation in stack.iter_mut() {
            if let Some(left) = &mut activation.fuel {
                *left = left.saturating_sub(retired);
            }
        }
        match stop {
            ExecutionStop::Fault(code) => {
                self.machines[vm as usize].set_fault(code, execution_fault_message(code), None);
            }
            ExecutionStop::QuantumExpired
            | ExecutionStop::Recalled
            | ExecutionStop::HeapTrip
            | ExecutionStop::Boundary(ExecOutcome::Continue) => {}
            ExecutionStop::NeedsQuiescence => {}
            ExecutionStop::Boundary(ExecOutcome::Terminal(value)) => {
                if self.machines[vm as usize].start_body.is_some() {
                    self.enter_proc_body(vm, value);
                } else {
                    self.machines[vm as usize].set_done(value);
                }
            }
            ExecutionStop::Boundary(ExecOutcome::Raise { code, message }) => {
                self.machines[vm as usize].set_fault(code, message, None);
            }
            ExecutionStop::Boundary(ExecOutcome::Perform { op, args }) => {
                return self.handle_perform(stack, vm, op, args);
            }
            ExecutionStop::Boundary(ExecOutcome::PrepareWait {
                op,
                argc,
                reply_ty,
                env,
            }) => {
                return self.handle_prepare_wait(stack, vm, op, argc, reply_ty, env);
            }
            ExecutionStop::Boundary(ExecOutcome::LoadSlot { slot }) => {
                if let Err(code) = self.load_value_slot(vm, slot) {
                    self.machines[vm as usize].set_fault(code, "", None);
                }
            }
            ExecutionStop::Boundary(ExecOutcome::TableEdit {
                table,
                action,
                kind,
                slot,
                mock,
            }) => self.handle_table_edit(vm, table, action, kind, slot, mock),
            ExecutionStop::Boundary(ExecOutcome::AsCall {
                request,
                op,
                ty,
                env,
            }) => self.handle_as_call(vm, request, op, ty, env),
            ExecutionStop::Boundary(ExecOutcome::RequestOp { request }) => {
                self.handle_request_op(vm, request)
            }
            ExecutionStop::Boundary(ExecOutcome::CallArgs { call }) => {
                self.handle_call_args(vm, call)
            }
            ExecutionStop::Boundary(ExecOutcome::Digest { value, ty, env }) => {
                self.handle_digest(vm, value, ty, env)
            }
            ExecutionStop::Boundary(ExecOutcome::DynamicRender { value, ty }) => {
                self.handle_dynamic_render(vm, value, ty)
            }
            ExecutionStop::Boundary(ExecOutcome::FunctionCode { function, origin }) => {
                self.handle_function_code(vm, function, origin)
            }
            ExecutionStop::Boundary(ExecOutcome::ClassCode { class, origin }) => {
                self.handle_class_code(vm, class, origin)
            }
        }
        None
    }

    /// Advance one activation stack to execution or a caller event.
    pub(super) fn advance_stack(
        &mut self,
        stack: &mut Vec<Activation>,
        quantum: &mut Option<u32>,
    ) -> DriverStep {
        loop {
            let Some(top_idx) = stack.len().checked_sub(1) else {
                return DriverStep::Event(RootEvent::Ran);
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
                    return DriverStep::Event(event);
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
                    return DriverStep::Event(event);
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
                        return DriverStep::Event(RootEvent::Blocked);
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
                    // A base machine keeps its continuation in its frames.
                    // Release its activation at this scheduler safe point.
                    if stack.len() == 1 {
                        let activation = stack.pop().expect("the stack has one activation");
                        self.release_activation(activation);
                        return DriverStep::Event(RootEvent::Blocked);
                    }
                    // A nested stack keeps its reply chain until its wake.
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
                    return DriverStep::Event(RootEvent::Blocked);
                }
                MachineState::Done | MachineState::Faulted => {
                    // This machine reached its terminal state inside
                    // another task's slice, so no slice exit reports
                    // it. Wake the tasks that wait on this machine
                    // here, or a `done` or a full-mailbox `send` on it
                    // never becomes runnable again.
                    self.note_terminal(act.vm);
                    if let Some(event) = self.finish(stack, ExitKind::Terminal) {
                        return DriverStep::Event(event);
                    }
                }
                MachineState::Waiting => {
                    if act.mode == StopMode::OneStep {
                        if act.retired {
                            if let Some(event) = self.finish(stack, ExitKind::Waiting) {
                                return DriverStep::Event(event);
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
                                return DriverStep::Event(event);
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
                        return DriverStep::Event(RootEvent::Waiting);
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
                        return DriverStep::Event(event);
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
                            return DriverStep::Event(event);
                        }
                        continue;
                    }
                    if matches!(*quantum, Some(0)) {
                        // A base activation keeps its continuation in
                        // machine state. Release its driver record at
                        // this scheduler safepoint.
                        if stack.len() == 1 {
                            if let Some(event) = self.finish(stack, ExitKind::Ran) {
                                return DriverStep::Event(event);
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
                        return DriverStep::Event(RootEvent::Ran);
                    }
                    if self.budget.fuel.remaining() == 0 {
                        self.machines[act.vm as usize].set_fault(FaultCode::OutOfFuel, "", None);
                        continue;
                    }
                    let requested = match *quantum {
                        Some(_) if act.mode == StopMode::OneStep => 1,
                        Some(remaining) => remaining,
                        None if act.mode == StopMode::OneStep => 1,
                        None => u32::MAX,
                    };
                    let available = self.budget.fuel.remaining().min(u64::from(u32::MAX)) as u32;
                    // A bounded drive turn caps this batch, so the turn
                    // never retires past the bound its holder named.
                    let turn = stack
                        .iter()
                        .filter_map(|a| a.fuel)
                        .min()
                        .unwrap_or(u32::MAX)
                        .max(1);
                    let limit = requested.min(available).min(turn);
                    return DriverStep::Execute {
                        top_idx,
                        vm: act.vm,
                        limit,
                    };
                }
            }
        }
    }

    /// The one synchronous driver loop over the activation stack.
    pub(super) fn drive_stack(
        &mut self,
        stack: &mut Vec<Activation>,
        mut quantum: Option<u32>,
    ) -> RootEvent {
        loop {
            match self.advance_stack(stack, &mut quantum) {
                DriverStep::Execute { top_idx, vm, limit } => {
                    let report = self.execute_inline(vm, limit);
                    if let Some(event) = self.commit_execution_stop(
                        stack,
                        top_idx,
                        vm,
                        &mut quantum,
                        report.into_commit(),
                    ) {
                        return event;
                    }
                }
                DriverStep::Event(event) => return event,
            }
        }
    }

    /// Push the child of one reified nested VM control operation.
    pub(super) fn resume_nested(&mut self, stack: &mut Vec<Activation>, parent: VmId) {
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
    pub(super) fn note_terminal(&mut self, vm: VmId) {
        let Some(key) = self.task_key(vm) else {
            return;
        };
        self.emit_wake(WakeKey::Done(key));
        self.emit_wake(WakeKey::Send(key));
    }
}
