//! The scheduler-facing surface.
//!
//! One part of the `World` surface. `world/mod.rs` holds the
//! state these methods read.

use super::*;

impl World {
    /// True when the block of `vm` can complete now.
    pub(super) fn block_ready(&self, vm: VmId) -> bool {
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
    pub(super) fn block_wake_key(&self, vm: VmId) -> Option<WakeKey> {
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
    pub(super) fn complete_blocked_machine(&mut self, vm: VmId) {
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

    pub(super) fn release_saved_stack(&mut self, base: VmId) {
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

    pub(super) fn quiesce_wait_leases(&mut self, vm: VmId, token: u64, seen: &mut Vec<VmId>) {
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

    pub(super) fn mark_wait_stack_ready(&mut self, base: VmId, wait: WaitSetKey) {
        if let Some(saved) = self.suspended.get_mut(&base) {
            if matches!(saved.reason, SuspendReason::Parked { wait: held, .. } if held == wait) {
                saved.reason = SuspendReason::Yielded;
            }
        }
    }

    pub(super) fn service_wait_slice(
        &mut self,
        base: VmId,
        wait: WaitSetKey,
        quantum: u32,
    ) -> SliceExit {
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

    pub(super) fn drive_holder_slice(&mut self, target: VmId, quantum: u32) -> SliceExit {
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
                .or_else(|| self.block_wake_key(target).map(SliceExit::Blocked))
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

    pub(super) fn wait_set_status(&self, wait: WaitSetKey) -> TaskStatus {
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
    pub(super) fn append_wait_sources(
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

    pub(super) fn append_drive_sources(
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

    pub(super) fn emit_ready(&mut self, key: TaskKey) {
        self.schedule_events.push_ready(key);
    }

    pub(super) fn emit_removed(&mut self, key: TaskKey) {
        self.schedule_events.push_removed(key);
    }

    pub(super) fn emit_wake(&mut self, wake: WakeKey) {
        self.schedule_events.push_wake(wake);
    }

    pub(crate) fn prepare_scheduler_procs(
        &mut self,
        machine_slots: usize,
        added: usize,
    ) -> Result<(), FaultCode> {
        self.scheduler_procs.prepare_batch(machine_slots, added)
    }

    pub(super) fn prepare_scheduler_proc(&mut self, vm: VmId) -> Result<(), FaultCode> {
        self.scheduler_procs.prepare(vm)
    }

    pub(crate) fn activate_scheduler_proc_prepared(&mut self, vm: VmId) {
        let key = TaskKey {
            vm,
            generation: self.machines[vm as usize].generation,
        };
        self.scheduler_procs.insert_prepared(key);
        // The scheduler uses this event to rebuild every task index.
        // A resumed blocked proc must register its wake condition again.
        self.emit_ready(key);
    }

    /// Retire one scheduler-owned proc.
    ///
    /// This batch can still hold an earlier ready event for the same
    /// task. The scheduler drains removals first and ready events
    /// last, and it answers a ready event by reading the live task
    /// status. A retired proc reports `Dormant` there, so the
    /// scheduler drops it again. The stale event needs no removal.
    pub(super) fn deactivate_scheduler_proc(&mut self, key: TaskKey) {
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

    pub(super) fn snapshot_watchers(&self) -> Vec<(VmId, VmId, u32, u64, bool)> {
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
            // A completed operation can install another block.
            // Report that block before a new activation starts.
            match self.task_status(key) {
                TaskStatus::Dormant => return None,
                TaskStatus::Terminal => return Some(SliceExit::Terminal),
                TaskStatus::Blocked(wake) => return Some(SliceExit::Blocked(wake)),
                TaskStatus::Waiting(completion) => return Some(SliceExit::Waiting(completion)),
                TaskStatus::Parked(wait) => return Some(SliceExit::Parked(wait)),
                TaskStatus::Ready => {}
            }
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
            RootEvent::Blocked => self
                .suspended
                .get(&key.vm)
                .and_then(|saved| match saved.reason {
                    SuspendReason::Blocked { wake, .. } => Some(SliceExit::Blocked(wake)),
                    SuspendReason::Parked { wait, .. } => Some(SliceExit::Parked(wait)),
                    _ => None,
                })
                .or_else(|| self.block_wake_key(key.vm).map(SliceExit::Blocked)),
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
}
