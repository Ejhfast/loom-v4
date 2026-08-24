//! The bounded execution worker pool.

use lm_vm::{execute_turn, recall, ExecutionLease, ExecutionReport, ExecutionTurn};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::{SchedulerError, MAX_PARALLEL_WORKERS, WORKER_STACK};

struct WorkerJob {
    id: u64,
    lease: ExecutionLease,
    quantum: u32,
    reports: Sender<WorkerEvent>,
}

pub(crate) enum WorkerEvent {
    Report { job: u64, report: ExecutionReport },
    Failed { job: u64 },
}

struct PoolState {
    queue: VecDeque<WorkerJob>,
    running: Vec<Option<u64>>,
    live: Vec<bool>,
    recalls: BTreeSet<u64>,
    waiters: BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>,
    next_waiter: u64,
    next_job: u64,
    shutdown: bool,
}

struct PoolShared {
    state: Mutex<PoolState>,
    available: Condvar,
}

struct PoolInner {
    threads: Vec<Option<JoinHandle<()>>>,
    shared: Arc<PoolShared>,
}

/// A fixed worker pool with one shared FIFO run queue.
#[derive(Clone)]
pub struct SchedulerPool {
    inner: Arc<PoolInner>,
}

impl fmt::Debug for SchedulerPool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchedulerPool")
            .field("workers", &self.worker_count())
            .finish_non_exhaustive()
    }
}

impl SchedulerPool {
    /// Start one bounded worker pool.
    pub fn new(workers: usize) -> Result<SchedulerPool, SchedulerError> {
        if workers == 0 || workers > MAX_PARALLEL_WORKERS {
            return Err(SchedulerError::new(
                "the parallel worker count must be between 1 and 256",
            ));
        }
        let shared = Arc::new(PoolShared {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                running: vec![None; workers],
                live: vec![false; workers],
                recalls: BTreeSet::new(),
                waiters: BTreeMap::new(),
                next_waiter: 1,
                next_job: 1,
                shutdown: false,
            }),
            available: Condvar::new(),
        });
        let mut threads: Vec<Option<JoinHandle<()>>> = Vec::with_capacity(workers);
        for worker in 0..workers {
            let worker_state = Arc::clone(&shared);
            let thread = match std::thread::Builder::new()
                .name(format!("loom-worker-{worker}"))
                .stack_size(WORKER_STACK)
                .spawn(move || worker_loop(worker, worker_state))
            {
                Ok(thread) => thread,
                Err(error) => {
                    {
                        let mut state = shared
                            .state
                            .lock()
                            .expect("the worker pool state is available");
                        state.shutdown = true;
                    }
                    shared.available.notify_all();
                    for thread in threads.into_iter().flatten() {
                        let _ = thread.join();
                    }
                    return Err(SchedulerError::new(format!(
                        "the scheduler worker did not start: {error}"
                    )));
                }
            };
            threads.push(Some(thread));
            let mut state = shared
                .state
                .lock()
                .expect("the worker pool state is available");
            state.live[worker] = true;
        }
        Ok(SchedulerPool {
            inner: Arc::new(PoolInner { threads, shared }),
        })
    }

    /// The fixed worker count.
    pub fn worker_count(&self) -> usize {
        self.inner.threads.len()
    }

    pub(crate) fn has_live(&self) -> bool {
        self.inner
            .shared
            .state
            .lock()
            .expect("the worker pool state is available")
            .live
            .iter()
            .any(|live| *live)
    }

    pub(crate) fn register_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) -> PoolRegistration {
        let mut state = self
            .inner
            .shared
            .state
            .lock()
            .expect("the worker pool state is available");
        let id = state.next_waiter;
        state.next_waiter = state
            .next_waiter
            .checked_add(1)
            .expect("the pool waiter identity space is available");
        state.waiters.insert(id, wake);
        PoolRegistration {
            shared: Arc::clone(&self.inner.shared),
            id,
        }
    }

    /// Queue one machine lease and return its pool job identity.
    pub(crate) fn dispatch(
        &self,
        lease: ExecutionLease,
        quantum: u32,
        reports: Sender<WorkerEvent>,
    ) -> Result<u64, (String, Box<ExecutionLease>)> {
        let mut state = self
            .inner
            .shared
            .state
            .lock()
            .expect("the worker pool state is available");
        if state.shutdown || !state.live.iter().any(|live| *live) {
            return Err((
                "the scheduler has no live worker".to_string(),
                Box::new(lease),
            ));
        }
        let id = state.next_job;
        let Some(next) = state.next_job.checked_add(1) else {
            return Err((
                "the scheduler pool job identity space is exhausted".to_string(),
                Box::new(lease),
            ));
        };
        state.next_job = next;
        state.queue.push_back(WorkerJob {
            id,
            lease,
            quantum: quantum.max(1),
            reports,
        });
        drop(state);
        self.inner.shared.available.notify_one();
        Ok(id)
    }

    /// Recall exact pool jobs to their owning coordinators.
    pub(crate) fn recall(&self, jobs: &[u64]) {
        if jobs.is_empty() {
            return;
        }
        let requested: BTreeSet<u64> = jobs.iter().copied().collect();
        let (queued, waiters) = {
            let mut state = self
                .inner
                .shared
                .state
                .lock()
                .expect("the worker pool state is available");
            let mut queued = Vec::new();
            let mut retained = VecDeque::with_capacity(state.queue.len());
            while let Some(job) = state.queue.pop_front() {
                if requested.contains(&job.id) {
                    queued.push(job);
                } else {
                    retained.push_back(job);
                }
            }
            state.queue = retained;
            let running: Vec<u64> = state
                .running
                .iter()
                .flatten()
                .filter(|job| requested.contains(job))
                .copied()
                .collect();
            state.recalls.extend(running);
            let waiters = state.waiters.values().cloned().collect();
            (queued, waiters)
        };
        for job in queued {
            let _ = job.reports.send(WorkerEvent::Report {
                job: job.id,
                report: recall(job.lease),
            });
        }
        wake_all(waiters);
    }
}

pub(crate) struct PoolRegistration {
    shared: Arc<PoolShared>,
    id: u64,
}

impl Drop for PoolRegistration {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .expect("the worker pool state is available");
        state.waiters.remove(&self.id);
    }
}

impl Drop for PoolInner {
    fn drop(&mut self) {
        let queued = {
            let mut state = self
                .shared
                .state
                .lock()
                .expect("the worker pool state is available");
            state.shutdown = true;
            state.queue.drain(..).collect::<Vec<_>>()
        };
        for job in queued {
            let _ = job.reports.send(WorkerEvent::Failed { job: job.id });
        }
        self.shared.available.notify_all();
        for thread in &mut self.threads {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn worker_loop(worker: usize, shared: Arc<PoolShared>) {
    let Some(mut job) = take_job(&shared, worker) else {
        return;
    };
    loop {
        let result = catch_unwind(AssertUnwindSafe(|| execute_turn(job.lease, job.quantum)));
        match result {
            Ok(ExecutionTurn::Continue(mut lease)) => {
                let recalled = {
                    let mut state = shared
                        .state
                        .lock()
                        .expect("the worker pool state is available");
                    let recalled = state.shutdown || state.recalls.remove(&job.id);
                    if recalled {
                        state.running[worker] = None;
                    } else if let Some(next) = state.queue.pop_front() {
                        lease.note_local_rotation();
                        job.lease = lease;
                        state.queue.push_back(job);
                        state.running[worker] = Some(next.id);
                        drop(state);
                        job = next;
                        continue;
                    } else {
                        lease.note_local_continuation();
                        job.lease = lease;
                        drop(state);
                        continue;
                    }
                    recalled
                };
                debug_assert!(recalled);
                let _ = job.reports.send(WorkerEvent::Report {
                    job: job.id,
                    report: recall(lease),
                });
                wake_pool_waiters(&shared);
            }
            Ok(ExecutionTurn::Report(report)) => {
                {
                    let mut state = shared
                        .state
                        .lock()
                        .expect("the worker pool state is available");
                    state.recalls.remove(&job.id);
                    state.running[worker] = None;
                }
                let _ = job.reports.send(WorkerEvent::Report {
                    job: job.id,
                    report,
                });
                wake_pool_waiters(&shared);
            }
            Err(_) => {
                let _ = job.reports.send(WorkerEvent::Failed { job: job.id });
                let stranded = mark_worker_failed(&shared, worker);
                for job in stranded {
                    let _ = job.reports.send(WorkerEvent::Failed { job: job.id });
                }
                wake_pool_waiters(&shared);
                return;
            }
        }
        let Some(next) = take_job(&shared, worker) else {
            return;
        };
        job = next;
    }
}

fn take_job(shared: &Arc<PoolShared>, worker: usize) -> Option<WorkerJob> {
    let mut state = shared
        .state
        .lock()
        .expect("the worker pool state is available");
    loop {
        if state.shutdown {
            state.running[worker] = None;
            return None;
        }
        if let Some(job) = state.queue.pop_front() {
            state.running[worker] = Some(job.id);
            return Some(job);
        }
        state.running[worker] = None;
        state = shared
            .available
            .wait(state)
            .expect("the worker pool state is available");
    }
}

fn mark_worker_failed(shared: &Arc<PoolShared>, worker: usize) -> Vec<WorkerJob> {
    let mut state = shared
        .state
        .lock()
        .expect("the worker pool state is available");
    state.running[worker] = None;
    state.live[worker] = false;
    if state.live.iter().any(|live| *live) {
        Vec::new()
    } else {
        state.queue.drain(..).collect()
    }
}

fn wake_pool_waiters(shared: &Arc<PoolShared>) {
    let waiters = shared
        .state
        .lock()
        .expect("the worker pool state is available")
        .waiters
        .values()
        .cloned()
        .collect();
    shared.available.notify_all();
    wake_all(waiters);
}

fn wake_all(waiters: Vec<Arc<dyn Fn() + Send + Sync>>) {
    for wake in waiters {
        wake();
    }
}
