//! Plain scheduler records.
//!
//! These records contain identifiers and counters only. They contain
//! no guest reference. A later worker or process boundary can move
//! them without moving a guest heap.

use crate::machine::VmId;
use crate::FaultCode;
use std::collections::BTreeSet;

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
}

/// One pending host completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CompletionKey {
    /// The machine that started the host operation.
    pub machine: TaskKey,
    /// The pending operation ordinal.
    pub ordinal: u64,
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
    /// The task reached a stored result or fault.
    Terminal,
}

/// The scheduler view of one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Ready,
    Blocked(WakeKey),
    Waiting(CompletionKey),
    Terminal,
    /// Ownership, a gate, a barrier, or a pause stops scheduling.
    Dormant,
}

/// Coalesced changes produced during execution.
#[derive(Debug, Default)]
pub struct ScheduleEvents {
    pub ready: BTreeSet<TaskKey>,
    pub removed: BTreeSet<TaskKey>,
    pub wakes: BTreeSet<WakeKey>,
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
}
