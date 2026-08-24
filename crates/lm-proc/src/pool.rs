//! The bounded execution worker pool.

use lm_vm::{execute, ExecutionLease, ExecutionReport};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use crate::{SchedulerError, MAX_PARALLEL_WORKERS, WORKER_STACK};

enum WorkerCommand {
    Execute {
        job: u64,
        lease: ExecutionLease,
        reports: Sender<WorkerEvent>,
    },
    Shutdown,
}

pub(crate) enum WorkerEvent {
    Report { job: u64, report: ExecutionReport },
    Failed { job: u64 },
}

struct PoolState {
    idle: VecDeque<usize>,
    live: Vec<bool>,
    waiters: BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>,
    next_waiter: u64,
}

struct PoolShared {
    state: Mutex<PoolState>,
    available: Condvar,
}

struct PoolInner {
    commands: Vec<Sender<WorkerCommand>>,
    threads: Vec<Option<JoinHandle<()>>>,
    shared: Arc<PoolShared>,
}

/// A fixed worker pool that can serve several schedulers.
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
                idle: VecDeque::new(),
                live: vec![false; workers],
                waiters: BTreeMap::new(),
                next_waiter: 1,
            }),
            available: Condvar::new(),
        });
        let mut commands: Vec<Sender<WorkerCommand>> = Vec::with_capacity(workers);
        let mut threads: Vec<Option<JoinHandle<()>>> = Vec::with_capacity(workers);
        for worker in 0..workers {
            let (command_tx, command_rx) = mpsc::channel();
            let worker_state = Arc::clone(&shared);
            let thread = match std::thread::Builder::new()
                .name(format!("loom-worker-{worker}"))
                .stack_size(WORKER_STACK)
                .spawn(move || worker_loop(worker, command_rx, worker_state))
            {
                Ok(thread) => thread,
                Err(error) => {
                    for command in &commands {
                        let _ = command.send(WorkerCommand::Shutdown);
                    }
                    for thread in threads.into_iter().flatten() {
                        let _ = thread.join();
                    }
                    return Err(SchedulerError::new(format!(
                        "the scheduler worker did not start: {error}"
                    )));
                }
            };
            commands.push(command_tx);
            threads.push(Some(thread));
            let mut state = shared
                .state
                .lock()
                .expect("the worker pool state is available");
            state.live[worker] = true;
            state.idle.push_back(worker);
        }
        Ok(SchedulerPool {
            inner: Arc::new(PoolInner {
                commands,
                threads,
                shared,
            }),
        })
    }

    /// The fixed worker count.
    pub fn worker_count(&self) -> usize {
        self.inner.commands.len()
    }

    pub(crate) fn has_idle(&self) -> bool {
        !self
            .inner
            .shared
            .state
            .lock()
            .expect("the worker pool state is available")
            .idle
            .is_empty()
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

    pub(crate) fn dispatch(
        &self,
        job: u64,
        lease: ExecutionLease,
        reports: Sender<WorkerEvent>,
    ) -> Result<(), (String, ExecutionLease)> {
        let mut state = self
            .inner
            .shared
            .state
            .lock()
            .expect("the worker pool state is available");
        while state.idle.is_empty() && state.live.iter().any(|live| *live) {
            state = self
                .inner
                .shared
                .available
                .wait(state)
                .expect("the worker pool state is available");
        }
        let worker = state.idle.pop_front();
        drop(state);
        let Some(worker) = worker else {
            return Err(("the scheduler has no idle worker".to_string(), lease));
        };
        if let Err(error) = self.inner.commands[worker].send(WorkerCommand::Execute {
            job,
            lease,
            reports,
        }) {
            let WorkerCommand::Execute { lease, .. } = error.0 else {
                unreachable!("the failed command contains one execution lease")
            };
            let waiters = mark_worker_failed(&self.inner.shared, worker);
            wake_all(waiters);
            return Err((
                "the scheduler worker command channel closed".to_string(),
                lease,
            ));
        }
        Ok(())
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
        for command in &self.commands {
            let _ = command.send(WorkerCommand::Shutdown);
        }
        for thread in &mut self.threads {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
    }
}

fn worker_loop(worker: usize, commands: Receiver<WorkerCommand>, shared: Arc<PoolShared>) {
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Shutdown => return,
            WorkerCommand::Execute {
                job,
                lease,
                reports,
            } => {
                let result = catch_unwind(AssertUnwindSafe(|| execute(lease)));
                let failed = result.is_err();
                let event = match result {
                    Ok(report) => WorkerEvent::Report { job, report },
                    Err(_) => WorkerEvent::Failed { job },
                };
                let _ = reports.send(event);
                let waiters = if failed {
                    mark_worker_failed(&shared, worker)
                } else {
                    mark_worker_idle(&shared, worker)
                };
                wake_all(waiters);
                if failed {
                    return;
                }
            }
        }
    }
}

fn mark_worker_idle(shared: &Arc<PoolShared>, worker: usize) -> Vec<Arc<dyn Fn() + Send + Sync>> {
    let mut state = shared
        .state
        .lock()
        .expect("the worker pool state is available");
    if state.live.get(worker) == Some(&true) {
        state.idle.push_back(worker);
    }
    let waiters = state.waiters.values().cloned().collect();
    drop(state);
    shared.available.notify_all();
    waiters
}

fn mark_worker_failed(shared: &Arc<PoolShared>, worker: usize) -> Vec<Arc<dyn Fn() + Send + Sync>> {
    let mut state = shared
        .state
        .lock()
        .expect("the worker pool state is available");
    if let Some(live) = state.live.get_mut(worker) {
        *live = false;
    }
    let waiters = state.waiters.values().cloned().collect();
    drop(state);
    shared.available.notify_all();
    waiters
}

fn wake_all(waiters: Vec<Arc<dyn Fn() + Send + Sync>>) {
    for wake in waiters {
        wake();
    }
}
