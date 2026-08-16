//! Canonical operation and group manifest.
//!
//! This crate is the one source for effect groups, exact operations,
//! their signatures, and their stable identities. The checker, the
//! verifier, the VM, and the host all read this table. Nothing else
//! defines an operation.
//!
//! An exact operation has a dense slot: its index in `OPS`. The slot
//! order is part of the manifest identity. `manifest_digest` pins the
//! full manifest: version, groups, names, signatures, and order. A
//! change to any of these changes the digest and therefore the ABI
//! version.

mod fault;
mod sha;

pub use fault::{FaultCode, SnapshotClass, FAULT_CODES};
pub use sha::{sha256, sha256_hex};

/// The manifest ABI version. A signature or membership change must
/// increment this value.
///
/// Version 2 adds the eight proc operations of specification 23.6.
/// Version 3 adds the four snapshot operations of specification 23.5.
/// Version 4 adds the snapshot classification to the operation
/// identity, so a classification-only change moves every dependent
/// digest.
pub const ABI_VERSION: u32 = 4;

/// A dense group slot: the index in `GROUPS`.
pub type GroupSlot = u32;

/// A dense operation slot: the index in `OPS`.
pub type OpSlot = u32;

/// The effect groups, in canonical order. Groups without week-4
/// operations exist for rows and policy targets only.
pub const GROUPS: [&str; 9] = [
    "Io", "Fs", "Clock", "Rand", "Net", "Proc", "Vm", "Compiler", "Reflect",
];

/// One manifest type. The set is closed: it covers every parameter
/// and reply position of the week-4 operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbiType {
    Unit,
    Int,
    Str,
    /// `Result[Option[String], IoError]`, the `Io.ReadLine` reply.
    ResultOptionStrIoError,
    /// `Result[SnapshotImage, SnapshotError]`, the `Vm.SnapshotSelf`
    /// reply. A restored self snapshot is answered through the
    /// ordinary typed call path, so the reply type has a name here.
    ResultSnapshotImageError,
}

impl AbiType {
    /// The canonical text of the type, for identity hashing.
    pub fn text(&self) -> &'static str {
        match self {
            AbiType::Unit => "()",
            AbiType::Int => "Int",
            AbiType::Str => "String",
            AbiType::ResultOptionStrIoError => "Result[Option[String], IoError]",
            AbiType::ResultSnapshotImageError => "Result[SnapshotImage, SnapshotError]",
        }
    }
}

/// The behavior family of one operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// A host operation with a fixed first-order signature. It is
    /// callable through `sys`, first-class as an `Op` value, and a
    /// valid `mock` and `as_call` target.
    Fixed,
    /// A VM control operation. Its signature is generic and the
    /// verifier applies a built-in rule per slot. It is not
    /// first-class and cannot be mocked.
    VmControl,
}

/// One manifest entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpDef {
    /// The group name, for example `Io`.
    pub group: &'static str,
    /// The member name, for example `Print`.
    pub member: &'static str,
    pub kind: OpKind,
    /// Parameter types. Empty for `VmControl` entries: their generic
    /// schemas live in the verifier rules.
    pub params: &'static [AbiType],
    /// The reply type. `Unit` for `VmControl` entries.
    pub reply: AbiType,
    /// The generic schema text of a `VmControl` entry, for identity
    /// hashing. Empty for `Fixed` entries.
    pub schema: &'static str,
    /// The snapshot classification of one pending instance of this
    /// operation (specification 16.4 and 25.5).
    ///
    /// `HostAttachment` means the operation may suspend and hold live
    /// state outside every machine while it waits. `MachineState`
    /// means the operation always completes inside the host call, so
    /// it leaves nothing pending for a snapshot to copy. A host that
    /// suspends a `MachineState` operation breaks the contract, and
    /// the VM rejects that reply.
    pub snapshot: SnapshotClass,
}

impl OpDef {
    /// True when a pending instance of this operation is a live host
    /// attachment.
    pub fn suspends(&self) -> bool {
        matches!(self.snapshot, SnapshotClass::HostAttachment)
    }
}

/// Dense slots for the week-4 operations. The constants match the
/// index in `OPS`.
pub const OP_IO_PRINT: OpSlot = 0;
pub const OP_IO_ERROR: OpSlot = 1;
pub const OP_IO_READ_LINE: OpSlot = 2;
pub const OP_CLOCK_NOW: OpSlot = 3;
pub const OP_CLOCK_MONOTONIC: OpSlot = 4;
pub const OP_CLOCK_SLEEP: OpSlot = 5;
pub const OP_RAND_INT: OpSlot = 6;
pub const OP_VM_NEW: OpSlot = 7;
pub const OP_VM_FROM_OBJECT: OpSlot = 8;
pub const OP_VM_RUN: OpSlot = 9;
pub const OP_VM_STEP: OpSlot = 10;
pub const OP_VM_DRIVE: OpSlot = 11;
pub const OP_VM_ANSWER: OpSlot = 12;
pub const OP_VM_REJECT: OpSlot = 13;
pub const OP_VM_DISPATCH: OpSlot = 14;
pub const OP_VM_TABLE: OpSlot = 15;
pub const OP_PROC_RUN: OpSlot = 16;
pub const OP_PROC_SPAWN: OpSlot = 17;
pub const OP_PROC_SEND: OpSlot = 18;
pub const OP_PROC_CLOSE: OpSlot = 19;
pub const OP_PROC_RECV: OpSlot = 20;
pub const OP_PROC_DONE: OpSlot = 21;
pub const OP_PROC_PAUSE: OpSlot = 22;
pub const OP_PROC_RESUME: OpSlot = 23;
pub const OP_VM_SNAPSHOT_HELD: OpSlot = 24;
pub const OP_VM_SNAPSHOT_SELF: OpSlot = 25;
pub const OP_VM_LOAD_SNAPSHOT: OpSlot = 26;
pub const OP_VM_RESTORE: OpSlot = 27;

/// The exact operations, in canonical slot order.
pub const OPS: [OpDef; 28] = [
    OpDef {
        group: "Io",
        member: "Print",
        kind: OpKind::Fixed,
        params: &[AbiType::Str],
        reply: AbiType::Unit,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Io",
        member: "Error",
        kind: OpKind::Fixed,
        params: &[AbiType::Str],
        reply: AbiType::Unit,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Io",
        member: "ReadLine",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::ResultOptionStrIoError,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Clock",
        member: "Now",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::Int,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Clock",
        member: "Monotonic",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::Int,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Clock",
        member: "Sleep",
        kind: OpKind::Fixed,
        params: &[AbiType::Int],
        reply: AbiType::Unit,
        schema: "",
        snapshot: SnapshotClass::HostAttachment,
    },
    OpDef {
        group: "Rand",
        member: "Int",
        kind: OpKind::Fixed,
        params: &[AbiType::Int, AbiType::Int],
        reply: AbiType::Int,
        schema: "",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "New",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "() -> EmptyVm",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "FromObject",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[A,T,e](EmptyVm, Fn[A,T,e], control A) -> Vm[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Run",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> RunResult[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Step",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> StepEvent[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Drive",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> DriveEvent[T]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Answer",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T,A,R](Vm[T], PendingCall[A,R], R) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Reject",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T], Request, Fault) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Dispatch",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T], Request) -> ()",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Table",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> PolicyTable",
        snapshot: SnapshotClass::MachineState,
    },
    // The proc operations of specification 23.6. Every one of them is
    // machine state: a blocked proc call waits on another machine of
    // the same machine world, never on live state outside it. The
    // scheduler record that carries the block holds proc identifiers
    // and ordinals only, so a snapshot rebuilds it from the machines.
    OpDef {
        group: "Proc",
        member: "Run",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Vm[R], Type[M]) -> Handle[M,R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Spawn",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R,A](Class[Proc[M]], control A) -> Handle[M,R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Send",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Handle[M,R], M) -> SendResult",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Close",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Handle[M,R]) -> SendResult",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Recv",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M](proc self) -> Recv[M]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Done",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Handle[M,R]) -> ProcResult[R]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Pause",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Handle[M,R]) -> Result[Vm[R], ProcError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Proc",
        member: "Resume",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[M,R](Handle[M,R]) -> Result[(), ProcError]",
        snapshot: SnapshotClass::MachineState,
    },
    // The snapshot operations of specification 23.5. A capture, a
    // load, and a restore all run inside the driver loop, so none of
    // them holds live state outside a machine.
    OpDef {
        group: "Vm",
        member: "SnapshotHeld",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> Result[Snapshot[T], SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "SnapshotSelf",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::ResultSnapshotImageError,
        schema: "() -> Result[SnapshotImage, SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "LoadSnapshot",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "(Bytes) -> Result[SnapshotImage, SnapshotError]",
        snapshot: SnapshotClass::MachineState,
    },
    OpDef {
        group: "Vm",
        member: "Restore",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](EmptyVm, Snapshot[T]) -> Result[Vm[T], RestoreError]",
        snapshot: SnapshotClass::MachineState,
    },
];

/// The number of exact operations.
pub const OP_COUNT: u32 = OPS.len() as u32;

/// The number of groups.
pub const GROUP_COUNT: u32 = GROUPS.len() as u32;

/// Return the definition of one operation slot.
pub fn op(slot: OpSlot) -> &'static OpDef {
    &OPS[slot as usize]
}

/// The canonical qualified name of one operation, for example
/// `Io.Print`.
pub fn op_name(slot: OpSlot) -> String {
    let def = op(slot);
    format!("{}.{}", def.group, def.member)
}

/// The group slot of one operation.
pub fn op_group(slot: OpSlot) -> GroupSlot {
    let def = op(slot);
    group_by_name(def.group).expect("every operation names a manifest group")
}

/// Find a group slot by name.
pub fn group_by_name(name: &str) -> Option<GroupSlot> {
    GROUPS.iter().position(|g| *g == name).map(|i| i as u32)
}

/// Find an operation slot by its canonical qualified name.
pub fn op_by_name(name: &str) -> Option<OpSlot> {
    let (group, member) = name.split_once('.')?;
    OPS.iter()
        .position(|def| def.group == group && def.member == member)
        .map(|i| i as u32)
}

/// Find a `Fixed` operation inside a group by its member name. This
/// is the lookup behind `sys.<group>.<Member>`.
pub fn fixed_member(group: &str, member: &str) -> Option<OpSlot> {
    OPS.iter()
        .position(|def| def.group == group && def.member == member && def.kind == OpKind::Fixed)
        .map(|i| i as u32)
}

/// True when a row name is valid: an exact operation name or a group
/// name.
pub fn row_name_valid(name: &str) -> bool {
    if name.contains('.') {
        op_by_name(name).is_some()
    } else {
        group_by_name(name).is_some()
    }
}

/// The stable identity hash of one operation: the domain-separated
/// SHA-256 of the ABI version, the qualified name, the complete
/// signature or schema text, and every other semantic field.
pub fn op_identity(slot: OpSlot) -> [u8; 32] {
    identity_of(&op_name(slot), op(slot))
}

/// The stable identity hash of one operation definition.
///
/// The hash covers every semantic field of the definition. The
/// snapshot classification is one of them: it decides whether a
/// pending instance holds live host state, so it changes snapshot and
/// resource behavior. A classification-only change therefore moves the
/// operation identity, the manifest digest, and the verification hash
/// of every module that names the operation.
///
/// The call takes the definition, so a test can hash a changed
/// definition without a second manifest.
pub fn identity_of(name: &str, def: &OpDef) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-operation-v2\0");
    input.extend_from_slice(&ABI_VERSION.to_le_bytes());
    input.extend_from_slice(name.as_bytes());
    input.push(0);
    match def.kind {
        OpKind::Fixed => {
            input.push(0);
            for p in def.params {
                input.extend_from_slice(p.text().as_bytes());
                input.push(0);
            }
            input.push(0xff);
            input.extend_from_slice(def.reply.text().as_bytes());
        }
        OpKind::VmControl => {
            input.push(1);
            input.extend_from_slice(def.schema.as_bytes());
        }
    }
    input.push(0xfe);
    input.extend_from_slice(def.snapshot.tag().as_bytes());
    sha256(&input)
}

/// The digest of the full manifest: version, groups, and every
/// operation identity in slot order.
pub fn manifest_digest() -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-operations-manifest-v1\0");
    input.extend_from_slice(&ABI_VERSION.to_le_bytes());
    for group in GROUPS {
        input.extend_from_slice(group.as_bytes());
        input.push(0);
    }
    for slot in 0..OP_COUNT {
        input.extend_from_slice(&op_identity(slot));
    }
    sha256(&input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_match_the_constants() {
        assert_eq!(op_by_name("Io.Print"), Some(OP_IO_PRINT));
        assert_eq!(op_by_name("Io.Error"), Some(OP_IO_ERROR));
        assert_eq!(op_by_name("Io.ReadLine"), Some(OP_IO_READ_LINE));
        assert_eq!(op_by_name("Clock.Now"), Some(OP_CLOCK_NOW));
        assert_eq!(op_by_name("Clock.Monotonic"), Some(OP_CLOCK_MONOTONIC));
        assert_eq!(op_by_name("Clock.Sleep"), Some(OP_CLOCK_SLEEP));
        assert_eq!(op_by_name("Rand.Int"), Some(OP_RAND_INT));
        assert_eq!(op_by_name("Vm.New"), Some(OP_VM_NEW));
        assert_eq!(op_by_name("Vm.Table"), Some(OP_VM_TABLE));
        assert_eq!(op_by_name("Proc.Run"), Some(OP_PROC_RUN));
        assert_eq!(op_by_name("Proc.Spawn"), Some(OP_PROC_SPAWN));
        assert_eq!(op_by_name("Proc.Send"), Some(OP_PROC_SEND));
        assert_eq!(op_by_name("Proc.Close"), Some(OP_PROC_CLOSE));
        assert_eq!(op_by_name("Proc.Recv"), Some(OP_PROC_RECV));
        assert_eq!(op_by_name("Proc.Done"), Some(OP_PROC_DONE));
        assert_eq!(op_by_name("Proc.Pause"), Some(OP_PROC_PAUSE));
        assert_eq!(op_by_name("Proc.Resume"), Some(OP_PROC_RESUME));
        assert_eq!(op_by_name("Vm.SnapshotHeld"), Some(OP_VM_SNAPSHOT_HELD));
        assert_eq!(op_by_name("Vm.SnapshotSelf"), Some(OP_VM_SNAPSHOT_SELF));
        assert_eq!(op_by_name("Vm.LoadSnapshot"), Some(OP_VM_LOAD_SNAPSHOT));
        assert_eq!(op_by_name("Vm.Restore"), Some(OP_VM_RESTORE));
        assert_eq!(op_by_name("Fs.Open"), None);
    }

    #[test]
    fn names_round_trip() {
        for slot in 0..OP_COUNT {
            assert_eq!(op_by_name(&op_name(slot)), Some(slot));
        }
    }

    #[test]
    fn groups_resolve() {
        assert_eq!(group_by_name("Io"), Some(0));
        assert_eq!(group_by_name("Vm"), Some(6));
        assert_eq!(group_by_name("Nope"), None);
        assert_eq!(op_group(OP_CLOCK_NOW), group_by_name("Clock").unwrap());
    }

    #[test]
    fn row_names_validate() {
        assert!(row_name_valid("Io"));
        assert!(row_name_valid("Io.Print"));
        assert!(row_name_valid("Fs"));
        assert!(row_name_valid("Vm.Run"));
        assert!(!row_name_valid("Io.Prin"));
        assert!(!row_name_valid("Web"));
        assert!(!row_name_valid("Web.Get"));
    }

    #[test]
    fn identities_are_distinct_and_stable_within_a_run() {
        let mut seen = Vec::new();
        for slot in 0..OP_COUNT {
            let id = op_identity(slot);
            assert!(!seen.contains(&id), "duplicate identity for slot {slot}");
            seen.push(id);
            assert_eq!(op_identity(slot), id);
        }
        assert_eq!(manifest_digest(), manifest_digest());
    }

    /// Every operation declares one snapshot classification, and the
    /// suspending set is exactly the four operations that reach live
    /// host state. A new operation must join this list on purpose.
    #[test]
    fn every_operation_declares_one_snapshot_class() {
        let suspending: Vec<String> = (0..OP_COUNT)
            .filter(|slot| op(*slot).suspends())
            .map(op_name)
            .collect();
        assert_eq!(
            suspending,
            vec!["Io.Print", "Io.Error", "Io.ReadLine", "Clock.Sleep"]
        );
        // A VM control operation runs inside the driver loop, so it
        // never holds a host attachment.
        for slot in 0..OP_COUNT {
            if op(slot).kind == OpKind::VmControl {
                assert_eq!(op(slot).snapshot, SnapshotClass::MachineState);
            }
        }
    }

    #[test]
    fn fixed_member_excludes_vm_control() {
        assert_eq!(fixed_member("Io", "Print"), Some(OP_IO_PRINT));
        assert_eq!(fixed_member("Vm", "Run"), None);
        assert_eq!(fixed_member("Vm", "New"), None);
    }
}
