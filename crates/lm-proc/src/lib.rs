//! The proc scheduler and the snapshot barrier.
//!
//! This crate sits above `lm-vm` in the dependency order of the build
//! order, section 1. It owns the scheduling policy: the run order, the
//! progress rule, the deadlock rule, and the barrier algorithm. The
//! machine records, the mailboxes, and the proc operations live below
//! it, in `lm-vm`.
//!
//! A scheduler record names a machine by identifier and generation. It
//! never holds a guest heap reference, so a snapshot rebuilds the
//! scheduler from the machines it copied.
//!
//! One VM never executes concurrently. The deterministic mode drives
//! one machine at a time in ascending identifier order, so two runs of
//! one program produce the same interleaving and the same result.

mod barrier;

pub use barrier::{Barrier, BarrierError, BarrierReport};

use lm_vm::{Outcome, ProcStop, RootEvent, VmId, World};

/// The stack of one scheduler worker thread.
///
/// The project precedent is 8 MiB, and the interpreter never grows
/// the Rust stack with guest call depth, so the bound is generous.
pub const WORKER_STACK: usize = 8 << 20;

/// How the scheduler picks the next machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerMode {
    /// Drive the root, then the runnable procs in ascending
    /// identifier order. Every run of one program produces the same
    /// interleaving.
    #[default]
    Deterministic,
}

/// Why one scheduler run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The root machine reached a terminal result.
    RootTerminal,
    /// No machine could make progress. Every blocked machine faulted.
    Deadlock,
}

/// The counters of one scheduler run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    /// How often the scheduler drove the root stack.
    pub root_slices: u64,
    /// How often the scheduler drove one proc.
    pub proc_slices: u64,
    /// How many blocks the scheduler completed.
    pub unblocked: u64,
}

/// The proc scheduler.
pub struct Scheduler {
    mode: SchedulerMode,
    stats: SchedulerStats,
    stop: Option<StopReason>,
}

impl Default for Scheduler {
    fn default() -> Scheduler {
        Scheduler::new(SchedulerMode::Deterministic)
    }
}

impl Scheduler {
    pub fn new(mode: SchedulerMode) -> Scheduler {
        Scheduler {
            mode,
            stats: SchedulerStats::default(),
            stop: None,
        }
    }

    pub fn mode(&self) -> SchedulerMode {
        self.mode
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats
    }

    /// Why the last run stopped.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop
    }

    /// Run one world to the terminal result of its root machine.
    ///
    /// The loop drives the root stack first. When the root blocks on
    /// another machine, the scheduler completes every block that can
    /// complete, then drives one runnable proc. A world with no
    /// runnable machine and a blocked root is a deadlock: every
    /// blocked machine faults, and the root fault becomes the result.
    pub fn run(&mut self, world: &mut World<'_>) -> Outcome {
        self.stop = None;
        loop {
            self.stats.root_slices += 1;
            match world.drive_root() {
                RootEvent::Done(value) => {
                    self.stop.get_or_insert(StopReason::RootTerminal);
                    return Outcome::Done(value);
                }
                RootEvent::Fault(rec) => {
                    self.stop.get_or_insert(StopReason::RootTerminal);
                    return Outcome::Fault(rec.code);
                }
                RootEvent::Blocked => {}
                other => unreachable!("the scheduler runs the root to a terminal: {other:?}"),
            }
            if !self.progress(world) {
                self.stop = Some(StopReason::Deadlock);
                self.fail_every_block(world);
            }
        }
    }

    /// Make one unit of progress somewhere other than the root stack.
    /// Return false when no machine can move.
    fn progress(&mut self, world: &mut World<'_>) -> bool {
        let released = world.poll_blocked();
        if released > 0 {
            self.stats.unblocked += released as u64;
            return true;
        }
        let Some(proc) = self.next_proc(world) else {
            return false;
        };
        self.stats.proc_slices += 1;
        // One machine runs at a time, so no VM ever executes
        // concurrently with another.
        world.drive_proc(proc);
        true
    }

    /// The next proc to drive.
    fn next_proc(&self, world: &World<'_>) -> Option<VmId> {
        match self.mode {
            // Ascending identifier order is the whole policy: it is
            // total, it depends on no clock, and it repeats.
            SchedulerMode::Deterministic => world.runnable_procs().first().copied(),
        }
    }

    /// Fault every blocked machine of a deadlocked world.
    ///
    /// Specification 18.6 makes a deadlock a policy decision. The
    /// deterministic scheduler is the supervisor here, and it converts
    /// the condition into one fault per blocked machine.
    fn fail_every_block(&mut self, world: &mut World<'_>) {
        for vm in world.blocked_machines() {
            world.fail_blocked(vm, "the scheduler found no runnable machine");
        }
    }
}

/// Run one world to its root terminal result with the deterministic
/// scheduler.
pub fn run_world(world: &mut World<'_>) -> Outcome {
    Scheduler::default().run(world)
}

/// Run one program on a worker thread with a bounded stack.
///
/// This is the thread-backed baseline of specification 22.12. Guest
/// execution leaves the host thread: the whole machine world is
/// built, driven, and dropped inside one worker with an 8 MiB stack,
/// and only the rendered outcome comes back. Nothing of the world
/// crosses the boundary, so no guest reference ever leaves it.
///
/// The world is not shared, so this mode changes no semantics. One VM
/// still never executes concurrently, because one worker owns the
/// whole world.
pub fn run_on_worker(
    loaded: &lm_vm::LoadedModule,
    config: lm_vm::VmConfig,
    grants: &[&str],
    host: Box<dyn FnOnce() -> Box<dyn lm_vm::Host> + Send>,
) -> Result<WorkerOutcome, String> {
    std::thread::scope(|scope| {
        let worker = std::thread::Builder::new()
            .stack_size(WORKER_STACK)
            .spawn_scoped(scope, move || {
                let mut world = World::new(loaded, config, host());
                for grant in grants {
                    world.allow(grant)?;
                }
                let outcome = Scheduler::default().run(&mut world);
                Ok(WorkerOutcome {
                    faulted: matches!(outcome, Outcome::Fault(_)),
                    text: world.show_outcome(&outcome),
                })
            })
            .map_err(|e| format!("the worker thread did not start: {e}"))?;
        worker
            .join()
            .map_err(|_| "the worker thread panicked".to_string())?
    })
}

/// What one worker run reports back.
///
/// A terminal value belongs to the heap that produced it, so the
/// worker renders it and returns text. Nothing of the machine world
/// crosses the thread boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerOutcome {
    /// True when the root machine faulted.
    pub faulted: bool,
    /// The stable outcome text, for example `Done(42)`.
    pub text: String,
}

/// Drive one proc slice at a time until nothing is runnable.
///
/// Tests use it to bring every proc to a stop without touching the
/// root machine.
pub fn drain_procs(world: &mut World<'_>) -> usize {
    let mut slices = 0;
    loop {
        if world.poll_blocked() > 0 {
            continue;
        }
        let Some(proc) = world.runnable_procs().first().copied() else {
            return slices;
        };
        slices += 1;
        if world.drive_proc(proc) == ProcStop::Waiting {
            // A host completion is outside the scheduler. The caller
            // drives it.
            return slices;
        }
    }
}
