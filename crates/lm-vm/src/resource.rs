//! The host-side resource registry of one machine.
//!
//! The registry lives outside the guest heap. It records every live
//! host attachment a machine holds: the resource kind, the owning
//! machine, the scope identity, the pending operation ordinal, and the
//! cleanup state (specification 25.5).
//!
//! The registry tracks pending operations and open external resources.
//! Guest code cannot forge a record.
//!
//! Snapshot preflight reads this registry beside the guest graph. A
//! live host attachment blocks a snapshot.

use crate::machine::VmId;
use crate::FaultCode;
use lm_abi::SnapshotClass;
use std::cell::Cell;
use std::rc::Rc;

/// One live-resource ledger for a root VM and its spawned procs.
#[derive(Debug, Clone)]
pub(crate) struct ResourceBudget {
    used: Rc<Cell<usize>>,
    limit: usize,
}

impl ResourceBudget {
    pub(crate) fn new(limit: usize) -> ResourceBudget {
        ResourceBudget {
            used: Rc::new(Cell::new(0)),
            limit,
        }
    }

    fn take(&self) -> bool {
        let used = self.used.get();
        let Some(next) = used.checked_add(1) else {
            return false;
        };
        if next > self.limit {
            return false;
        }
        self.used.set(next);
        true
    }

    fn has_room(&self) -> bool {
        self.used.get() < self.limit
    }

    fn give(&self, count: usize) {
        debug_assert!(self.used.get() >= count);
        self.used.set(self.used.get().saturating_sub(count));
    }

    pub(crate) fn used(&self) -> usize {
        self.used.get()
    }
}

/// The kind of one registered resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// One suspending host operation that has not completed. The
    /// scope identity is the host completion token.
    PendingOperation,
    /// One open file resource.
    File,
    /// One open TCP stream.
    TcpStream,
    /// One open TCP listener.
    TcpListener,
    /// One open TLS stream.
    TlsStream,
    /// One active raw terminal mode.
    RawMode,
    /// One open process signal stream.
    SignalStream,
    /// One open pipe read end.
    PipeReader,
    /// One open pipe write end.
    PipeWriter,
    /// One live operating-system child handle.
    Child,
    /// One opaque extension resource, by stable kind identity.
    Extension([u8; 32]),
}

impl ResourceKind {
    /// The snapshot classification of this kind.
    pub fn snapshot(&self) -> SnapshotClass {
        match self {
            // A live completion callback has no bytes to copy.
            ResourceKind::PendingOperation => SnapshotClass::HostAttachment,
            ResourceKind::File => SnapshotClass::HostAttachment,
            ResourceKind::TcpStream
            | ResourceKind::TcpListener
            | ResourceKind::TlsStream
            | ResourceKind::RawMode
            | ResourceKind::SignalStream
            | ResourceKind::PipeReader
            | ResourceKind::PipeWriter
            | ResourceKind::Child => SnapshotClass::HostAttachment,
            ResourceKind::Extension(_) => SnapshotClass::HostAttachment,
        }
    }
}

/// The cleanup state of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceState {
    /// The resource is live and holds external state.
    Live,
    /// Cleanup closed the resource. The slot is free for reuse.
    Closed,
}

/// One registry record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceRecord {
    pub kind: ResourceKind,
    /// The machine that owns the resource.
    pub owner: VmId,
    /// The host scope identity. A pending operation stores its
    /// completion token here.
    pub scope: u64,
    /// The request ordinal of the pending operation that holds it.
    pub ordinal: u64,
    /// The operation slot, for the manifest classification.
    pub op: u32,
    pub state: ResourceState,
}

/// The per-machine registry.
///
/// The record vector never grows past the configured cap: a closed
/// slot is reused before a new slot is added.
#[derive(Debug)]
pub struct ResourceRegistry {
    records: Vec<ResourceRecord>,
    cap: u32,
    /// The number of records cleanup closed, for the tests.
    closed: u64,
    /// The aggregate ledger of the owning world.
    budget: Option<ResourceBudget>,
}

impl ResourceRegistry {
    pub fn new(cap: u32) -> ResourceRegistry {
        ResourceRegistry::with_optional_budget(cap, None)
    }

    /// Create a registry that charges one proc-tree ledger.
    pub(crate) fn with_budget(cap: u32, budget: ResourceBudget) -> ResourceRegistry {
        ResourceRegistry::with_optional_budget(cap, Some(budget))
    }

    fn with_optional_budget(cap: u32, budget: Option<ResourceBudget>) -> ResourceRegistry {
        ResourceRegistry {
            records: Vec::new(),
            cap,
            closed: 0,
            budget,
        }
    }

    /// Register one live resource.
    ///
    /// The call fails when the machine already holds its cap of live
    /// resources. The host, not the guest heap, owns the limit, so the
    /// failure is a host fault.
    pub fn register(
        &mut self,
        kind: ResourceKind,
        owner: VmId,
        scope: u64,
        ordinal: u64,
        op: u32,
    ) -> Result<(), FaultCode> {
        let record = ResourceRecord {
            kind,
            owner,
            scope,
            ordinal,
            op,
            state: ResourceState::Live,
        };
        let reuse = self
            .records
            .iter()
            .position(|record| record.state == ResourceState::Closed);
        if reuse.is_none() && self.records.len() as u32 >= self.cap {
            return Err(FaultCode::HostFault);
        }
        if self.budget.as_ref().is_some_and(|budget| !budget.take()) {
            return Err(FaultCode::HostFault);
        }
        match reuse {
            Some(slot) => self.records[slot] = record,
            None => self.records.push(record),
        }
        Ok(())
    }

    /// Reserve registry storage before host work can start.
    pub fn prepare_register(&mut self) -> Result<(), FaultCode> {
        let reuses_closed = self
            .records
            .iter()
            .any(|record| record.state == ResourceState::Closed);
        if !reuses_closed && self.records.len() as u32 >= self.cap {
            return Err(FaultCode::HostFault);
        }
        if self
            .budget
            .as_ref()
            .is_some_and(|budget| !budget.has_room())
        {
            return Err(FaultCode::HostFault);
        }
        if !reuses_closed {
            self.records
                .try_reserve(1)
                .map_err(|_| FaultCode::HostFault)?;
        }
        Ok(())
    }

    /// Close the live record with this scope identity. Return true
    /// when a record closed.
    pub fn close(&mut self, scope: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live && record.scope == scope {
                record.state = ResourceState::Closed;
                self.closed = self.closed.saturating_add(1);
                if let Some(budget) = &self.budget {
                    budget.give(1);
                }
                return true;
            }
        }
        false
    }

    /// Close one live record with this kind and scope.
    pub fn close_kind(&mut self, kind: ResourceKind, scope: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live && record.kind == kind && record.scope == scope {
                record.state = ResourceState::Closed;
                self.closed = self.closed.saturating_add(1);
                if let Some(budget) = &self.budget {
                    budget.give(1);
                }
                return true;
            }
        }
        false
    }

    /// Close the live record of one pending request ordinal. Return
    /// true when a record closed.
    pub fn close_by_ordinal(&mut self, ordinal: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live
                && record.kind == ResourceKind::PendingOperation
                && record.ordinal == ordinal
            {
                record.state = ResourceState::Closed;
                self.closed = self.closed.saturating_add(1);
                if let Some(budget) = &self.budget {
                    budget.give(1);
                }
                return true;
            }
        }
        false
    }

    /// Set the host scope of one prepared operation.
    pub fn set_pending_scope(&mut self, ordinal: u64, scope: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live
                && record.kind == ResourceKind::PendingOperation
                && record.ordinal == ordinal
            {
                record.scope = scope;
                return true;
            }
        }
        false
    }

    /// Close every live record. Machine termination calls this. It
    /// invokes no guest callback, and it never replaces an existing
    /// machine fault.
    pub fn close_all(&mut self) -> usize {
        let mut count = 0;
        for record in &mut self.records {
            if record.state == ResourceState::Live {
                record.state = ResourceState::Closed;
                self.closed = self.closed.saturating_add(1);
                count += 1;
            }
        }
        if let Some(budget) = &self.budget {
            budget.give(count);
        }
        count
    }

    /// Release storage after all live resources close.
    pub(crate) fn compact_closed(&mut self) {
        debug_assert_eq!(self.live_count(), 0);
        self.records = Vec::new();
    }

    /// The live record of one pending request ordinal.
    pub fn pending(&self, ordinal: u64) -> Option<&ResourceRecord> {
        self.records
            .iter()
            .find(|r| r.state == ResourceState::Live && r.ordinal == ordinal)
    }

    /// Every live record.
    pub fn live(&self) -> impl Iterator<Item = &ResourceRecord> {
        self.records
            .iter()
            .filter(|r| r.state == ResourceState::Live)
    }

    /// Return every host completion token retained by this registry.
    pub fn pending_scopes(&self) -> impl Iterator<Item = u64> + '_ {
        self.records
            .iter()
            .filter(|record| record.kind == ResourceKind::PendingOperation)
            .map(|record| record.scope)
    }

    /// The number of live records.
    pub fn live_count(&self) -> usize {
        self.live().count()
    }

    /// The number of records cleanup closed.
    pub fn closed_count(&self) -> u64 {
        self.closed
    }

    /// The first live host attachment, if the machine holds one. A
    /// snapshot preflight rejects the machine when it does.
    pub fn live_attachment(&self) -> Option<&ResourceRecord> {
        self.live()
            .find(|r| r.kind.snapshot() == SnapshotClass::HostAttachment)
    }
}

impl Drop for ResourceRegistry {
    fn drop(&mut self) {
        let live = self.live_count();
        if let Some(budget) = &self.budget {
            budget.give(live);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ResourceRegistry {
        ResourceRegistry::new(2)
    }

    #[test]
    fn a_pending_operation_registers_and_closes() {
        let mut reg = registry();
        reg.register(ResourceKind::PendingOperation, 1, 77, 5, 3)
            .expect("the first record fits");
        assert_eq!(reg.live_count(), 1);
        assert_eq!(reg.pending(5).expect("the record exists").scope, 77);
        assert!(reg.live_attachment().is_some());
        assert!(reg.close(77));
        assert_eq!(reg.live_count(), 0);
        assert_eq!(reg.closed_count(), 1);
        assert!(reg.live_attachment().is_none());
        // A second close of the same scope finds nothing.
        assert!(!reg.close(77));
    }

    #[test]
    fn a_closed_slot_is_reused_before_the_cap_applies() {
        let mut reg = registry();
        for scope in 0..2 {
            reg.register(ResourceKind::PendingOperation, 1, scope, scope, 3)
                .expect("both records fit");
        }
        assert_eq!(
            reg.register(ResourceKind::PendingOperation, 1, 9, 9, 3),
            Err(FaultCode::HostFault)
        );
        assert!(reg.close(0));
        reg.register(ResourceKind::PendingOperation, 1, 9, 9, 3)
            .expect("the closed slot is reused");
        assert_eq!(reg.live_count(), 2);
    }

    #[test]
    fn termination_closes_every_live_record() {
        let mut reg = registry();
        reg.register(ResourceKind::PendingOperation, 1, 1, 1, 3)
            .expect("the record fits");
        reg.register(ResourceKind::PendingOperation, 1, 2, 2, 3)
            .expect("the record fits");
        assert_eq!(reg.close_all(), 2);
        assert_eq!(reg.live_count(), 0);
        // A second cleanup closes nothing more.
        assert_eq!(reg.close_all(), 0);
    }

    #[test]
    fn two_registries_charge_one_shared_budget() {
        let budget = ResourceBudget::new(1);
        let mut first = ResourceRegistry::with_budget(2, budget.clone());
        let mut second = ResourceRegistry::with_budget(2, budget.clone());
        first.prepare_register().expect("the first slot fits");
        first
            .register(ResourceKind::PendingOperation, 0, 1, 1, 0)
            .expect("the first record registers");
        assert_eq!(budget.used(), 1);
        assert_eq!(second.prepare_register(), Err(FaultCode::HostFault));
        assert!(first.close(1));
        second
            .prepare_register()
            .expect("closing the first record releases the ledger");
    }
}
