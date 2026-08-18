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
//! One VM never executes concurrently. The deterministic mode uses
//! one FIFO ready queue and one fixed instruction quantum.
//!
//! At reset, the root enters first. Active procs follow in `TaskKey`
//! order. A nonterminal slice enqueues its events before itself. A
//! terminal slice wakes its waiters before it enqueues new tasks.
//! Waiters for one wake key enter in `TaskKey` order.

mod barrier;

pub use barrier::{Barrier, BarrierError, BarrierReport};

use lm_vm::{
    CompletionKey, Outcome, ScheduleEvents, SliceExit, TaskKey, TaskStatus, WaitSetKey,
    WaitSourceKey, WakeKey, World,
};
use std::collections::{BTreeMap, VecDeque};

/// The stack of one scheduler worker thread.
///
/// The project precedent is 8 MiB, and the interpreter never grows
/// the Rust stack with guest call depth, so the bound is generous.
pub const WORKER_STACK: usize = 8 << 20;

/// The default guest instruction count of one scheduler slice.
pub const DEFAULT_QUANTUM: u32 = 1_024;

/// How the scheduler picks the next machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerMode {
    /// Use FIFO readiness and stable wake ordering.
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
    quantum: u32,
    include_root: bool,
    stats: SchedulerStats,
    stop: Option<StopReason>,
    ready: VecDeque<(TaskKey, u128)>,
    /// A ticket makes an old queue entry stay stale after requeue.
    /// One run has a `u64` fuel bound, so this counter cannot wrap.
    ready_ticket: u128,
    tasks: TaskIndex,
    blocked: WakeIndex,
    /// The extra wake keys of a task that waits on more than one
    /// condition. A driver waits on its own condition and on every
    /// surface it drives, so a descendant request reaches it.
    block_keys: BTreeMap<TaskKey, Vec<WakeKey>>,
    waiting: BTreeMap<CompletionKey, TaskKey>,
    parked: ParkIndex,
    events: ScheduleEvents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexedState {
    Queued(u128),
    Running,
    Blocked(WakeKey),
    Waiting(CompletionKey),
    Parked(WaitSetKey),
}

/// Scheduler task states indexed by dense machine identifier.
#[derive(Debug, Default)]
struct TaskIndex {
    slots: Vec<Option<IndexedTask>>,
}

#[derive(Debug, Clone, Copy)]
struct IndexedTask {
    key: TaskKey,
    state: IndexedState,
}

/// Blocked tasks indexed by the dense target machine identifier.
#[derive(Debug, Default)]
struct WakeIndex {
    slots: Vec<Vec<WakeBucket>>,
}

#[derive(Debug)]
struct WakeBucket {
    wake: WakeKey,
    waiters: Waiters,
}

/// Typed wait sets indexed by all current scheduler sources.
#[derive(Debug, Default)]
struct ParkIndex {
    sets: BTreeMap<WaitSetKey, Vec<WaitSourceKey>>,
    wakes: WakeIndex,
    completions: BTreeMap<CompletionKey, Waiters>,
}

#[derive(Debug)]
enum Waiters {
    One(TaskKey),
    Many(Vec<TaskKey>),
}

impl Waiters {
    fn insert(&mut self, task: TaskKey) {
        match self {
            Waiters::One(held) if *held == task => {}
            Waiters::One(held) => {
                let mut tasks = vec![*held, task];
                tasks.sort_unstable();
                *self = Waiters::Many(tasks);
            }
            Waiters::Many(tasks) => {
                if let Err(at) = tasks.binary_search(&task) {
                    tasks.insert(at, task);
                }
            }
        }
    }

    fn remove(&mut self, task: TaskKey) -> bool {
        match self {
            Waiters::One(held) => *held == task,
            Waiters::Many(tasks) => {
                let Ok(at) = tasks.binary_search(&task) else {
                    return false;
                };
                tasks.remove(at);
                if tasks.len() == 1 {
                    *self = Waiters::One(tasks[0]);
                }
                false
            }
        }
    }

    fn for_each(self, mut visit: impl FnMut(TaskKey)) {
        match self {
            Waiters::One(task) => visit(task),
            Waiters::Many(tasks) => tasks.into_iter().for_each(visit),
        }
    }
}

impl WakeIndex {
    fn clear(&mut self) {
        self.slots.clear();
    }

    fn insert(&mut self, wake: WakeKey, task: TaskKey) {
        let slot = wake_task(wake).vm as usize;
        if self.slots.len() <= slot {
            self.slots.resize_with(slot + 1, Vec::new);
        }
        let buckets = &mut self.slots[slot];
        match buckets.binary_search_by_key(&wake, |bucket| bucket.wake) {
            Ok(at) => buckets[at].waiters.insert(task),
            Err(at) => buckets.insert(
                at,
                WakeBucket {
                    wake,
                    waiters: Waiters::One(task),
                },
            ),
        }
    }

    fn remove(&mut self, wake: WakeKey, task: TaskKey) {
        let Some(buckets) = self.slots.get_mut(wake_task(wake).vm as usize) else {
            return;
        };
        let Ok(at) = buckets.binary_search_by_key(&wake, |bucket| bucket.wake) else {
            return;
        };
        if buckets[at].waiters.remove(task) {
            buckets.remove(at);
        }
    }

    fn take(&mut self, wake: WakeKey) -> Option<Waiters> {
        let buckets = self.slots.get_mut(wake_task(wake).vm as usize)?;
        let at = buckets
            .binary_search_by_key(&wake, |bucket| bucket.wake)
            .ok()?;
        Some(buckets.remove(at).waiters)
    }
}

impl ParkIndex {
    fn clear(&mut self) {
        self.sets.clear();
        self.wakes.clear();
        self.completions.clear();
    }

    fn insert(&mut self, wait: WaitSetKey, task: TaskKey, sources: Vec<WaitSourceKey>) {
        self.remove(wait, task);
        for source in &sources {
            match *source {
                WaitSourceKey::Wake(wake) => self.wakes.insert(wake, task),
                WaitSourceKey::Completion(completion) => {
                    match self.completions.get_mut(&completion) {
                        Some(waiters) => waiters.insert(task),
                        None => {
                            self.completions.insert(completion, Waiters::One(task));
                        }
                    }
                }
            }
        }
        self.sets.insert(wait, sources);
    }

    fn remove(&mut self, wait: WaitSetKey, task: TaskKey) {
        let Some(sources) = self.sets.remove(&wait) else {
            return;
        };
        for source in sources {
            match source {
                WaitSourceKey::Wake(wake) => self.wakes.remove(wake, task),
                WaitSourceKey::Completion(completion) => {
                    let remove = self
                        .completions
                        .get_mut(&completion)
                        .is_some_and(|waiters| waiters.remove(task));
                    if remove {
                        self.completions.remove(&completion);
                    }
                }
            }
        }
    }

    fn take_wake(&mut self, wake: WakeKey) -> Option<Waiters> {
        self.wakes.take(wake)
    }

    fn take_completion(&mut self, completion: CompletionKey) -> Option<Waiters> {
        self.completions.remove(&completion)
    }

    fn has_completion(&self, completion: CompletionKey) -> bool {
        self.completions.contains_key(&completion)
    }

    fn has_completions(&self) -> bool {
        !self.completions.is_empty()
    }

    fn tasks_with_completions(&self) -> Vec<TaskKey> {
        let mut tasks = Vec::new();
        for waiters in self.completions.values() {
            match waiters {
                Waiters::One(task) => tasks.push(*task),
                Waiters::Many(found) => tasks.extend(found.iter().copied()),
            }
        }
        tasks.sort_unstable();
        tasks.dedup();
        tasks
    }
}

fn wake_task(wake: WakeKey) -> TaskKey {
    match wake {
        WakeKey::Receive(task)
        | WakeKey::Send(task)
        | WakeKey::Done(task)
        | WakeKey::Asked(task)
        | WakeKey::Snapshot(task) => task,
    }
}

impl TaskIndex {
    fn clear(&mut self) {
        self.slots.clear();
    }

    fn get(&self, key: &TaskKey) -> Option<&IndexedState> {
        let task = self.slots.get(key.vm as usize)?.as_ref()?;
        (task.key == *key).then_some(&task.state)
    }

    fn insert(&mut self, key: TaskKey, state: IndexedState) {
        let slot = key.vm as usize;
        if self.slots.len() <= slot {
            self.slots.resize(slot + 1, None);
        }
        self.slots[slot] = Some(IndexedTask { key, state });
    }

    fn remove(&mut self, key: &TaskKey) {
        let Some(slot) = self.slots.get_mut(key.vm as usize) else {
            return;
        };
        if slot.as_ref().is_some_and(|task| task.key == *key) {
            *slot = None;
        }
    }

    fn values(&self) -> impl Iterator<Item = &IndexedState> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref().map(|task| &task.state))
    }

    fn iter(&self) -> impl Iterator<Item = (&TaskKey, &IndexedState)> {
        self.slots
            .iter()
            .filter_map(|slot| slot.as_ref().map(|task| (&task.key, &task.state)))
    }
}

impl Default for Scheduler {
    fn default() -> Scheduler {
        Scheduler::new(SchedulerMode::Deterministic)
    }
}

impl Scheduler {
    pub fn new(mode: SchedulerMode) -> Scheduler {
        Scheduler::new_with_quantum(mode, DEFAULT_QUANTUM)
    }

    /// Create one scheduler with an explicit instruction quantum.
    pub fn new_with_quantum(mode: SchedulerMode, quantum: u32) -> Scheduler {
        Scheduler {
            mode,
            quantum: quantum.max(1),
            include_root: true,
            stats: SchedulerStats::default(),
            stop: None,
            ready: VecDeque::new(),
            ready_ticket: 0,
            tasks: TaskIndex::default(),
            blocked: WakeIndex::default(),
            block_keys: BTreeMap::new(),
            waiting: BTreeMap::new(),
            parked: ParkIndex::default(),
            events: ScheduleEvents::default(),
        }
    }

    pub fn mode(&self) -> SchedulerMode {
        self.mode
    }

    /// The guest instruction count of one scheduler slice.
    pub fn quantum(&self) -> u32 {
        self.quantum
    }

    pub fn stats(&self) -> SchedulerStats {
        self.stats
    }

    /// Why the last run stopped.
    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop
    }

    /// Run one execution to the terminal result of its root VM.
    pub fn run(&mut self, world: &mut World<'_>) -> Outcome {
        self.reset(world, true);
        loop {
            self.consume_events(world);
            self.poll_completions(world);
            if let Some(root) = world.task_key(world.root()) {
                if world.task_status(root) == TaskStatus::Terminal {
                    self.stop.get_or_insert(StopReason::RootTerminal);
                    return world.task_outcome(root);
                }
            }

            if let Some(task) = self.next_ready(world) {
                if task.vm == world.root() {
                    self.stats.root_slices = self.stats.root_slices.saturating_add(1);
                } else {
                    self.stats.proc_slices = self.stats.proc_slices.saturating_add(1);
                }
                self.tasks.insert(task, IndexedState::Running);
                let quantum = world.snapshot_wait_quantum(task, self.quantum);
                let before = world.world_fuel();
                let exit = world.drive_slice(task, quantum);
                let retired = before.saturating_sub(world.world_fuel());
                world.note_scheduler_slice(task, retired, exit.is_some());
                // A terminal parent loses its pass-through before a
                // child from its last slice runs.
                if exit == Some(SliceExit::Terminal) {
                    self.finish_slice(world, task, exit);
                    self.consume_events(world);
                } else {
                    self.consume_events(world);
                    self.finish_slice(world, task, exit);
                }
                continue;
            }

            if !self.waiting.is_empty() || self.parked.has_completions() {
                if let Some(completion) = world.wait_host_completion(|key| {
                    self.waiting.contains_key(&key) || self.parked.has_completion(key)
                }) {
                    self.complete_completion(world, completion);
                } else {
                    self.fail_waiting(world);
                }
                continue;
            }

            if self
                .tasks
                .values()
                .any(|state| matches!(state, IndexedState::Blocked(_) | IndexedState::Parked(_)))
            {
                self.stop = Some(StopReason::Deadlock);
                self.fail_every_block(world);
                continue;
            }

            let root = world
                .task_key(world.root())
                .expect("the execution keeps its root VM");
            world.fail_blocked_task(root, "the scheduler found no runnable task");
            self.refresh(world, root);
            self.stop = Some(StopReason::Deadlock);
        }
    }

    /// Reset policy state at one requested run boundary.
    fn reset(&mut self, world: &mut World<'_>, include_root: bool) {
        self.include_root = include_root;
        self.stats = SchedulerStats::default();
        self.stop = None;
        self.ready.clear();
        self.ready_ticket = 0;
        self.tasks.clear();
        self.blocked.clear();
        self.block_keys.clear();
        self.waiting.clear();
        self.parked.clear();
        self.events.clear();
        for (task, status) in world.scheduler_seeds(include_root) {
            self.index_status(world, task, status);
        }
        self.consume_events(world);
    }

    fn index_status(&mut self, world: &World<'_>, task: TaskKey, status: TaskStatus) {
        match status {
            TaskStatus::Ready => self.enqueue(task),
            TaskStatus::Blocked(wake) => self.block(world, task, wake),
            TaskStatus::Waiting(completion) => self.wait(task, completion),
            TaskStatus::Parked(wait) => self.park(world, task, wait),
            TaskStatus::Terminal | TaskStatus::Dormant => self.drop_task(task),
        }
    }

    fn enqueue(&mut self, task: TaskKey) {
        if matches!(self.tasks.get(&task), Some(IndexedState::Queued(_))) {
            return;
        }
        self.remove_index(task);
        let ticket = self.ready_ticket;
        self.ready_ticket += 1;
        self.tasks.insert(task, IndexedState::Queued(ticket));
        self.ready.push_back((task, ticket));
    }

    /// Index one blocked task under every condition that can wake it.
    ///
    /// The primary condition comes from the machine state. A task that
    /// holds a `drive` call adds one condition for each surface it
    /// drives, so a request from any descendant reaches it.
    fn block(&mut self, world: &World<'_>, task: TaskKey, wake: WakeKey) {
        if self.tasks.get(&task) == Some(&IndexedState::Blocked(wake)) {
            return;
        }
        self.remove_index(task);
        self.tasks.insert(task, IndexedState::Blocked(wake));
        let keys = world.block_wakes(task, wake);
        for key in &keys {
            self.blocked.insert(*key, task);
        }
        if keys.len() > 1 {
            self.block_keys.insert(task, keys);
        }
    }

    fn wait(&mut self, task: TaskKey, completion: CompletionKey) {
        if self.tasks.get(&task) == Some(&IndexedState::Waiting(completion)) {
            return;
        }
        self.remove_index(task);
        self.tasks.insert(task, IndexedState::Waiting(completion));
        self.waiting.insert(completion, task);
    }

    fn park(&mut self, world: &World<'_>, task: TaskKey, wait: WaitSetKey) {
        if self.tasks.get(&task) == Some(&IndexedState::Parked(wait)) {
            return;
        }
        self.remove_index(task);
        let sources = world.wait_sources(wait);
        if sources.is_empty() {
            self.enqueue(task);
            return;
        }
        self.tasks.insert(task, IndexedState::Parked(wait));
        self.parked.insert(wait, task, sources);
    }

    fn remove_index(&mut self, task: TaskKey) {
        match self.tasks.get(&task).copied() {
            Some(IndexedState::Blocked(wake)) => match self.block_keys.remove(&task) {
                Some(keys) => {
                    for key in keys {
                        self.blocked.remove(key, task);
                    }
                }
                None => self.blocked.remove(wake, task),
            },
            Some(IndexedState::Waiting(completion)) => {
                self.waiting.remove(&completion);
            }
            Some(IndexedState::Parked(wait)) => {
                self.parked.remove(wait, task);
            }
            _ => {}
        }
    }

    fn drop_task(&mut self, task: TaskKey) {
        self.remove_index(task);
        self.tasks.remove(&task);
    }

    fn refresh(&mut self, world: &World<'_>, task: TaskKey) {
        self.index_status(world, task, world.task_status(task));
    }

    /// Apply changes produced by the last execution slice.
    fn consume_events(&mut self, world: &mut World<'_>) {
        if !world.swap_schedule_events(&mut self.events) {
            return;
        }
        self.events.prepare_drain();
        while let Some(task) = self.events.pop_removed() {
            self.drop_task(task);
        }
        while let Some(wake) = self.events.pop_wake() {
            self.wake(world, wake);
        }
        while let Some(task) = self.events.pop_ready() {
            if self.include_root || task.vm != world.root() {
                self.refresh(world, task);
            }
        }
    }

    /// Move indexed waiters whose wake condition changed.
    fn wake(&mut self, world: &World<'_>, wake: WakeKey) {
        let Some(waiters) = self.blocked.take(wake) else {
            if let Some(waiters) = self.parked.take_wake(wake) {
                self.wake_parked(world, waiters);
            }
            return;
        };
        waiters.for_each(|task| {
            let Some(IndexedState::Blocked(primary)) = self.tasks.get(&task).copied() else {
                return;
            };
            // The task can hold several conditions. Accept this one
            // when it is the primary condition or one of the extras.
            let holds = primary == wake
                || self
                    .block_keys
                    .get(&task)
                    .is_some_and(|keys| keys.contains(&wake));
            if !holds {
                return;
            }
            // Leave every other condition before the task moves.
            self.remove_index(task);
            if world.task_status(task) == TaskStatus::Ready {
                self.stats.unblocked = self.stats.unblocked.saturating_add(1);
            }
            self.refresh(world, task);
        });
        if let Some(waiters) = self.parked.take_wake(wake) {
            self.wake_parked(world, waiters);
        }
    }

    fn wake_parked(&mut self, world: &World<'_>, waiters: Waiters) {
        waiters.for_each(|task| {
            if !matches!(self.tasks.get(&task), Some(IndexedState::Parked(_))) {
                return;
            }
            if world.task_status(task) == TaskStatus::Ready {
                self.stats.unblocked = self.stats.unblocked.saturating_add(1);
            }
            self.refresh(world, task);
        });
    }

    /// Poll every completion that is ready now.
    fn poll_completions(&mut self, world: &mut World<'_>) {
        if self.waiting.is_empty() && !self.parked.has_completions() {
            return;
        }
        while let Some(completion) = world.poll_host_completion(|key| {
            self.waiting.contains_key(&key) || self.parked.has_completion(key)
        }) {
            self.complete_completion(world, completion);
        }
    }

    fn complete_completion(&mut self, world: &World<'_>, completion: CompletionKey) {
        if let Some(task) = self.waiting.remove(&completion) {
            if self.tasks.get(&task) == Some(&IndexedState::Waiting(completion)) {
                self.stats.unblocked = self.stats.unblocked.saturating_add(1);
                self.refresh(world, task);
            }
        }
        if let Some(waiters) = self.parked.take_completion(completion) {
            self.wake_parked(world, waiters);
        }
    }

    /// Take the next valid FIFO task.
    fn next_ready(&mut self, world: &World<'_>) -> Option<TaskKey> {
        while let Some((task, ticket)) = self.ready.pop_front() {
            if self.tasks.get(&task) != Some(&IndexedState::Queued(ticket)) {
                continue;
            }
            match world.task_status(task) {
                TaskStatus::Ready => return Some(task),
                status => self.index_status(world, task, status),
            }
        }
        None
    }

    /// Install one slice exit after its events enter the queue.
    fn finish_slice(&mut self, world: &mut World<'_>, task: TaskKey, exit: Option<SliceExit>) {
        match exit {
            Some(SliceExit::Yielded) => self.enqueue(task),
            Some(SliceExit::Blocked(wake)) => self.block(world, task, wake),
            Some(SliceExit::Waiting(completion)) => self.wait(task, completion),
            Some(SliceExit::Parked(wait)) => self.park(world, task, wait),
            Some(SliceExit::Terminal) => self.finish_terminal(world, task),
            None => self.refresh(world, task),
        }
    }

    fn finish_terminal(&mut self, world: &mut World<'_>, task: TaskKey) {
        self.drop_task(task);
        world.retire_scheduler_task(task);
        self.wake(world, WakeKey::Send(task));
        self.wake(world, WakeKey::Done(task));
    }

    /// Fault every blocked machine of a deadlocked world.
    ///
    /// Specification 18.6 makes a deadlock a policy decision. The
    /// deterministic scheduler is the supervisor here, and it converts
    /// the condition into one fault per blocked machine.
    fn fail_every_block(&mut self, world: &mut World<'_>) {
        let tasks: Vec<TaskKey> = self
            .tasks
            .iter()
            .filter_map(|(task, state)| {
                matches!(state, IndexedState::Blocked(_) | IndexedState::Parked(_)).then_some(*task)
            })
            .collect();
        for task in tasks {
            world.fail_blocked_task(task, "the scheduler found no runnable task");
            self.refresh(world, task);
        }
    }

    /// Fault tasks when the host completion source fails.
    fn fail_waiting(&mut self, world: &mut World<'_>) {
        let mut tasks: Vec<TaskKey> = self.waiting.values().copied().collect();
        tasks.extend(self.parked.tasks_with_completions());
        tasks.sort_unstable();
        tasks.dedup();
        self.waiting.clear();
        self.parked.clear();
        for task in tasks {
            world.fail_waiting_task(task, "the host returned no pending completion");
            self.refresh(world, task);
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
    let mut scheduler = Scheduler::default();
    scheduler.reset(world, false);
    let mut slices = 0;
    loop {
        scheduler.consume_events(world);
        scheduler.poll_completions(world);
        let Some(task) = scheduler.next_ready(world) else {
            return slices;
        };
        scheduler.tasks.insert(task, IndexedState::Running);
        let exit = world.drive_slice(task, scheduler.quantum);
        if exit == Some(SliceExit::Terminal) {
            scheduler.finish_slice(world, task, exit);
            scheduler.consume_events(world);
        } else {
            scheduler.consume_events(world);
            scheduler.finish_slice(world, task, exit);
        }
        slices += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(vm: u32, generation: u32) -> TaskKey {
        TaskKey { vm, generation }
    }

    #[test]
    fn a_requeue_invalidates_the_old_ready_entry() {
        let task = TaskKey {
            vm: 7,
            generation: 3,
        };
        let mut scheduler = Scheduler::default();
        scheduler.enqueue(task);
        let old = scheduler.ready[0].1;
        scheduler.drop_task(task);
        scheduler.enqueue(task);
        let new = scheduler.ready[1].1;
        assert_ne!(old, new);
        assert_eq!(scheduler.tasks.get(&task), Some(&IndexedState::Queued(new)));
    }

    #[test]
    fn a_wake_keeps_waiters_in_task_order() {
        let wake = WakeKey::Send(key(9, 2));
        let mut index = WakeIndex::default();
        index.insert(wake, key(3, 0));
        index.insert(wake, key(1, 0));
        index.insert(wake, key(2, 0));
        index.insert(wake, key(2, 0));
        let mut found = Vec::new();
        index
            .take(wake)
            .expect("the wake has waiters")
            .for_each(|task| found.push(task));
        assert_eq!(found, vec![key(1, 0), key(2, 0), key(3, 0)]);
    }

    #[test]
    fn wake_generations_share_one_dense_machine_slot() {
        let old = WakeKey::Done(key(9, 2));
        let new = WakeKey::Done(key(9, 3));
        let mut index = WakeIndex::default();
        index.insert(old, key(1, 0));
        index.insert(new, key(2, 0));
        assert!(index.take(old).is_some());
        assert!(index.take(new).is_some());
    }
}
