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

mod sha;

pub use sha::{sha256, sha256_hex};

/// The manifest ABI version. A signature or membership change must
/// increment this value.
pub const ABI_VERSION: u32 = 1;

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
}

impl AbiType {
    /// The canonical text of the type, for identity hashing.
    pub fn text(&self) -> &'static str {
        match self {
            AbiType::Unit => "()",
            AbiType::Int => "Int",
            AbiType::Str => "String",
            AbiType::ResultOptionStrIoError => "Result[Option[String], IoError]",
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

/// The exact operations, in canonical slot order.
pub const OPS: [OpDef; 16] = [
    OpDef {
        group: "Io",
        member: "Print",
        kind: OpKind::Fixed,
        params: &[AbiType::Str],
        reply: AbiType::Unit,
        schema: "",
    },
    OpDef {
        group: "Io",
        member: "Error",
        kind: OpKind::Fixed,
        params: &[AbiType::Str],
        reply: AbiType::Unit,
        schema: "",
    },
    OpDef {
        group: "Io",
        member: "ReadLine",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::ResultOptionStrIoError,
        schema: "",
    },
    OpDef {
        group: "Clock",
        member: "Now",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::Int,
        schema: "",
    },
    OpDef {
        group: "Clock",
        member: "Monotonic",
        kind: OpKind::Fixed,
        params: &[],
        reply: AbiType::Int,
        schema: "",
    },
    OpDef {
        group: "Clock",
        member: "Sleep",
        kind: OpKind::Fixed,
        params: &[AbiType::Int],
        reply: AbiType::Unit,
        schema: "",
    },
    OpDef {
        group: "Rand",
        member: "Int",
        kind: OpKind::Fixed,
        params: &[AbiType::Int, AbiType::Int],
        reply: AbiType::Int,
        schema: "",
    },
    OpDef {
        group: "Vm",
        member: "New",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "() -> EmptyVm",
    },
    OpDef {
        group: "Vm",
        member: "FromObject",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[A,T,e](EmptyVm, Fn[A,T,e], control A) -> Vm[T]",
    },
    OpDef {
        group: "Vm",
        member: "Run",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> RunResult[T]",
    },
    OpDef {
        group: "Vm",
        member: "Step",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> StepEvent[T]",
    },
    OpDef {
        group: "Vm",
        member: "Drive",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> DriveEvent[T]",
    },
    OpDef {
        group: "Vm",
        member: "Answer",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T,A,R](Vm[T], PendingCall[A,R], R) -> ()",
    },
    OpDef {
        group: "Vm",
        member: "Reject",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T], Request, Fault) -> ()",
    },
    OpDef {
        group: "Vm",
        member: "Dispatch",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T], Request) -> ()",
    },
    OpDef {
        group: "Vm",
        member: "Table",
        kind: OpKind::VmControl,
        params: &[],
        reply: AbiType::Unit,
        schema: "[T](Vm[T]) -> PolicyTable",
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
/// SHA-256 of the ABI version, the qualified name, and the complete
/// signature or schema text.
pub fn op_identity(slot: OpSlot) -> [u8; 32] {
    let def = op(slot);
    let mut input = Vec::new();
    input.extend_from_slice(b"lm-operation-v1\0");
    input.extend_from_slice(&ABI_VERSION.to_le_bytes());
    input.extend_from_slice(op_name(slot).as_bytes());
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

    #[test]
    fn fixed_member_excludes_vm_control() {
        assert_eq!(fixed_member("Io", "Print"), Some(OP_IO_PRINT));
        assert_eq!(fixed_member("Vm", "Run"), None);
        assert_eq!(fixed_member("Vm", "New"), None);
    }
}
