//! The central parallel scheduler coordinator.

use super::*;
use crate::pool::{WorkerEvent, WorkerPool};
use lm_vm::{
    ParallelContinuation, ParallelFallback, ParallelJob, ParallelRequirement, ParallelReturned,
    ParallelStep,
};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

/// One failure below guest fault semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerError {
    message: String,
}

impl SchedulerError {
    fn new(message: impl Into<String>) -> SchedulerError {
        SchedulerError {
            message: message.into(),
        }
    }
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SchedulerError {}

struct ActiveJob {
    job: ParallelJob,
    starts_slice: bool,
    exclusive: bool,
}

struct PendingContinuation {
    continuation: ParallelContinuation,
}

struct PendingFallback {
    fallback: ParallelFallback,
    starts_slice: bool,
}

struct ParallelCoordinator<'a> {
    scheduler: &'a mut Scheduler,
    world: &'a mut World,
    pool: Option<WorkerPool>,
    worker_count: usize,
    notifier: Arc<dyn Fn() + Send + Sync>,
    wake: Receiver<()>,
    active: BTreeMap<u64, ActiveJob>,
    returned: VecDeque<ParallelReturned>,
    continuations: VecDeque<PendingContinuation>,
    refills: VecDeque<ParallelContinuation>,
    quiescent: VecDeque<ParallelContinuation>,
    fallbacks: VecDeque<PendingFallback>,
    next_lease: u64,
    root_terminal: bool,
}

impl Scheduler {
    /// Run one world with a bounded parallel worker pool.
    pub fn run_parallel(
        &mut self,
        world: &mut World,
        workers: usize,
    ) -> Result<Outcome, SchedulerError> {
        if workers == 0 || workers > 256 {
            return Err(SchedulerError::new(
                "the parallel worker count must be between 1 and 256",
            ));
        }
        self.reset(world, true);
        let (wake_tx, wake) = mpsc::channel();
        let notifier: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        world.set_scheduler_wake(Some(Arc::clone(&notifier)));
        let mut coordinator = ParallelCoordinator {
            scheduler: self,
            world,
            pool: None,
            worker_count: workers,
            notifier,
            wake,
            active: BTreeMap::new(),
            returned: VecDeque::new(),
            continuations: VecDeque::new(),
            refills: VecDeque::new(),
            quiescent: VecDeque::new(),
            fallbacks: VecDeque::new(),
            next_lease: 1,
            root_terminal: false,
        };
        let result = coordinator.run();
        if result.is_err() {
            coordinator.abort();
        }
        if let Some(pool) = &mut coordinator.pool {
            pool.shutdown();
        }
        coordinator.world.set_scheduler_wake(None);
        result
    }
}

impl ParallelCoordinator<'_> {
    fn run(&mut self) -> Result<Outcome, SchedulerError> {
        loop {
            self.scheduler.consume_events(self.world);
            self.scheduler.poll_completions(self.world);
            self.drain_wake_markers();
            self.drain_worker_reports()?;
            self.commit_ready_reports()?;

            if self.root_terminal {
                if self.active.is_empty() {
                    self.settle_after_root()?;
                    let root = self
                        .world
                        .task_key(self.world.root())
                        .ok_or_else(|| SchedulerError::new("the root machine is missing"))?;
                    return Ok(self.world.task_outcome(root));
                }
                self.park()?;
                continue;
            }

            if self.active.is_empty() {
                if let Some(continuation) = self.quiescent.pop_front() {
                    self.park_retained_continuations()?;
                    let lease = self.take_lease()?;
                    let step = self
                        .world
                        .continue_parallel_slice(continuation, lease, true)
                        .map_err(|error| SchedulerError::new(error.to_string()))?;
                    self.accept_step(step, false, true)?;
                    continue;
                }
                if let Some(continuation) = self.refills.pop_front() {
                    self.park_retained_continuations()?;
                    let lease = self.take_lease()?;
                    let step = self
                        .world
                        .continue_parallel_slice(continuation, lease, true)
                        .map_err(|error| SchedulerError::new(error.to_string()))?;
                    self.accept_step(step, false, true)?;
                    continue;
                }
                if let Some(fallback) = self.fallbacks.pop_front() {
                    self.park_retained_continuations()?;
                    let task = fallback.fallback.task();
                    if fallback.starts_slice {
                        self.world.note_parallel_report(0, 0, true);
                    }
                    let exit = self
                        .world
                        .run_parallel_fallback(fallback.fallback)
                        .map_err(|error| SchedulerError::new(error.to_string()))?;
                    self.finish_slice(task, exit);
                    continue;
                }
            }

            let stopping = self.needs_quiescence();
            if !stopping {
                if self.world.has_snapshot_watchers() && self.active.is_empty() {
                    self.park_retained_continuations()?;
                    self.run_one_serial_slice()?;
                    continue;
                }
                if self.pool.is_none() {
                    match self.parallel_work_count() {
                        0 => {}
                        1 => {
                            self.run_one_serial_slice()?;
                            continue;
                        }
                        _ => self.ensure_pool()?,
                    }
                }
                self.dispatch_ready_work()?;
                self.commit_ready_reports()?;
            }

            if self.root_terminal {
                continue;
            }
            if !self.active.is_empty() {
                self.park()?;
                continue;
            }
            if self.needs_quiescence() {
                continue;
            }
            if !self.scheduler.waiting.is_empty() || self.scheduler.parked.has_completions() {
                if let Some(completion) = self.world.wait_host_completion(|key| {
                    self.scheduler.waiting.contains_key(&key)
                        || self.scheduler.parked.has_completion(key)
                }) {
                    self.scheduler.complete_completion(self.world, completion);
                } else {
                    self.scheduler.fail_waiting(self.world);
                }
                continue;
            }
            if self
                .scheduler
                .tasks
                .values()
                .any(|state| matches!(state, IndexedState::Blocked(_) | IndexedState::Parked(_)))
            {
                self.scheduler.stop = Some(StopReason::Deadlock);
                self.scheduler.fail_every_block(self.world);
                continue;
            }
            let root = self
                .world
                .task_key(self.world.root())
                .ok_or_else(|| SchedulerError::new("the root machine is missing"))?;
            self.world
                .fail_blocked_task(root, "the scheduler found no runnable task");
            self.scheduler.refresh(self.world, root);
            self.scheduler.stop = Some(StopReason::Deadlock);
        }
    }

    fn dispatch_ready_work(&mut self) -> Result<(), SchedulerError> {
        while self.pool.as_ref().is_some_and(WorkerPool::has_idle) && !self.needs_quiescence() {
            if let Some(pending) = self.continuations.pop_front() {
                let lease = self.take_lease()?;
                let step = self
                    .world
                    .continue_parallel_slice(pending.continuation, lease, false)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                self.accept_step(step, false, false)?;
                continue;
            }
            let Some(task) = self.scheduler.next_ready(self.world) else {
                break;
            };
            if task.vm == self.world.root() {
                self.scheduler.stats.root_slices =
                    self.scheduler.stats.root_slices.saturating_add(1);
            } else {
                self.scheduler.stats.proc_slices =
                    self.scheduler.stats.proc_slices.saturating_add(1);
            }
            self.scheduler.tasks.insert(task, IndexedState::Running);
            let lease = self.take_lease()?;
            let step = self
                .world
                .begin_parallel_slice(task, self.scheduler.parallel_quantum(), lease)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.accept_step(step, true, false)?;
        }
        Ok(())
    }

    fn accept_step(
        &mut self,
        step: ParallelStep,
        starts_slice: bool,
        exclusive: bool,
    ) -> Result<(), SchedulerError> {
        match step {
            ParallelStep::Dispatch(dispatch) => {
                let lease_id = dispatch.lease_id();
                let (lease, job) = dispatch.into_parts();
                if self.active.contains_key(&lease_id) {
                    drop(lease);
                    let error = self.world.cancel_parallel_job(job);
                    return Err(SchedulerError::new(error.to_string()));
                }
                let pool = self
                    .pool
                    .as_mut()
                    .expect("parallel dispatch has a worker pool");
                if let Err((message, lease)) = pool.dispatch(lease_id, lease) {
                    drop(lease);
                    let _ = self.world.cancel_parallel_job(job);
                    return Err(SchedulerError::new(message));
                }
                self.active.insert(
                    lease_id,
                    ActiveJob {
                        job,
                        starts_slice,
                        exclusive,
                    },
                );
                self.scheduler.stats.max_active_leases = self
                    .scheduler
                    .stats
                    .max_active_leases
                    .max(self.active.len().min(u32::MAX as usize) as u32);
            }
            ParallelStep::Complete { task, exit } => {
                if starts_slice {
                    self.world.note_parallel_report(0, 0, true);
                }
                self.finish_slice(task, exit);
            }
            ParallelStep::Continue(continuation) => {
                self.continuations
                    .push_back(PendingContinuation { continuation });
            }
            ParallelStep::Refill(continuation) => self.refills.push_back(continuation),
            ParallelStep::Quiesce(continuation) => self.quiescent.push_back(continuation),
            ParallelStep::Fallback(fallback) => self.fallbacks.push_back(PendingFallback {
                fallback,
                starts_slice,
            }),
        }
        Ok(())
    }

    fn drain_worker_reports(&mut self) -> Result<(), SchedulerError> {
        let Some(pool) = &mut self.pool else {
            return Ok(());
        };
        loop {
            let Some(event) = pool.try_event().map_err(SchedulerError::new)? else {
                return Ok(());
            };
            match event {
                WorkerEvent::Report { job: id, report } => {
                    let Some(active) = self.active.remove(&id) else {
                        return Err(SchedulerError::new(
                            "a worker returned an unknown execution report",
                        ));
                    };
                    let retired = report.retired_instructions();
                    let growth = report.heap_growth_bytes();
                    let returned = self
                        .world
                        .accept_parallel_report(active.job, report)
                        .map_err(|error| SchedulerError::new(error.to_string()))?;
                    self.world
                        .note_parallel_report(retired, growth, active.starts_slice);
                    self.returned.push_back(returned);
                }
                WorkerEvent::Failed { job: id } => {
                    let Some(active) = self.active.remove(&id) else {
                        return Err(SchedulerError::new("an unknown scheduler worker failed"));
                    };
                    let error = self.world.cancel_parallel_job(active.job);
                    return Err(SchedulerError::new(error.to_string()));
                }
            }
        }
    }

    fn commit_ready_reports(&mut self) -> Result<(), SchedulerError> {
        loop {
            let Some(returned) = self.returned.front() else {
                return Ok(());
            };
            let requirement = self.world.parallel_requirement(returned);
            if let ParallelRequirement::Machine(key) = requirement {
                if !self
                    .active
                    .values()
                    .any(|active| active.job.machine() == key.vm)
                    && self.park_retained_machine(key)?
                {
                    continue;
                }
            }
            let front_ready = matches!(requirement, ParallelRequirement::Ready)
                || (requirement == ParallelRequirement::Quiescent && self.active.is_empty());
            let index = if front_ready {
                0
            } else {
                let Some(index) =
                    self.returned
                        .iter()
                        .enumerate()
                        .skip(1)
                        .find_map(|(index, returned)| {
                            let needed_machine = match requirement {
                                ParallelRequirement::Machine(key) => returned.machine() == key.vm,
                                _ => false,
                            };
                            (returned.can_commit_out_of_order() || needed_machine).then_some(index)
                        })
                else {
                    return Ok(());
                };
                index
            };
            if self.root_terminal {
                return Ok(());
            }
            let returned = self
                .returned
                .remove(index)
                .expect("the checked report remains queued");
            let task = returned.task();
            let step = self
                .world
                .commit_parallel_report(returned)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            match step {
                ParallelStep::Complete {
                    task: completed,
                    exit,
                } => {
                    debug_assert_eq!(task, completed);
                    self.finish_slice(completed, exit);
                }
                other => self.accept_step(other, false, false)?,
            }
        }
    }

    fn finish_slice(&mut self, task: TaskKey, exit: Option<SliceExit>) {
        if exit == Some(SliceExit::Terminal) {
            self.scheduler.finish_slice(self.world, task, exit);
            self.scheduler.consume_events(self.world);
            if task.vm == self.world.root() {
                self.root_terminal = true;
                self.scheduler.stop.get_or_insert(StopReason::RootTerminal);
            }
        } else {
            self.scheduler.consume_events(self.world);
            self.scheduler.finish_slice(self.world, task, exit);
        }
    }

    fn needs_quiescence(&self) -> bool {
        !self.refills.is_empty()
            || !self.quiescent.is_empty()
            || !self.fallbacks.is_empty()
            || self.active.values().any(|active| active.exclusive)
            || self.returned.iter().any(|returned| {
                self.world.parallel_requirement(returned) == ParallelRequirement::Quiescent
            })
    }

    fn park_retained_continuations(&mut self) -> Result<(), SchedulerError> {
        let mut retained = VecDeque::new();
        while let Some(pending) = self.continuations.pop_front() {
            retained.push_back(pending.continuation);
        }
        retained.append(&mut self.refills);
        retained.append(&mut self.quiescent);
        while let Some(continuation) = retained.pop_front() {
            let (task, exit) = self
                .world
                .park_parallel_continuation(continuation)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.finish_slice(task, exit);
        }
        Ok(())
    }

    fn settle_after_root(&mut self) -> Result<(), SchedulerError> {
        while let Some(returned) = self.returned.pop_front() {
            let task = returned.task();
            if returned.can_commit_out_of_order() {
                let step = self
                    .world
                    .commit_parallel_report(returned)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                match step {
                    ParallelStep::Complete { task, exit } => self.finish_slice(task, exit),
                    ParallelStep::Continue(continuation)
                    | ParallelStep::Refill(continuation)
                    | ParallelStep::Quiesce(continuation) => {
                        self.continuations
                            .push_back(PendingContinuation { continuation });
                    }
                    ParallelStep::Fallback(fallback) => {
                        self.fallbacks.push_back(PendingFallback {
                            fallback,
                            starts_slice: false,
                        });
                    }
                    ParallelStep::Dispatch(dispatch) => {
                        let (_, job) = dispatch.into_parts();
                        let error = self.world.cancel_parallel_job(job);
                        return Err(SchedulerError::new(error.to_string()));
                    }
                }
            } else {
                let completed = self
                    .world
                    .discard_parallel_report_after_root(returned)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                debug_assert_eq!(task, completed);
                self.finish_slice(completed, Some(SliceExit::Terminal));
            }
        }
        self.park_retained_continuations()
    }

    fn park_retained_machine(&mut self, key: TaskKey) -> Result<bool, SchedulerError> {
        let continuation = take_continuation_for(&mut self.continuations, key.vm)
            .or_else(|| take_parallel_continuation_for(&mut self.refills, key.vm))
            .or_else(|| take_parallel_continuation_for(&mut self.quiescent, key.vm));
        let Some(continuation) = continuation else {
            return Ok(false);
        };
        let (task, exit) = self
            .world
            .park_parallel_continuation(continuation)
            .map_err(|error| SchedulerError::new(error.to_string()))?;
        self.finish_slice(task, exit);
        Ok(true)
    }

    fn parallel_work_count(&self) -> usize {
        let retained = self.continuations.len();
        let queued = self
            .scheduler
            .tasks
            .values()
            .filter(|state| matches!(state, IndexedState::Queued(_)))
            .count();
        retained.saturating_add(queued)
    }

    fn ensure_pool(&mut self) -> Result<(), SchedulerError> {
        if self.pool.is_none() {
            self.pool = Some(
                WorkerPool::new(self.worker_count, Arc::clone(&self.notifier))
                    .map_err(SchedulerError::new)?,
            );
        }
        Ok(())
    }

    fn run_one_serial_slice(&mut self) -> Result<(), SchedulerError> {
        if let Some(pending) = self.continuations.pop_front() {
            let task = pending.continuation.task();
            let exit = self
                .world
                .run_parallel_continuation_inline(pending.continuation)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.finish_slice(task, exit);
            return Ok(());
        }
        let Some(task) = self.scheduler.next_ready(self.world) else {
            return Ok(());
        };
        if task.vm == self.world.root() {
            self.scheduler.stats.root_slices = self.scheduler.stats.root_slices.saturating_add(1);
        } else {
            self.scheduler.stats.proc_slices = self.scheduler.stats.proc_slices.saturating_add(1);
        }
        self.scheduler.tasks.insert(task, IndexedState::Running);
        let configured = if self.pool.is_some() {
            self.scheduler.parallel_quantum()
        } else {
            self.scheduler.quantum
        };
        let quantum = self.world.snapshot_wait_quantum(task, configured);
        let before = self.world.world_fuel();
        let heap_before = self.world.aggregate_heap_bytes();
        let exit = self.world.drive_slice(task, quantum);
        let retired = before.saturating_sub(self.world.world_fuel());
        self.world
            .note_scheduler_slice(task, retired, exit.is_some(), heap_before);
        self.finish_slice(task, exit);
        Ok(())
    }

    fn park(&mut self) -> Result<(), SchedulerError> {
        match self.world.scheduler_wait_nanos() {
            Some(0) => Ok(()),
            Some(nanos) => match self.wake.recv_timeout(Duration::from_nanos(nanos)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(SchedulerError::new("the scheduler wake channel closed"))
                }
            },
            None => self
                .wake
                .recv()
                .map_err(|_| SchedulerError::new("the scheduler wake channel closed")),
        }
    }

    fn drain_wake_markers(&self) {
        loop {
            match self.wake.try_recv() {
                Ok(()) => {}
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    fn take_lease(&mut self) -> Result<u64, SchedulerError> {
        let lease = self.next_lease;
        self.next_lease = self
            .next_lease
            .checked_add(1)
            .ok_or_else(|| SchedulerError::new("the execution lease space is exhausted"))?;
        Ok(lease)
    }

    fn abort(&mut self) {
        let Some(pool) = &mut self.pool else {
            return;
        };
        pool.shutdown();
        while !self.active.is_empty() {
            match pool.try_event() {
                Ok(Some(WorkerEvent::Report { job: id, report })) => {
                    if let Some(active) = self.active.remove(&id) {
                        let _ = self.world.accept_parallel_report(active.job, report);
                    }
                }
                Ok(Some(WorkerEvent::Failed { job: id })) => {
                    if let Some(active) = self.active.remove(&id) {
                        let _ = self.world.cancel_parallel_job(active.job);
                    }
                }
                Ok(None) | Err(_) => break,
            }
        }
        let remaining = std::mem::take(&mut self.active);
        for (_, active) in remaining {
            let _ = self.world.cancel_parallel_job(active.job);
        }
    }
}

fn take_continuation_for(
    queue: &mut VecDeque<PendingContinuation>,
    vm: lm_vm::VmId,
) -> Option<ParallelContinuation> {
    let at = queue
        .iter()
        .position(|pending| pending.continuation.contains_machine(vm))?;
    queue.remove(at).map(|pending| pending.continuation)
}

fn take_parallel_continuation_for(
    queue: &mut VecDeque<ParallelContinuation>,
    vm: lm_vm::VmId,
) -> Option<ParallelContinuation> {
    let at = queue
        .iter()
        .position(|continuation| continuation.contains_machine(vm))?;
    queue.remove(at)
}
