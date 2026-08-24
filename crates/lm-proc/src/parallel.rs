//! The central parallel scheduler coordinator.

use super::*;
use crate::pool::{PoolRegistration, SchedulerPool, WorkerEvent};
use lm_vm::{
    ParallelContinuation, ParallelDrive, ParallelJob, ParallelParked, ParallelRequirement,
    ParallelReturned, ParallelStep,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

/// One failure below guest fault semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerError {
    message: String,
}

impl SchedulerError {
    pub(crate) fn new(message: impl Into<String>) -> SchedulerError {
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
    recall_requested: bool,
}

struct PendingContinuation {
    continuation: ParallelContinuation,
}

struct ParallelSnapshotWatch {
    target: u32,
    generation: u32,
    remaining: u64,
    members: BTreeSet<TaskKey>,
}

struct ParallelCoordinator<'a> {
    scheduler: &'a mut Scheduler,
    world: &'a mut World,
    pool: Option<SchedulerPool>,
    pool_registration: Option<PoolRegistration>,
    worker_count: usize,
    workers_enabled: bool,
    notifier: Arc<dyn Fn() + Send + Sync>,
    wake: Receiver<()>,
    report_tx: Sender<WorkerEvent>,
    reports: Receiver<WorkerEvent>,
    active: BTreeMap<u64, ActiveJob>,
    returned: VecDeque<ParallelReturned>,
    continuations: VecDeque<PendingContinuation>,
    drives: VecDeque<ParallelDrive>,
    snapshot_watches: BTreeMap<u32, ParallelSnapshotWatch>,
    scoped_stops: BTreeSet<TaskKey>,
    commit_quiescence: bool,
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
        self.run_parallel_with_pool(world, workers, None)
    }

    pub(crate) fn run_parallel_with_pool(
        &mut self,
        world: &mut World,
        workers: usize,
        pool: Option<SchedulerPool>,
    ) -> Result<Outcome, SchedulerError> {
        if workers == 0 || workers > MAX_PARALLEL_WORKERS {
            return Err(SchedulerError::new(
                "the parallel worker count must be between 1 and 256",
            ));
        }
        self.reset(world, true);
        let (wake_tx, wake) = mpsc::channel();
        let notifier: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let (report_tx, reports) = mpsc::channel();
        let pool_registration = pool
            .as_ref()
            .map(|pool| pool.register_wake(Arc::clone(&notifier)));
        world.set_scheduler_wake(Some(Arc::clone(&notifier)));
        let mut coordinator = ParallelCoordinator {
            scheduler: self,
            world,
            pool,
            pool_registration,
            worker_count: workers,
            workers_enabled: false,
            notifier,
            wake,
            report_tx,
            reports,
            active: BTreeMap::new(),
            returned: VecDeque::new(),
            continuations: VecDeque::new(),
            drives: VecDeque::new(),
            snapshot_watches: BTreeMap::new(),
            scoped_stops: BTreeSet::new(),
            commit_quiescence: false,
            next_lease: 1,
            root_terminal: false,
        };
        let result = coordinator.run();
        if result.is_err() {
            coordinator.abort();
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
            if self.workers_enabled {
                self.drain_wake_markers();
                self.drain_worker_reports()?;
                self.commit_ready_reports()?;
                self.withdraw_ready_drives()?;
                self.sync_snapshot_watches()?;
            }

            let root_vm = self.world.root();
            let root_is_leased = self.workers_enabled
                && self
                    .active
                    .values()
                    .any(|active| active.job.machine() == root_vm);
            if !root_is_leased {
                if let Some(root) = self.world.task_key(root_vm) {
                    if self.world.task_status(root) == TaskStatus::Terminal {
                        self.root_terminal = true;
                        self.scheduler.stop.get_or_insert(StopReason::RootTerminal);
                    }
                }
            }

            if self.root_terminal {
                if self.active.is_empty() {
                    self.settle_after_root()?;
                    let root = self
                        .world
                        .task_key(self.world.root())
                        .ok_or_else(|| SchedulerError::new("the root machine is missing"))?;
                    return Ok(self.world.task_outcome(root));
                }
                self.recall_all_active();
                self.park()?;
                continue;
            }

            if !self.workers_enabled {
                if self.scheduler.has_wide_parallel_wait(self.world) {
                    self.ensure_pool()?;
                    self.workers_enabled = true;
                    continue;
                }
                if let Some(boundary_sparse) = self.run_one_serial_slice()? {
                    if boundary_sparse && self.parallel_work_count() > 1 {
                        self.ensure_pool()?;
                        self.workers_enabled = true;
                    }
                    continue;
                }
            }

            let stopping = self.needs_quiescence();
            if self.needs_global_recall() {
                self.recall_all_active();
            }
            if !stopping {
                if self.workers_enabled
                    && self.active.is_empty()
                    && self.parallel_work_count() == 1
                    && !self.scheduler.has_wide_parallel_wait(self.world)
                {
                    let _ = self.run_one_serial_slice()?;
                    continue;
                }
                self.dispatch_ready_work()?;
                self.commit_ready_reports()?;
            }

            if self.root_terminal {
                continue;
            }
            if self.workers_enabled {
                if !self.active.is_empty() {
                    self.park()?;
                    continue;
                }
                if self.needs_quiescence() {
                    continue;
                }
                if self.parallel_work_count() > 0 {
                    if let Some(pool) = &self.pool {
                        if !pool.has_live() {
                            return Err(SchedulerError::new(
                                "the shared scheduler pool has no live worker",
                            ));
                        }
                    }
                }
                // Out-of-order commits can satisfy an older block
                // before its report enters the scheduler index.
                if self.scheduler.reconcile_parallel_states(self.world) {
                    continue;
                }
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
            if self.world.task_status(root) == TaskStatus::Terminal {
                self.root_terminal = true;
                self.scheduler.stop.get_or_insert(StopReason::RootTerminal);
                continue;
            }
            self.world
                .fail_blocked_task(root, "the scheduler found no runnable task");
            self.scheduler.refresh(self.world, root);
            self.scheduler.stop = Some(StopReason::Deadlock);
        }
    }

    fn dispatch_ready_work(&mut self) -> Result<(), SchedulerError> {
        while self.pool.is_some() && !self.needs_quiescence() {
            if let Some(drive) = self.drives.pop_front() {
                let lease = self.take_lease()?;
                let drive = drive
                    .with_quantum(self.parallel_snapshot_quantum(drive.task(), drive.quantum()));
                let step = self
                    .world
                    .begin_parallel_drive(drive, lease)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                self.accept_step(step, false)?;
                continue;
            }
            if let Some(pending) = self.continuations.pop_front() {
                let lease = self.take_lease()?;
                let step = self
                    .world
                    .continue_parallel_slice(pending.continuation, lease, false)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                self.accept_step(step, false)?;
                continue;
            }
            let stopped = self.stopped_tasks();
            let Some(task) = self.scheduler.next_parallel_ready(self.world, &stopped) else {
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
            let quantum = self.parallel_snapshot_quantum(task, u32::MAX);
            let step = self
                .world
                .begin_parallel_slice(task, quantum, lease)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.accept_step(step, true)?;
        }
        Ok(())
    }

    fn accept_step(
        &mut self,
        step: ParallelStep,
        starts_slice: bool,
    ) -> Result<(), SchedulerError> {
        match step {
            ParallelStep::Dispatch(dispatch) => {
                let (lease, job) = dispatch.into_parts();
                let pool = self
                    .pool
                    .as_ref()
                    .expect("parallel dispatch has a worker pool");
                let pool_job = match pool.dispatch(
                    lease,
                    self.scheduler.parallel_quantum(),
                    self.report_tx.clone(),
                ) {
                    Ok(pool_job) => pool_job,
                    Err((message, lease)) => {
                        drop(lease);
                        let _ = self.world.cancel_parallel_job(job);
                        return Err(SchedulerError::new(message));
                    }
                };
                self.active.insert(
                    pool_job,
                    ActiveJob {
                        job,
                        starts_slice,
                        recall_requested: false,
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
            ParallelStep::ArmWait(wait) => {
                if starts_slice {
                    self.world.note_parallel_report(0, 0, true);
                }
                let task = wait.task();
                let key = wait.wait_set();
                self.finish_slice(task, Some(SliceExit::Parked(key)));
                self.drives.extend(wait.into_drives());
            }
            ParallelStep::DriveStopped(drive) => {
                self.scheduler.consume_events(self.world);
                self.scheduler.refresh(self.world, drive.task());
            }
        }
        Ok(())
    }

    fn drain_worker_reports(&mut self) -> Result<(), SchedulerError> {
        loop {
            let event = match self.reports.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(SchedulerError::new("the scheduler report channel closed"));
                }
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
                    if report.stopped_by_recall() {
                        self.scheduler.stats.worker_recalls =
                            self.scheduler.stats.worker_recalls.saturating_add(1);
                    }
                    self.scheduler.stats.local_continuations = self
                        .scheduler
                        .stats
                        .local_continuations
                        .saturating_add(report.local_continuations());
                    self.scheduler.stats.local_rotations = self
                        .scheduler
                        .stats
                        .local_rotations
                        .saturating_add(report.local_rotations());
                    self.scheduler.stats.worker_heap_growth_bytes = self
                        .scheduler
                        .stats
                        .worker_heap_growth_bytes
                        .saturating_add(growth as u64);
                    let returned = self
                        .world
                        .accept_parallel_report(active.job, report)
                        .map_err(|error| SchedulerError::new(error.to_string()))?;
                    self.world
                        .note_parallel_report(retired, growth, active.starts_slice);
                    let task = returned.task();
                    let stopped = returned.reached_boundary();
                    let retired = returned.retired_instructions();
                    let waiters: Vec<u32> = self
                        .snapshot_watches
                        .iter()
                        .filter_map(|(waiter, watch)| {
                            watch.members.contains(&task).then_some(*waiter)
                        })
                        .collect();
                    for waiter in waiters {
                        self.world
                            .note_parallel_snapshot_progress(waiter, retired, stopped);
                    }
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
            if requirement == ParallelRequirement::InvalidState {
                return Err(SchedulerError::new(
                    "the parallel scheduler found a cyclic policy chain",
                ));
            }
            let needs_quiescence = requirement == ParallelRequirement::Collection;
            if needs_quiescence && !self.active.is_empty() {
                if !self.commit_quiescence {
                    self.scheduler.stats.global_quiescence =
                        self.scheduler.stats.global_quiescence.saturating_add(1);
                    if requirement == ParallelRequirement::Collection {
                        self.scheduler.stats.collection_quiescence =
                            self.scheduler.stats.collection_quiescence.saturating_add(1);
                    }
                }
                self.commit_quiescence = true;
                self.recall_all_active();
            } else if self.active.is_empty() {
                self.commit_quiescence = false;
            }
            let needed = match requirement {
                ParallelRequirement::Machine(key) => Some(key),
                ParallelRequirement::Safepoint(key) => {
                    if self.scoped_stops.insert(key) {
                        self.scheduler.stats.scoped_safepoint_waits = self
                            .scheduler
                            .stats
                            .scoped_safepoint_waits
                            .saturating_add(1);
                    }
                    Some(key)
                }
                _ => None,
            };
            if let Some(key) = needed {
                self.recall_machine(key);
                if !self
                    .active
                    .values()
                    .any(|active| active.job.machine_key() == key)
                    && self.park_retained_machine(key)?
                {
                    continue;
                }
            }
            let front_ready = matches!(requirement, ParallelRequirement::Ready)
                || (needs_quiescence && self.active.is_empty());
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
                                ParallelRequirement::Machine(key)
                                | ParallelRequirement::Safepoint(key) => {
                                    returned.machine_key() == key
                                }
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
            if index == 0 {
                self.scoped_stops.clear();
            }
            match step {
                ParallelStep::Complete {
                    task: completed,
                    exit,
                } => {
                    debug_assert_eq!(task, completed);
                    self.finish_slice(completed, exit);
                }
                other => self.accept_step(other, false)?,
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
            // Another report can satisfy this task's block before
            // this report commits. Index the current machine state.
            self.scheduler.refresh(self.world, task);
        }
    }

    fn needs_quiescence(&self) -> bool {
        self.commit_quiescence
    }

    fn needs_global_recall(&self) -> bool {
        self.commit_quiescence
    }

    fn stopped_tasks(&self) -> BTreeSet<TaskKey> {
        let mut stopped = self.scoped_stops.clone();
        for wait in self.drive_waits_in_flight() {
            stopped.insert(wait.owner);
        }
        stopped
    }

    fn drive_waits_in_flight(&self) -> BTreeSet<WaitSetKey> {
        let mut waits = BTreeSet::new();
        waits.extend(self.drives.iter().map(ParallelDrive::wait_set));
        waits.extend(
            self.continuations
                .iter()
                .filter_map(|pending| pending.continuation.wait_set()),
        );
        waits.extend(
            self.active
                .values()
                .filter_map(|active| active.job.wait_set()),
        );
        waits.extend(self.returned.iter().filter_map(ParallelReturned::wait_set));
        waits
    }

    fn withdraw_ready_drives(&mut self) -> Result<(), SchedulerError> {
        let ready: BTreeSet<WaitSetKey> = self
            .drive_waits_in_flight()
            .into_iter()
            .filter(|wait| {
                matches!(
                    self.scheduler.tasks.get(&wait.owner),
                    Some(IndexedState::Queued(_))
                )
            })
            .collect();
        if ready.is_empty() {
            return Ok(());
        }
        self.drives
            .retain(|drive| !ready.contains(&drive.wait_set()));
        let mut retained = VecDeque::new();
        while let Some(pending) = self.continuations.pop_front() {
            if pending
                .continuation
                .wait_set()
                .is_some_and(|wait| ready.contains(&wait))
            {
                let parked = self
                    .world
                    .park_parallel_continuation(pending.continuation)
                    .map_err(|error| SchedulerError::new(error.to_string()))?;
                self.accept_parked(parked);
            } else {
                retained.push_back(pending);
            }
        }
        self.continuations = retained;
        let jobs: Vec<u64> = self
            .active
            .iter_mut()
            .filter_map(|(id, active)| {
                let wait = active.job.wait_set()?;
                if !ready.contains(&wait) || active.recall_requested {
                    return None;
                }
                active.recall_requested = true;
                Some(*id)
            })
            .collect();
        if let Some(pool) = &self.pool {
            pool.recall(&jobs);
        }
        Ok(())
    }

    fn accept_parked(&mut self, parked: ParallelParked) {
        match parked {
            ParallelParked::Task { task, exit } => self.finish_slice(task, exit),
            ParallelParked::Drive(drive) => {
                self.scheduler.consume_events(self.world);
                self.scheduler.refresh(self.world, drive.task());
            }
        }
    }

    fn parallel_snapshot_quantum(&self, task: TaskKey, requested: u32) -> u32 {
        self.snapshot_watches
            .values()
            .filter(|watch| watch.members.contains(&task))
            .fold(requested.max(1), |quantum, watch| {
                let cap = watch.remaining.min(u64::from(u32::MAX)) as u32;
                quantum.min(cap.max(1))
            })
    }

    fn sync_snapshot_watches(&mut self) -> Result<(), SchedulerError> {
        let records = self.world.parallel_snapshot_watchers();
        let current: BTreeMap<u32, (u32, u32, u64, bool)> = records
            .into_iter()
            .map(|(waiter, target, generation, remaining, retry)| {
                (waiter, (target, generation, remaining, retry))
            })
            .collect();
        self.snapshot_watches.retain(|waiter, watch| {
            current
                .get(waiter)
                .is_some_and(|(target, generation, remaining, retry)| {
                    !retry
                        && watch.target == *target
                        && watch.generation == *generation
                        && watch.remaining == *remaining
                })
        });
        for (waiter, (target, generation, remaining, retry)) in current {
            if retry || self.snapshot_watches.contains_key(&waiter) {
                continue;
            }
            let members = self
                .world
                .parallel_snapshot_members(target)
                .map_err(|code| {
                    SchedulerError::new(format!("snapshot wait membership failed with {code:?}"))
                })?
                .into_iter()
                .collect();
            self.snapshot_watches.insert(
                waiter,
                ParallelSnapshotWatch {
                    target,
                    generation,
                    remaining,
                    members,
                },
            );
        }
        Ok(())
    }

    fn recall_machine(&mut self, key: TaskKey) {
        let jobs: Vec<u64> = self
            .active
            .iter_mut()
            .filter_map(|(id, active)| {
                if active.job.machine_key() == key && !active.recall_requested {
                    active.recall_requested = true;
                    Some(*id)
                } else {
                    None
                }
            })
            .collect();
        if let Some(pool) = &self.pool {
            pool.recall(&jobs);
        }
    }

    fn recall_all_active(&mut self) {
        let jobs: Vec<u64> = self
            .active
            .iter_mut()
            .filter_map(|(id, active)| {
                if active.recall_requested {
                    None
                } else {
                    active.recall_requested = true;
                    Some(*id)
                }
            })
            .collect();
        if let Some(pool) = &self.pool {
            pool.recall(&jobs);
        }
    }

    fn park_retained_continuations(&mut self) -> Result<(), SchedulerError> {
        let mut retained = VecDeque::new();
        while let Some(pending) = self.continuations.pop_front() {
            retained.push_back(pending.continuation);
        }
        while let Some(continuation) = retained.pop_front() {
            let parked = self
                .world
                .park_parallel_continuation(continuation)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.accept_parked(parked);
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
                    ParallelStep::Continue(continuation) => {
                        self.continuations
                            .push_back(PendingContinuation { continuation });
                    }
                    ParallelStep::ArmWait(wait) => {
                        let task = wait.task();
                        let key = wait.wait_set();
                        self.finish_slice(task, Some(SliceExit::Parked(key)));
                        self.drives.extend(wait.into_drives());
                    }
                    ParallelStep::DriveStopped(drive) => {
                        self.scheduler.consume_events(self.world);
                        self.scheduler.refresh(self.world, drive.task());
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
        let continuation = take_continuation_for(&mut self.continuations, key.vm);
        let Some(continuation) = continuation else {
            return Ok(false);
        };
        let parked = self
            .world
            .park_parallel_continuation(continuation)
            .map_err(|error| SchedulerError::new(error.to_string()))?;
        self.accept_parked(parked);
        Ok(true)
    }

    fn parallel_work_count(&self) -> usize {
        let retained = self.continuations.len().saturating_add(self.drives.len());
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
            let pool = SchedulerPool::new(self.worker_count)?;
            self.pool_registration = Some(pool.register_wake(Arc::clone(&self.notifier)));
            self.pool = Some(pool);
        }
        Ok(())
    }

    fn run_one_serial_slice(&mut self) -> Result<Option<bool>, SchedulerError> {
        if let Some(pending) = self.continuations.pop_front() {
            let task = pending.continuation.task();
            let before = self.world.metrics();
            let exit = self
                .world
                .run_parallel_continuation_inline(pending.continuation)
                .map_err(|error| SchedulerError::new(error.to_string()))?;
            self.finish_serial_slice(task, exit);
            let after = self.world.metrics();
            return Ok(Some(
                exit == Some(SliceExit::Yielded)
                    && after.boundary_exits.saturating_sub(before.boundary_exits) <= 1,
            ));
        }
        let Some(task) = self.scheduler.next_ready(self.world) else {
            return Ok(None);
        };
        if task.vm == self.world.root() {
            self.scheduler.stats.root_slices = self.scheduler.stats.root_slices.saturating_add(1);
        } else {
            self.scheduler.stats.proc_slices = self.scheduler.stats.proc_slices.saturating_add(1);
        }
        self.scheduler.tasks.insert(task, IndexedState::Running);
        let configured = self.scheduler.quantum;
        let quantum = self.world.snapshot_wait_quantum(task, configured);
        let metrics_before = self.world.metrics();
        let before = self.world.world_fuel();
        let heap_before = self.world.heap_of(task.vm).used_bytes();
        let exit = self.world.drive_slice(task, quantum);
        let retired = before.saturating_sub(self.world.world_fuel());
        self.world
            .note_scheduler_slice(task, retired, exit.is_some(), heap_before);
        self.finish_serial_slice(task, exit);
        let metrics_after = self.world.metrics();
        Ok(Some(
            exit == Some(SliceExit::Yielded)
                && retired == u64::from(quantum)
                && metrics_after
                    .boundary_exits
                    .saturating_sub(metrics_before.boundary_exits)
                    <= 1,
        ))
    }

    fn finish_serial_slice(&mut self, task: TaskKey, exit: Option<SliceExit>) {
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
        self.recall_all_active();
        while !self.active.is_empty() {
            match self.reports.recv() {
                Ok(WorkerEvent::Report { job: id, report }) => {
                    if let Some(active) = self.active.remove(&id) {
                        let _ = self.world.accept_parallel_report(active.job, report);
                    }
                }
                Ok(WorkerEvent::Failed { job: id }) => {
                    if let Some(active) = self.active.remove(&id) {
                        let _ = self.world.cancel_parallel_job(active.job);
                    }
                }
                Err(_) => break,
            }
        }
        let remaining = std::mem::take(&mut self.active);
        for (_, active) in remaining {
            let _ = self.world.cancel_parallel_job(active.job);
        }
    }
}

impl Scheduler {
    fn has_wide_parallel_wait(&self, world: &World) -> bool {
        self.tasks.iter().any(|(task, state)| {
            matches!(state, IndexedState::Queued(_)) && world.parallel_drive_width(*task) > 1
        })
    }

    fn next_parallel_ready(
        &mut self,
        world: &World,
        stopped: &BTreeSet<TaskKey>,
    ) -> Option<TaskKey> {
        let available = self.ready.len();
        for _ in 0..available {
            let Some((task, ticket)) = self.ready.pop_front() else {
                break;
            };
            if self.tasks.get(&task) != Some(&IndexedState::Queued(ticket)) {
                continue;
            }
            match world.task_status(task) {
                TaskStatus::Ready if stopped.contains(&task) => {
                    self.ready.push_back((task, ticket));
                }
                TaskStatus::Ready => return Some(task),
                status => self.index_status(world, task, status),
            }
        }
        None
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
