//! The stable machine-fault codes.
//!
//! A fault code is manifest content: the specification fixes the name
//! and the meaning of each code, and every layer of the runtime reads
//! the same set. The codes live beside the operation manifest so the
//! heap, the graph engine, and the VM can all name them without a
//! dependency on the VM.
//!
//! The code set is not part of `manifest_digest` today. The digest
//! pins the operation ABI, and an artifact carries no fault table, so
//! adding a code cannot invalidate an artifact.

use std::fmt;

/// A stable machine-fault code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultCode {
    IntegerOverflow,
    DivideByZero,
    OutOfFuel,
    StackLimit,
    HeapLimit,
    FrozenWrite,
    IndexOutOfBounds,
    MissingKey,
    BadCast,
    PolicyDenied,
    InvalidVmState,
    InvalidRequestToken,
    UnsendableValue,
    /// A graph mode reached its object, edge, byte, or work limit.
    BoundaryLimit,
    /// A codec or descriptor rule failed. A live host attachment or a
    /// nondigestible native value raises it.
    BoundaryViolation,
    HostFault,
    /// An operation needed a live proc. The reference names a dead or
    /// replaced machine (specification 12.3).
    DeadProc,
    /// Implementation subcode: a field was read before its first
    /// assignment. Checked source programs cannot reach this fault.
    UninitializedField,
    /// Implementation subcode: the runtime backstop behind a proven
    /// exhaustive `case` executed. Checked source programs cannot
    /// reach this fault.
    UnreachableCode,
}

/// Every stable fault code, in declaration order.
///
/// A snapshot writes a fault by its stable name (specification 12.3),
/// so a loader must map a name back to a code. This table is the one
/// place that lists the codes, so a new code joins both directions at
/// once.
pub const FAULT_CODES: [FaultCode; 19] = [
    FaultCode::IntegerOverflow,
    FaultCode::DivideByZero,
    FaultCode::OutOfFuel,
    FaultCode::StackLimit,
    FaultCode::HeapLimit,
    FaultCode::FrozenWrite,
    FaultCode::IndexOutOfBounds,
    FaultCode::MissingKey,
    FaultCode::BadCast,
    FaultCode::PolicyDenied,
    FaultCode::InvalidVmState,
    FaultCode::InvalidRequestToken,
    FaultCode::UnsendableValue,
    FaultCode::BoundaryLimit,
    FaultCode::BoundaryViolation,
    FaultCode::HostFault,
    FaultCode::DeadProc,
    FaultCode::UninitializedField,
    FaultCode::UnreachableCode,
];

impl FaultCode {
    /// The code with this stable name, or `None`.
    pub fn from_name(name: &str) -> Option<FaultCode> {
        FAULT_CODES
            .iter()
            .copied()
            .find(|code| code.to_string() == name)
    }
}

impl fmt::Display for FaultCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            FaultCode::IntegerOverflow => "IntegerOverflow",
            FaultCode::DivideByZero => "DivideByZero",
            FaultCode::OutOfFuel => "OutOfFuel",
            FaultCode::StackLimit => "StackLimit",
            FaultCode::HeapLimit => "HeapLimit",
            FaultCode::FrozenWrite => "FrozenWrite",
            FaultCode::IndexOutOfBounds => "IndexOutOfBounds",
            FaultCode::MissingKey => "MissingKey",
            FaultCode::BadCast => "BadCast",
            FaultCode::PolicyDenied => "PolicyDenied",
            FaultCode::InvalidVmState => "InvalidVmState",
            FaultCode::InvalidRequestToken => "InvalidRequestToken",
            FaultCode::UnsendableValue => "UnsendableValue",
            FaultCode::BoundaryLimit => "BoundaryLimit",
            FaultCode::BoundaryViolation => "BoundaryViolation",
            FaultCode::HostFault => "HostFault",
            FaultCode::DeadProc => "DeadProc",
            FaultCode::UninitializedField => "UninitializedField",
            FaultCode::UnreachableCode => "UnreachableCode",
        };
        f.write_str(name)
    }
}

/// The snapshot classification of one native value or one suspending
/// operation (specification 16.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotClass {
    /// The codec can copy the bytes of this value into a snapshot.
    MachineState,
    /// The value names live state outside every machine. It has no
    /// bytes to copy, and it blocks snapshot creation.
    HostAttachment,
}

impl fmt::Display for SnapshotClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SnapshotClass::MachineState => "machine-state",
            SnapshotClass::HostAttachment => "host-attachment",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_print_their_names() {
        assert_eq!(FaultCode::BoundaryLimit.to_string(), "BoundaryLimit");
        assert_eq!(
            FaultCode::BoundaryViolation.to_string(),
            "BoundaryViolation"
        );
    }

    /// Every code round trips through its stable name, and the table
    /// lists each code once. A snapshot writes the name, so a missing
    /// entry would make a stored fault unreadable.
    #[test]
    fn every_code_round_trips_through_its_name() {
        let mut seen: Vec<String> = Vec::new();
        for code in FAULT_CODES {
            let name = code.to_string();
            assert!(!seen.contains(&name), "duplicate code name {name}");
            assert_eq!(FaultCode::from_name(&name), Some(code));
            seen.push(name);
        }
        assert_eq!(FaultCode::from_name("NoSuchCode"), None);
    }

    #[test]
    fn classifications_print_their_names() {
        assert_eq!(SnapshotClass::MachineState.to_string(), "machine-state");
        assert_eq!(SnapshotClass::HostAttachment.to_string(), "host-attachment");
    }
}
