//! Owned scheduler slices for parallel execution.
//!
//! This module moves one machine into a worker lease.
//! The coordinator keeps every activation stack and world operation.

use super::*;
use crate::executor::{
    ExecutionCommit, ExecutionLease, ExecutionLimits, ExecutionReport, ExecutionReservation,
    ExecutionToken,
};
use crate::machine::operation_needs_global_quiescence;
use std::collections::VecDeque;
use std::fmt;

/// The default byte trip point of one worker lease.
const LEASE_HEAP_BYTES: usize = 64 << 10;
/// The default object trip point of one worker lease.
const LEASE_HEAP_OBJECTS: usize = 1_024;

/// One scheduler continuation between worker jobs.
pub struct ParallelContinuation {
    task: TaskKey,
    stack: Vec<Activation>,
    quantum: Option<u32>,
}

impl ParallelContinuation {
    /// The scheduler task that owns this continuation.
    pub fn task(&self) -> TaskKey {
        self.task
    }

    /// True when this continuation holds one named machine.
    pub fn contains_machine(&self, vm: VmId) -> bool {
        self.stack.iter().any(|activation| activation.vm == vm)
    }
}

/// Coordinator state retained while one worker owns a machine.
#[must_use = "the coordinator must accept or cancel this parallel job"]
pub struct ParallelJob {
    continuation: ParallelContinuation,
    top_idx: usize,
    vm: VmId,
    token: ExecutionToken,
    reservation: ExecutionReservation,
    reserved_fuel: u32,
}

impl ParallelJob {
    /// The scheduler task that owns this job.
    pub fn task(&self) -> TaskKey {
        self.continuation.task
    }

    /// The machine returned by this report.
    pub fn machine(&self) -> VmId {
        self.vm
    }
}

/// One worker dispatch and its coordinator state.
pub struct ParallelDispatch {
    lease: ExecutionLease,
    job: ParallelJob,
}

impl ParallelDispatch {
    /// The unique lease identity of this dispatch.
    pub fn lease_id(&self) -> u64 {
        self.job.token.lease
    }

    /// Separate the worker payload from coordinator state.
    pub fn into_parts(self) -> (ExecutionLease, ParallelJob) {
        (self.lease, self.job)
    }
}

/// One restored worker report before its world action commits.
#[must_use = "the coordinator must commit this parallel report"]
pub struct ParallelReturned {
    continuation: ParallelContinuation,
    top_idx: usize,
    vm: VmId,
    stop: ExecutionStop,
    retired: u32,
    reached_boundary: bool,
}

impl ParallelReturned {
    /// The scheduler task that owns this report.
    pub fn task(&self) -> TaskKey {
        self.continuation.task
    }

    /// The machine returned by this report.
    pub fn machine(&self) -> VmId {
        self.vm
    }

    /// True when this report has no semantic world action.
    pub fn can_commit_out_of_order(&self) -> bool {
        matches!(
            self.stop,
            ExecutionStop::QuantumExpired
                | ExecutionStop::HeapTrip
                | ExecutionStop::NeedsQuiescence
        )
    }

    /// True when this report stopped before a global control operation.
    pub fn needs_global_quiescence(&self) -> bool {
        matches!(self.stop, ExecutionStop::NeedsQuiescence)
    }
}

/// One quiescent deterministic fallback for a complex wait tree.
pub struct ParallelFallback {
    key: TaskKey,
    quantum: u32,
}

impl ParallelFallback {
    /// The scheduler task that owns this fallback.
    pub fn task(&self) -> TaskKey {
        self.key
    }
}

/// The next action for one parallel scheduler task.
// Keep `Dispatch` inline. Boxing it would allocate at each worker boundary.
#[allow(clippy::large_enum_variant)]
pub enum ParallelStep {
    /// Send one machine lease to a worker.
    Dispatch(ParallelDispatch),
    /// Publish one scheduler slice exit.
    Complete {
        /// The completed scheduler task.
        task: TaskKey,
        /// The task's scheduler exit.
        exit: Option<SliceExit>,
    },
    /// Quiesce active workers before one global operation.
    Quiesce(ParallelContinuation),
    /// Continue this slice after the coordinator committed one action.
    Continue(ParallelContinuation),
    /// Quiesce active workers before one nested wait fallback.
    Fallback(ParallelFallback),
}

/// The machine residency needed before one report can commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelRequirement {
    /// The report can commit now.
    Ready,
    /// The named machine must return first.
    Machine(TaskKey),
    /// The named machine must stop at a control safepoint.
    Safepoint(TaskKey),
    /// Every machine must return first.
    Quiescent,
}

/// A scheduler boundary rejected invalid worker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParallelError {
    /// A report does not match its world, machine, or lease.
    StaleReport,
    /// A worker lost its leased machine.
    WorkerFailed,
    /// Parallel execution reached an invalid machine state.
    InvalidState,
    /// An earlier worker failure poisoned this world.
    Poisoned,
}

impl fmt::Display for ParallelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            ParallelError::StaleReport => "a worker returned a stale execution report",
            ParallelError::WorkerFailed => "a worker failed during guest execution",
            ParallelError::InvalidState => "parallel execution reached an invalid machine state",
            ParallelError::Poisoned => "a prior worker failure poisoned this world",
        };
        f.write_str(text)
    }
}

impl std::error::Error for ParallelError {}

impl World {
    /// Start one owned scheduler slice.
    pub fn begin_parallel_slice(
        &mut self,
        key: TaskKey,
        quantum: u32,
        lease: u64,
    ) -> Result<ParallelStep, ParallelError> {
        if self.poisoned {
            return Err(ParallelError::Poisoned);
        }
        match self.task_status(key) {
            TaskStatus::Dormant => {
                return Ok(ParallelStep::Complete {
                    task: key,
                    exit: None,
                })
            }
            TaskStatus::Terminal => {
                return Ok(ParallelStep::Complete {
                    task: key,
                    exit: Some(SliceExit::Terminal),
                })
            }
            TaskStatus::Blocked(wake) => {
                return Ok(ParallelStep::Complete {
                    task: key,
                    exit: Some(SliceExit::Blocked(wake)),
                })
            }
            TaskStatus::Waiting(completion) => {
                return Ok(ParallelStep::Complete {
                    task: key,
                    exit: Some(SliceExit::Waiting(completion)),
                })
            }
            TaskStatus::Parked(wait) => {
                if self
                    .suspended
                    .get(&key.vm)
                    .is_some_and(|saved| matches!(saved.reason, SuspendReason::Parked { .. }))
                {
                    return Ok(ParallelStep::Fallback(ParallelFallback {
                        key,
                        quantum: quantum.max(1),
                    }));
                }
                return Ok(ParallelStep::Complete {
                    task: key,
                    exit: Some(SliceExit::Parked(wait)),
                });
            }
            TaskStatus::Ready => {}
        }
        if self.machines[key.vm as usize].vm.state == MachineState::Blocked
            && !self.suspended.contains_key(&key.vm)
        {
            self.complete_blocked_machine(key.vm);
            match self.task_status(key) {
                TaskStatus::Ready => {}
                TaskStatus::Dormant => {
                    return Ok(ParallelStep::Complete {
                        task: key,
                        exit: None,
                    })
                }
                TaskStatus::Terminal => {
                    return Ok(ParallelStep::Complete {
                        task: key,
                        exit: Some(SliceExit::Terminal),
                    })
                }
                TaskStatus::Blocked(wake) => {
                    return Ok(ParallelStep::Complete {
                        task: key,
                        exit: Some(SliceExit::Blocked(wake)),
                    })
                }
                TaskStatus::Waiting(completion) => {
                    return Ok(ParallelStep::Complete {
                        task: key,
                        exit: Some(SliceExit::Waiting(completion)),
                    })
                }
                TaskStatus::Parked(wait) => {
                    return Ok(ParallelStep::Complete {
                        task: key,
                        exit: Some(SliceExit::Parked(wait)),
                    })
                }
            }
        }
        let stack = if let Some(saved) = self.suspended.remove(&key.vm) {
            if saved.activations.is_empty() {
                return Err(ParallelError::InvalidState);
            }
            saved.activations
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
            stack
        } else {
            return Err(ParallelError::InvalidState);
        };
        self.continue_parallel_slice(
            ParallelContinuation {
                task: key,
                stack,
                quantum: Some(quantum.max(1)),
            },
            lease,
            false,
        )
    }

    /// Continue one restored scheduler slice.
    pub fn continue_parallel_slice(
        &mut self,
        mut continuation: ParallelContinuation,
        lease: u64,
        exclusive_world: bool,
    ) -> Result<ParallelStep, ParallelError> {
        if self.poisoned {
            return Err(ParallelError::Poisoned);
        }
        match self.advance_stack(&mut continuation.stack, &mut continuation.quantum) {
            DriverStep::Event(event) => {
                let exit = self.scheduler_event_exit(continuation.task, event);
                Ok(ParallelStep::Complete {
                    task: continuation.task,
                    exit: Some(exit),
                })
            }
            DriverStep::Execute { top_idx, vm, limit } => self.make_parallel_dispatch(
                continuation,
                top_idx,
                vm,
                limit,
                lease,
                exclusive_world,
            ),
        }
    }

    fn make_parallel_dispatch(
        &mut self,
        continuation: ParallelContinuation,
        top_idx: usize,
        vm: VmId,
        limit: u32,
        lease: u64,
        exclusive_world: bool,
    ) -> Result<ParallelStep, ParallelError> {
        if lease == 0
            || !self
                .machines
                .get(vm as usize)
                .is_some_and(MachineSlot::is_resident)
        {
            return Err(ParallelError::InvalidState);
        }
        if !self.share_heap_budget() {
            return Err(ParallelError::InvalidState);
        }
        let generation = self.machines[vm as usize].generation();
        let token = ExecutionToken {
            world: self.world_id,
            machine: vm,
            generation,
            lease,
        };
        let image = self.machines[vm as usize].image();
        let slots = image.and_then(|key| {
            self.vm_images.get(key.image as usize).and_then(|record| {
                (record.live && record.generation == key.generation).then(|| record.slots.clone())
            })
        });
        let mut envs = self.machines[vm as usize]
            .take_worker_envs()
            .unwrap_or_else(|| Box::new(self.envs.fork_shared()));
        envs.sync_shared();
        let machine = self.machines[vm as usize].take_for_lease(token);
        let limits = ExecutionLimits {
            instructions: limit,
            heap_trip_bytes: if exclusive_world {
                usize::MAX
            } else {
                LEASE_HEAP_BYTES
            },
            heap_trip_objects: if exclusive_world {
                usize::MAX
            } else {
                LEASE_HEAP_OBJECTS
            },
            exclusive_world,
        };
        let (worker, reservation) = ExecutionLease::new(
            token,
            machine,
            self.execution_code.clone(),
            envs,
            slots,
            limits,
        );
        debug_assert!(u64::from(limit) <= self.budget.fuel);
        self.budget.fuel -= u64::from(limit);
        Ok(ParallelStep::Dispatch(ParallelDispatch {
            lease: worker,
            job: ParallelJob {
                continuation,
                top_idx,
                vm,
                token,
                reservation,
                reserved_fuel: limit,
            },
        }))
    }

    /// Restore one worker report without committing its world action.
    pub fn accept_parallel_report(
        &mut self,
        job: ParallelJob,
        report: ExecutionReport,
    ) -> Result<ParallelReturned, ParallelError> {
        let report_retired = report.retired_instructions();
        let valid = report.token() == job.token
            && report_retired <= job.reserved_fuel
            && job.token.world == self.world_id
            && self
                .machines
                .get(job.vm as usize)
                .is_some_and(|slot| slot.lease == Some(job.token));
        if !valid {
            self.release_parallel_fuel(job.reserved_fuel, 0);
            job.reservation.cancel_destroyed();
            if let Some(slot) = self.machines.get_mut(job.vm as usize) {
                slot.abandon_lease(job.token);
            }
            self.poisoned = true;
            return Err(ParallelError::StaleReport);
        }
        let (token, machine, _code, mut envs, stop, retired) = report.into_parts(job.reservation);
        self.release_parallel_fuel(job.reserved_fuel, retired);
        self.envs.merge_metrics(&mut envs);
        self.envs.sync_shared();
        envs.sync_shared();
        if self.machines[job.vm as usize]
            .restore_from_lease(token, machine)
            .is_err()
        {
            self.poisoned = true;
            return Err(ParallelError::StaleReport);
        }
        self.machines[job.vm as usize].restore_worker_envs(envs);
        let reached_boundary = !matches!(
            stop,
            ExecutionStop::QuantumExpired
                | ExecutionStop::HeapTrip
                | ExecutionStop::NeedsQuiescence
        );
        Ok(ParallelReturned {
            continuation: job.continuation,
            top_idx: job.top_idx,
            vm: job.vm,
            stop,
            retired,
            reached_boundary,
        })
    }

    /// Return the residency needed by one report action.
    pub fn parallel_requirement(&mut self, returned: &ParallelReturned) -> ParallelRequirement {
        let ExecutionStop::Boundary(outcome) = &returned.stop else {
            return ParallelRequirement::Ready;
        };
        match outcome {
            ExecOutcome::Perform { op, args } => {
                if operation_needs_global_quiescence(*op) {
                    return ParallelRequirement::Quiescent;
                }
                if let Some(requirement) = self.policy_requirement(returned.vm) {
                    return requirement;
                }
                if let Some(requirement) =
                    self.control_operation_requirement(returned.vm, *op, args)
                {
                    if requirement != ParallelRequirement::Ready {
                        return requirement;
                    }
                }
                self.perform_machine_requirement(returned.vm, *op, args)
            }
            ExecOutcome::PrepareWait { op, argc, .. } => {
                if let Some(requirement) = self.policy_requirement(returned.vm) {
                    return requirement;
                }
                let operands = &self.machines[returned.vm as usize].vm.operands;
                let args = usize::try_from(*argc)
                    .ok()
                    .and_then(|argc| operands.len().checked_sub(argc).map(|at| &operands[at..]));
                args.map(|args| self.perform_machine_requirement(returned.vm, *op, args))
                    .unwrap_or(ParallelRequirement::Ready)
            }
            ExecOutcome::TableEdit { table, .. } => {
                let target = match self.machines[returned.vm as usize].vm.heap.get(*table) {
                    Object::NativeTable { vm } => Some(*vm),
                    _ => None,
                };
                target
                    .and_then(|vm| {
                        self.machines
                            .get(vm as usize)
                            .map(|slot| self.machine_requirement(vm, slot.generation()))
                    })
                    .unwrap_or(ParallelRequirement::Ready)
            }
            ExecOutcome::AsCall { request, .. } | ExecOutcome::RequestOp { request } => {
                let target = match self.machines[returned.vm as usize].vm.heap.get(*request) {
                    Object::NativeRequest { vm, .. } => Some(*vm),
                    _ => None,
                };
                target
                    .and_then(|vm| {
                        self.machines
                            .get(vm as usize)
                            .map(|slot| self.machine_requirement(vm, slot.generation()))
                    })
                    .unwrap_or(ParallelRequirement::Ready)
            }
            ExecOutcome::CallArgs { call } => {
                let target = match self.machines[returned.vm as usize].vm.heap.get(*call) {
                    Object::NativeCall { vm, .. } => Some(*vm),
                    _ => None,
                };
                target
                    .and_then(|vm| {
                        self.machines
                            .get(vm as usize)
                            .map(|slot| self.machine_requirement(vm, slot.generation()))
                    })
                    .unwrap_or(ParallelRequirement::Ready)
            }
            _ => ParallelRequirement::Ready,
        }
    }

    fn control_operation_requirement(
        &mut self,
        source: VmId,
        op: u32,
        args: &[Value],
    ) -> Option<ParallelRequirement> {
        let first = args.first().copied();
        let roots = match op {
            lm_abi::OP_VM_SNAPSHOT_HELD | lm_abi::OP_VM_SNAPSHOT_WAIT_HELD => {
                vec![self.handle_run(source, first?)?]
            }
            lm_abi::OP_VM_SNAPSHOT_SELF => vec![source],
            lm_abi::OP_PROC_SNAPSHOT_WAIT => {
                let reference = first?.as_obj()?;
                let Object::NativeHandle { proc, .. } =
                    self.machines[source as usize].vm.heap.try_get(reference)?
                else {
                    return Some(ParallelRequirement::Ready);
                };
                vec![*proc]
            }
            lm_abi::OP_VM_SNAPSHOT_VM => {
                let key = self.handle_vm(source, first?)?;
                let live = self
                    .vm_images
                    .get(key.image as usize)
                    .is_some_and(|image| image.live && image.generation == key.generation);
                if !live {
                    return Some(ParallelRequirement::Ready);
                }
                match self.vm_snapshot_roots(key) {
                    Ok(roots) => roots,
                    Err(_) => return Some(ParallelRequirement::Ready),
                }
            }
            lm_abi::OP_VM_INSTALL
            | lm_abi::OP_VM_INSTALL_WITH
            | lm_abi::OP_VM_REPLACE_FUNCTION
            | lm_abi::OP_VM_REPLACE_CLASS
            | lm_abi::OP_VM_REPLACE_VALUE
            | lm_abi::OP_VM_REPLACE_PROCESS
            | lm_abi::OP_VM_REPLACE_ALL => {
                let key = self.handle_vm(source, first?)?;
                return Some(self.image_safepoint_requirement(source, key));
            }
            _ => return None,
        };
        Some(self.snapshot_safepoint_requirement(source, &roots))
    }

    fn snapshot_safepoint_requirement(
        &mut self,
        source: VmId,
        roots: &[VmId],
    ) -> ParallelRequirement {
        let mut found = Vec::new();
        let mut queue: VecDeque<VmId> = roots.iter().copied().collect();
        while let Some(vm) = queue.pop_front() {
            if found.contains(&vm) {
                continue;
            }
            let Some(slot) = self.machines.get(vm as usize) else {
                continue;
            };
            let generation = slot.generation();
            if vm != source {
                let requirement = self.machine_safepoint_requirement(vm, generation);
                if requirement != ParallelRequirement::Ready {
                    return requirement;
                }
            }
            found.push(vm);
            if !self.is_live_machine(vm) {
                continue;
            }
            let references = match self.machine_references(vm) {
                Ok(references) => references,
                Err(_) => return ParallelRequirement::Ready,
            };
            queue.extend(references);
        }
        ParallelRequirement::Ready
    }

    fn image_safepoint_requirement(&self, source: VmId, key: VmImageKey) -> ParallelRequirement {
        for (vm, slot) in self.machines.iter().enumerate() {
            let vm = vm as VmId;
            if vm == source || slot.image() != Some(key) {
                continue;
            }
            let requirement = self.machine_safepoint_requirement(vm, slot.generation());
            if requirement != ParallelRequirement::Ready {
                return requirement;
            }
        }
        ParallelRequirement::Ready
    }

    fn policy_requirement(&self, vm: VmId) -> Option<ParallelRequirement> {
        let mut current = vm;
        for _ in 0..self.machines.len() {
            let parent = self.machines.get(current as usize)?.parent();
            let parent = parent?;
            let slot = self.machines.get(parent as usize)?;
            if !slot.is_resident() {
                return Some(ParallelRequirement::Machine(TaskKey {
                    vm: parent,
                    generation: slot.generation(),
                }));
            }
            current = parent;
        }
        Some(ParallelRequirement::Quiescent)
    }

    fn perform_machine_requirement(
        &self,
        source: VmId,
        op: u32,
        args: &[Value],
    ) -> ParallelRequirement {
        if matches!(
            op,
            lm_abi::OP_PROC_SPAWN
                | lm_abi::OP_VM_ACTIVATE
                | lm_abi::OP_VM_ACTIVATE_OR_FAULT
                | lm_abi::OP_VM_ACTIVATE_DEF
        ) && self.child_reclamation_needed(source)
        {
            return ParallelRequirement::Quiescent;
        }
        if op == lm_abi::OP_VM_NEW && self.image_reclamation_needed() {
            return ParallelRequirement::Quiescent;
        }
        let Some(source_machine) = self.machines.get(source as usize) else {
            return ParallelRequirement::Ready;
        };
        for value in args {
            let Some(reference) = value.as_obj() else {
                continue;
            };
            let Some(object) = source_machine.vm.heap.try_get(reference) else {
                continue;
            };
            let target = match object {
                Object::NativeHandle { proc, generation } => Some((*proc, Some(*generation))),
                Object::NativeRun { vm }
                | Object::NativeTable { vm }
                | Object::NativeRequest { vm, .. }
                | Object::NativeCall { vm, .. } => Some((*vm, None)),
                Object::NativeResourceHandle { surface, .. } => Some((*surface, None)),
                _ => None,
            };
            let Some((target, generation)) = target else {
                continue;
            };
            if target == source {
                continue;
            }
            let Some(slot) = self.machines.get(target as usize) else {
                continue;
            };
            let generation = generation.unwrap_or_else(|| slot.generation());
            let requirement = if operation_needs_idle_target(op) {
                self.machine_safepoint_requirement(target, generation)
            } else {
                self.machine_requirement(target, generation)
            };
            if requirement != ParallelRequirement::Ready {
                return requirement;
            }
        }
        if matches!(op, lm_abi::OP_WAIT_WAIT | lm_abi::OP_WAIT_CANCEL) {
            if let Some(requirement) = self.wait_machine_requirement(source, args.first().copied())
            {
                return requirement;
            }
        }
        ParallelRequirement::Ready
    }

    fn child_reclamation_needed(&self, parent: VmId) -> bool {
        let Some(machine) = self.machines.get(parent as usize) else {
            return false;
        };
        !self.has_machine_room(1) || machine.children >= machine.config.max_children
    }

    fn image_reclamation_needed(&self) -> bool {
        self.vm_images
            .len()
            .saturating_sub(self.vm_image_free.len())
            >= self.vm_image_limit()
    }

    fn wait_machine_requirement(
        &self,
        source: VmId,
        value: Option<Value>,
    ) -> Option<ParallelRequirement> {
        let reference = value?.as_obj()?;
        let machine = self.machines.get(source as usize)?;
        let token = match machine.vm.heap.try_get(reference)? {
            Object::NativeWait { owner, token } if *owner == source => *token,
            _ => return None,
        };
        let (leaves, _) = self.wait_tree(source, token).ok()?;
        leaves.into_iter().find_map(|leaf| {
            let WaitLeaf::Drive { target } = leaf.leaf else {
                return None;
            };
            let slot = self.machines.get(target as usize)?;
            let requirement = self.machine_safepoint_requirement(target, slot.generation());
            (requirement != ParallelRequirement::Ready).then_some(requirement)
        })
    }

    fn machine_requirement(&self, vm: VmId, generation: u32) -> ParallelRequirement {
        let Some(slot) = self.machines.get(vm as usize) else {
            return ParallelRequirement::Ready;
        };
        if slot.generation() != generation || slot.is_resident() {
            ParallelRequirement::Ready
        } else {
            ParallelRequirement::Machine(TaskKey { vm, generation })
        }
    }

    fn machine_safepoint_requirement(&self, vm: VmId, generation: u32) -> ParallelRequirement {
        let Some(slot) = self.machines.get(vm as usize) else {
            return ParallelRequirement::Ready;
        };
        if slot.generation() != generation {
            return ParallelRequirement::Ready;
        }
        if slot.is_resident() && slot.active() == 0 {
            ParallelRequirement::Ready
        } else {
            ParallelRequirement::Safepoint(TaskKey { vm, generation })
        }
    }

    /// Commit one restored report and continue its task.
    pub fn commit_parallel_report(
        &mut self,
        mut returned: ParallelReturned,
    ) -> Result<ParallelStep, ParallelError> {
        match self.parallel_requirement(&returned) {
            ParallelRequirement::Ready => {}
            ParallelRequirement::Machine(key) | ParallelRequirement::Safepoint(key) => {
                if !self
                    .machines
                    .get(key.vm as usize)
                    .is_some_and(MachineSlot::is_resident)
                {
                    return Err(ParallelError::InvalidState);
                }
            }
            ParallelRequirement::Quiescent if self.all_machines_resident() => {}
            ParallelRequirement::Quiescent => return Err(ParallelError::InvalidState),
        }
        if matches!(returned.stop, ExecutionStop::NeedsQuiescence) {
            let event = self.commit_execution_stop(
                &mut returned.continuation.stack,
                returned.top_idx,
                returned.vm,
                &mut returned.continuation.quantum,
                ExecutionCommit {
                    stop: returned.stop,
                    retired: returned.retired,
                    reached_boundary: false,
                    charge_fuel: false,
                },
            );
            debug_assert!(event.is_none());
            return Ok(ParallelStep::Quiesce(returned.continuation));
        }
        let event = self.commit_execution_stop(
            &mut returned.continuation.stack,
            returned.top_idx,
            returned.vm,
            &mut returned.continuation.quantum,
            ExecutionCommit {
                stop: returned.stop,
                retired: returned.retired,
                reached_boundary: returned.reached_boundary,
                charge_fuel: false,
            },
        );
        if let Some(event) = event {
            let exit = self.scheduler_event_exit(returned.continuation.task, event);
            return Ok(ParallelStep::Complete {
                task: returned.continuation.task,
                exit: Some(exit),
            });
        }
        Ok(ParallelStep::Continue(returned.continuation))
    }

    /// Execute one fallback after every worker returns.
    pub fn run_parallel_fallback(
        &mut self,
        fallback: ParallelFallback,
    ) -> Result<Option<SliceExit>, ParallelError> {
        if !self.all_machines_resident() {
            return Err(ParallelError::InvalidState);
        }
        Ok(self.drive_slice(fallback.key, fallback.quantum))
    }

    /// Run one retained continuation after global quiescence.
    pub fn run_parallel_continuation_inline(
        &mut self,
        mut continuation: ParallelContinuation,
    ) -> Result<Option<SliceExit>, ParallelError> {
        if !self.all_machines_resident() {
            return Err(ParallelError::InvalidState);
        }
        let event = self.drive_stack(&mut continuation.stack, continuation.quantum);
        Ok(Some(self.scheduler_event_exit(continuation.task, event)))
    }

    /// Park one retained continuation at its current worker boundary.
    pub fn park_parallel_continuation(
        &mut self,
        mut continuation: ParallelContinuation,
    ) -> Result<(TaskKey, Option<SliceExit>), ParallelError> {
        if continuation.stack.is_empty()
            || continuation.stack.iter().any(|activation| {
                !self
                    .machines
                    .get(activation.vm as usize)
                    .is_some_and(MachineSlot::is_resident)
            })
        {
            return Err(ParallelError::InvalidState);
        }
        let task = continuation.task;
        match self.advance_stack(&mut continuation.stack, &mut continuation.quantum) {
            DriverStep::Event(event) => {
                let exit = self.scheduler_event_exit(task, event);
                return Ok((task, Some(exit)));
            }
            DriverStep::Execute { .. } => {}
        }
        if continuation.stack.len() == 1 {
            let activation = continuation
                .stack
                .pop()
                .expect("the continuation has one activation");
            self.release_activation(activation);
        } else {
            let base = continuation.stack[0].vm;
            self.park_stack(&continuation.stack);
            self.suspended.insert(
                base,
                SuspendedStack {
                    activations: continuation.stack,
                    reason: SuspendReason::Yielded,
                },
            );
        }
        Ok((task, Some(SliceExit::Yielded)))
    }

    /// Discard one uncommitted child action after root termination.
    pub fn discard_parallel_report_after_root(
        &mut self,
        mut returned: ParallelReturned,
    ) -> Result<TaskKey, ParallelError> {
        if !self.all_machines_resident() || returned.continuation.stack.is_empty() {
            return Err(ParallelError::InvalidState);
        }
        let task = returned.continuation.task;
        let event = self.commit_execution_stop(
            &mut returned.continuation.stack,
            returned.top_idx,
            returned.vm,
            &mut returned.continuation.quantum,
            ExecutionCommit {
                stop: ExecutionStop::Boundary(ExecOutcome::Continue),
                retired: returned.retired,
                reached_boundary: returned.reached_boundary,
                charge_fuel: false,
            },
        );
        debug_assert!(event.is_none());
        while let Some(activation) = returned.continuation.stack.pop() {
            self.release_activation(activation);
        }
        if let Some(machine) = self.machines.get_mut(task.vm as usize) {
            machine.set_fault(
                FaultCode::InvalidVmState,
                "the root stopped before this task action committed",
                None,
            );
        }
        Ok(task)
    }

    /// Cancel one job after its worker lost the machine.
    pub fn cancel_parallel_job(&mut self, job: ParallelJob) -> ParallelError {
        self.release_parallel_fuel(job.reserved_fuel, 0);
        job.reservation.cancel_destroyed();
        if let Some(slot) = self.machines.get_mut(job.vm as usize) {
            slot.abandon_lease(job.token);
        }
        self.poisoned = true;
        ParallelError::WorkerFailed
    }

    fn release_parallel_fuel(&mut self, reserved: u32, retired: u32) {
        debug_assert!(retired <= reserved);
        self.budget.fuel = self
            .budget
            .fuel
            .checked_add(u64::from(reserved.saturating_sub(retired)))
            .expect("a returned fuel reservation fits its world limit");
    }

    /// True when every machine is coordinator-resident.
    pub fn all_machines_resident(&self) -> bool {
        self.machines.iter().all(MachineSlot::is_resident)
    }

    /// True after an invalid report or worker failure.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

fn operation_needs_idle_target(op: u32) -> bool {
    matches!(
        op,
        lm_abi::OP_VM_RUN
            | lm_abi::OP_VM_STEP
            | lm_abi::OP_VM_DRIVE
            | lm_abi::OP_VM_DRIVE_FOR
            | lm_abi::OP_VM_DRIVE_WAIT
            | lm_abi::OP_VM_HANDLES
            | lm_abi::OP_VM_RESOURCE
            | lm_abi::OP_PROC_RUN
            | lm_abi::OP_PROC_PAUSE
            | lm_abi::OP_PROC_RESUME
            | lm_abi::OP_WAIT_WAIT
            | lm_abi::OP_WAIT_CANCEL
    )
}
