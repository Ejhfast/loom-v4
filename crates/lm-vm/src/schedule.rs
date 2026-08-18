//! Plain scheduler records.
//!
//! These records contain identifiers and counters only. They contain
//! no guest reference. A later worker or process boundary can move
//! them without moving a guest heap.

use crate::machine::VmId;
use crate::FaultCode;

/// One stable task identity inside an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskKey {
    pub vm: VmId,
    pub generation: u32,
}

/// One condition that can wake a blocked task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WakeKey {
    /// A message arrived, or the mailbox closed.
    Receive(TaskKey),
    /// Mailbox capacity became available, or the target stopped.
    Send(TaskKey),
    /// The target reached a terminal state.
    Done(TaskKey),
    /// A machine of the target's world surfaced a request to the
    /// driver of the target. The target is the driven surface.
    Asked(TaskKey),
    /// The target world retired work during a snapshot wait.
    Snapshot(TaskKey),
}

/// One pending host completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CompletionKey {
    /// The machine that started the host operation.
    pub machine: TaskKey,
    /// The pending operation ordinal.
    pub ordinal: u64,
}

/// One active typed wait set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WaitSetKey {
    /// The machine that owns the wait token.
    pub owner: TaskKey,
    /// The root token of the prepared wait tree.
    pub token: u64,
}

/// One scheduler source in a typed wait set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WaitSourceKey {
    Wake(WakeKey),
    Completion(CompletionKey),
}

/// Why one bounded execution slice stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceExit {
    /// The task used its quantum and can run again.
    Yielded,
    /// The task waits for another proc state change.
    Blocked(WakeKey),
    /// The task waits for one host completion.
    Waiting(CompletionKey),
    /// The task waits for one source in a typed wait set.
    Parked(WaitSetKey),
    /// The task reached a stored result or fault.
    Terminal,
}

/// The scheduler view of one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Blocked(WakeKey),
    Waiting(CompletionKey),
    Parked(WaitSetKey),
    Terminal,
    /// Ownership, a gate, a barrier, or a pause stops scheduling.
    Dormant,
}

/// Batched changes produced during execution.
#[derive(Debug, Default)]
pub struct ScheduleEvents {
    ready: Vec<TaskKey>,
    removed: Vec<TaskKey>,
    wakes: Vec<WakeKey>,
    ready_marks: Vec<u64>,
    removed_marks: Vec<u64>,
    wake_marks: Vec<[u64; 5]>,
}

impl ScheduleEvents {
    /// True when this batch holds no event.
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.removed.is_empty() && self.wakes.is_empty()
    }

    /// Add one ready event unless this batch already holds it.
    pub fn push_ready(&mut self, key: TaskKey) {
        push_task(&mut self.ready, &mut self.ready_marks, key);
    }

    /// Add one removal event unless this batch already holds it.
    pub fn push_removed(&mut self, key: TaskKey) {
        push_task(&mut self.removed, &mut self.removed_marks, key);
    }

    /// Add one wake event unless this batch already holds it.
    pub fn push_wake(&mut self, wake: WakeKey) {
        let (key, lane) = wake_parts(wake);
        let slot = key.vm as usize;
        if self.wake_marks.len() <= slot {
            self.wake_marks.resize(slot + 1, [0; 5]);
        }
        let mark = u64::from(key.generation) + 1;
        if self.wake_marks[slot][lane] == mark {
            return;
        }
        self.wake_marks[slot][lane] = mark;
        self.wakes.push(wake);
    }

    /// Sort this batch for stable removal from each vector end.
    pub fn prepare_drain(&mut self) {
        self.release_marks();
        order_for_pop(&mut self.removed);
        order_for_pop(&mut self.wakes);
        order_for_pop(&mut self.ready);
    }

    pub fn pop_ready(&mut self) -> Option<TaskKey> {
        self.ready.pop()
    }

    pub fn pop_removed(&mut self) -> Option<TaskKey> {
        self.removed.pop()
    }

    pub fn pop_wake(&mut self) -> Option<WakeKey> {
        self.wakes.pop()
    }

    /// Remove all events and retain their storage.
    pub fn clear(&mut self) {
        self.release_marks();
        self.ready.clear();
        self.removed.clear();
        self.wakes.clear();
    }

    fn release_marks(&mut self) {
        for key in self.ready.iter().copied() {
            clear_task_mark(&mut self.ready_marks, key);
        }
        for key in self.removed.iter().copied() {
            clear_task_mark(&mut self.removed_marks, key);
        }
        for wake in self.wakes.iter().copied() {
            let (key, lane) = wake_parts(wake);
            if let Some(marks) = self.wake_marks.get_mut(key.vm as usize) {
                if marks[lane] == u64::from(key.generation) + 1 {
                    marks[lane] = 0;
                }
            }
        }
    }
}

fn push_task(values: &mut Vec<TaskKey>, marks: &mut Vec<u64>, key: TaskKey) {
    let slot = key.vm as usize;
    if marks.len() <= slot {
        marks.resize(slot + 1, 0);
    }
    let mark = u64::from(key.generation) + 1;
    if marks[slot] == mark {
        return;
    }
    marks[slot] = mark;
    values.push(key);
}

fn clear_task_mark(marks: &mut [u64], key: TaskKey) {
    let mark = &mut marks[key.vm as usize];
    if *mark == u64::from(key.generation) + 1 {
        *mark = 0;
    }
}

fn wake_parts(wake: WakeKey) -> (TaskKey, usize) {
    match wake {
        WakeKey::Receive(key) => (key, 0),
        WakeKey::Send(key) => (key, 1),
        WakeKey::Done(key) => (key, 2),
        WakeKey::Asked(key) => (key, 3),
        WakeKey::Snapshot(key) => (key, 4),
    }
}

fn order_for_pop<T: Ord>(values: &mut Vec<T>) {
    values.sort_unstable_by(|left, right| right.cmp(left));
    values.dedup();
}

/// The active scheduler-owned proc keys.
///
/// The dense position table gives constant-time removal. The entry
/// vector excludes terminal and holder-owned machine records.
#[derive(Debug)]
pub(crate) struct ActiveProcs {
    entries: Vec<TaskKey>,
    positions: Vec<Option<usize>>,
}

impl ActiveProcs {
    pub(crate) fn new(machine_slots: usize) -> ActiveProcs {
        ActiveProcs {
            entries: Vec::new(),
            positions: vec![None; machine_slots],
        }
    }

    /// Reserve one future insertion without changing active entries.
    pub(crate) fn prepare(&mut self, vm: VmId) -> Result<(), FaultCode> {
        self.prepare_batch(vm as usize + 1, 1)
    }

    /// Reserve storage for a batch commit.
    pub(crate) fn prepare_batch(
        &mut self,
        machine_slots: usize,
        added: usize,
    ) -> Result<(), FaultCode> {
        if self.positions.len() < machine_slots {
            self.positions
                .try_reserve_exact(machine_slots - self.positions.len())
                .map_err(|_| FaultCode::HostFault)?;
            self.positions.resize(machine_slots, None);
        }
        self.entries
            .try_reserve(added)
            .map_err(|_| FaultCode::HostFault)
    }

    /// Insert one key after `prepare` succeeds.
    pub(crate) fn insert_prepared(&mut self, key: TaskKey) {
        let slot = key.vm as usize;
        debug_assert!(slot < self.positions.len());
        match self.positions[slot] {
            Some(at) => self.entries[at] = key,
            None => {
                self.positions[slot] = Some(self.entries.len());
                self.entries.push(key);
            }
        }
    }

    pub(crate) fn remove(&mut self, key: TaskKey) -> bool {
        let Some(position) = self.positions.get_mut(key.vm as usize) else {
            return false;
        };
        let Some(at) = *position else {
            return false;
        };
        if self.entries[at] != key {
            return false;
        }
        *position = None;
        self.entries.swap_remove(at);
        if let Some(moved) = self.entries.get(at) {
            self.positions[moved.vm as usize] = Some(at);
        }
        true
    }

    pub(crate) fn contains(&self, key: TaskKey) -> bool {
        self.positions
            .get(key.vm as usize)
            .and_then(|position| *position)
            .is_some_and(|at| self.entries[at] == key)
    }

    pub(crate) fn entries(&self) -> &[TaskKey] {
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(vm: VmId, generation: u32) -> TaskKey {
        TaskKey { vm, generation }
    }

    #[test]
    fn active_removal_repairs_the_moved_position() {
        let mut active = ActiveProcs::new(1);
        active.prepare_batch(4, 3).expect("the index reserves");
        active.insert_prepared(key(1, 0));
        active.insert_prepared(key(2, 0));
        active.insert_prepared(key(3, 0));
        assert!(active.remove(key(2, 0)));
        assert!(active.contains(key(1, 0)));
        assert!(active.contains(key(3, 0)));
        assert!(!active.contains(key(2, 0)));
    }

    #[test]
    fn a_new_generation_replaces_the_old_key() {
        let mut active = ActiveProcs::new(1);
        active.prepare(1).expect("the index reserves");
        active.insert_prepared(key(1, 0));
        active.insert_prepared(key(1, 1));
        assert!(!active.contains(key(1, 0)));
        assert!(active.contains(key(1, 1)));
        assert_eq!(active.entries(), &[key(1, 1)]);
    }

    #[test]
    fn one_event_batch_coalesces_equal_keys() {
        let mut events = ScheduleEvents::default();
        let task = key(3, 7);
        events.push_ready(task);
        events.push_ready(task);
        events.push_wake(WakeKey::Receive(task));
        events.push_wake(WakeKey::Receive(task));
        events.prepare_drain();
        assert_eq!(events.pop_ready(), Some(task));
        assert_eq!(events.pop_ready(), None);
        assert_eq!(events.pop_wake(), Some(WakeKey::Receive(task)));
        assert_eq!(events.pop_wake(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn a_drained_event_can_enter_the_next_batch() {
        let mut events = ScheduleEvents::default();
        let task = key(3, 7);
        events.push_removed(task);
        events.prepare_drain();
        assert_eq!(events.pop_removed(), Some(task));
        events.push_removed(task);
        events.prepare_drain();
        assert_eq!(events.pop_removed(), Some(task));
    }
}
