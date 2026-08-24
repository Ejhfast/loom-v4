//! The bounded execution worker pool.

use lm_vm::{execute, ExecutionLease, ExecutionReport};
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::WORKER_STACK;

enum WorkerCommand {
    Execute { job: u64, lease: ExecutionLease },
    Shutdown,
}

pub(crate) enum WorkerEvent {
    Report { job: u64, report: ExecutionReport },
    Failed { job: u64 },
}

struct RoutedEvent {
    worker: usize,
    event: WorkerEvent,
}

pub(crate) struct WorkerPool {
    commands: Vec<Sender<WorkerCommand>>,
    reports: Receiver<RoutedEvent>,
    threads: Vec<Option<JoinHandle<()>>>,
    live: Vec<bool>,
    idle: VecDeque<usize>,
}

impl WorkerPool {
    pub(crate) fn new(
        workers: usize,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<WorkerPool, String> {
        if workers == 0 {
            return Err("the parallel worker count is zero".to_string());
        }
        let (report_tx, reports) = mpsc::channel();
        let mut commands: Vec<Sender<WorkerCommand>> = Vec::with_capacity(workers);
        let mut threads: Vec<Option<JoinHandle<()>>> = Vec::with_capacity(workers);
        for worker in 0..workers {
            let (command_tx, command_rx) = mpsc::channel();
            let report_tx = report_tx.clone();
            let wake = Arc::clone(&wake);
            let thread = match std::thread::Builder::new()
                .name(format!("loom-worker-{worker}"))
                .stack_size(WORKER_STACK)
                .spawn(move || worker_loop(worker, command_rx, report_tx, wake))
            {
                Ok(thread) => thread,
                Err(error) => {
                    for command in &commands {
                        let _ = command.send(WorkerCommand::Shutdown);
                    }
                    for thread in threads.into_iter().flatten() {
                        let _ = thread.join();
                    }
                    return Err(format!("the scheduler worker did not start: {error}"));
                }
            };
            commands.push(command_tx);
            threads.push(Some(thread));
        }
        Ok(WorkerPool {
            commands,
            reports,
            threads,
            live: vec![true; workers],
            idle: (0..workers).collect(),
        })
    }

    pub(crate) fn has_idle(&self) -> bool {
        !self.idle.is_empty()
    }

    pub(crate) fn dispatch(
        &mut self,
        job: u64,
        lease: ExecutionLease,
    ) -> Result<(), (String, ExecutionLease)> {
        let Some(worker) = self.idle.pop_front() else {
            return Err(("the scheduler has no idle worker".to_string(), lease));
        };
        if let Err(error) = self.commands[worker].send(WorkerCommand::Execute { job, lease }) {
            self.live[worker] = false;
            let WorkerCommand::Execute { lease, .. } = error.0 else {
                unreachable!("the failed command contains one execution lease")
            };
            return Err((
                "the scheduler worker command channel closed".to_string(),
                lease,
            ));
        }
        Ok(())
    }

    pub(crate) fn try_event(&mut self) -> Result<Option<WorkerEvent>, String> {
        match self.reports.try_recv() {
            Ok(event) => Ok(Some(self.accept_event(event))),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("the scheduler report channel closed".to_string())
            }
        }
    }

    fn accept_event(&mut self, routed: RoutedEvent) -> WorkerEvent {
        if matches!(routed.event, WorkerEvent::Report { .. }) {
            self.idle.push_back(routed.worker);
        } else {
            self.live[routed.worker] = false;
        }
        routed.event
    }

    pub(crate) fn shutdown(&mut self) {
        for (worker, command) in self.commands.iter().enumerate() {
            if self.live[worker] {
                let _ = command.send(WorkerCommand::Shutdown);
            }
        }
        for thread in &mut self.threads {
            if let Some(thread) = thread.take() {
                let _ = thread.join();
            }
        }
        self.idle.clear();
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    worker: usize,
    commands: Receiver<WorkerCommand>,
    reports: Sender<RoutedEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Shutdown => return,
            WorkerCommand::Execute { job, lease } => {
                let result = catch_unwind(AssertUnwindSafe(|| execute(lease)));
                let failed = result.is_err();
                let event = match result {
                    Ok(report) => WorkerEvent::Report { job, report },
                    Err(_) => WorkerEvent::Failed { job },
                };
                if reports.send(RoutedEvent { worker, event }).is_err() {
                    return;
                }
                wake();
                if failed {
                    return;
                }
            }
        }
    }
}
