//! The host-side resource registry of one machine.
//!
//! The registry lives outside the guest heap. It records every live
//! host attachment a machine holds: the resource kind, the owning
//! machine, the scope identity, the pending operation ordinal, and the
//! cleanup state (specification 25.5).
//!
//! Week 7 registers one kind: a pending suspending operation. A guest
//! value never names a record, so no guest code can forge one, and a
//! record can outlive the guest wrapper that started it.
//!
//! Snapshot preflight reads this registry beside the guest graph. A
//! live host attachment blocks a snapshot.

use crate::machine::VmId;
use crate::FaultCode;
use lm_abi::SnapshotClass;

/// The kind of one registered resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// One suspending host operation that has not completed. The
    /// scope identity is the host completion token.
    PendingOperation,
}

impl ResourceKind {
    /// The snapshot classification of this kind.
    pub fn snapshot(&self) -> SnapshotClass {
        match self {
            // A live completion callback has no bytes to copy.
            ResourceKind::PendingOperation => SnapshotClass::HostAttachment,
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
}

impl ResourceRegistry {
    pub fn new(cap: u32) -> ResourceRegistry {
        ResourceRegistry {
            records: Vec::new(),
            cap,
            closed: 0,
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
        if let Some(slot) = self
            .records
            .iter_mut()
            .find(|r| r.state == ResourceState::Closed)
        {
            *slot = record;
            return Ok(());
        }
        if self.records.len() as u32 >= self.cap {
            return Err(FaultCode::HostFault);
        }
        self.records.push(record);
        Ok(())
    }

    /// Close the live record with this scope identity. Return true
    /// when a record closed.
    pub fn close(&mut self, scope: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live && record.scope == scope {
                record.state = ResourceState::Closed;
                self.closed += 1;
                return true;
            }
        }
        false
    }

    /// Close the live record of one pending request ordinal. Return
    /// true when a record closed.
    pub fn close_by_ordinal(&mut self, ordinal: u64) -> bool {
        for record in &mut self.records {
            if record.state == ResourceState::Live && record.ordinal == ordinal {
                record.state = ResourceState::Closed;
                self.closed += 1;
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
                self.closed += 1;
                count += 1;
            }
        }
        count
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
}
