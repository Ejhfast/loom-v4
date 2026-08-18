//! The stable core role slots.
//!
//! The VM and the verifier need the core enum families: `Option`,
//! `Result`, `IoError`, and the three virtual-machine event families.
//! An artifact declares them in its core role table, one class index
//! per role. The compiler fills the table, the linker relocates it,
//! and the verifier proves the shape of every filled slot.
//!
//! Reading a slot therefore reads no name, no hash, and no position.
//! An artifact with no source, for example a foreign `.lma`, carries
//! its own table, so the resolution needs nothing outside the bytes.
//!
//! The file `core/pinned-core-defs.txt` pins the structural hash of
//! every core class. It is a determinism gate now, not a resolution
//! mechanism: a core edit that moves a hash must be deliberate. The
//! pin key is the pair `(qualified key, structural hash)`, because a
//! structural hash covers no name and two arms with equal shapes share
//! one value. `StepEvent.Ran` and `StepEvent.Waiting` are the pinned
//! example.

use crate::Module;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The pinned core definition hashes, one `label hash` pair per line.
const PINNED: &str = include_str!("../../../core/pinned-core-defs.txt");

/// The resolved core class indices of one module. Each entry is the
/// class index of one enum case or parent. `None` marks an absent
/// definition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CoreLayout {
    /// The core method table of immediate integer values.
    pub int: Option<u32>,
    /// The core method table of immediate Boolean values.
    pub boolean: Option<u32>,
    /// The core method table of immutable String values.
    pub string: Option<u32>,
    pub option_some: Option<u32>,
    pub option_none: Option<u32>,
    pub result_ok: Option<u32>,
    pub result_err: Option<u32>,
    pub io_error_failed: Option<u32>,
    pub run_done: Option<u32>,
    pub run_fault: Option<u32>,
    pub step_ran: Option<u32>,
    pub step_waiting: Option<u32>,
    pub step_done: Option<u32>,
    pub step_fault: Option<u32>,
    pub drive_asked: Option<u32>,
    pub drive_done: Option<u32>,
    pub drive_fault: Option<u32>,
    pub recv_msg: Option<u32>,
    pub recv_closed: Option<u32>,
    pub choice_first: Option<u32>,
    pub choice_second: Option<u32>,
    pub send_sent: Option<u32>,
    pub send_closed: Option<u32>,
    pub send_fault: Option<u32>,
    pub proc_done: Option<u32>,
    pub proc_fault: Option<u32>,
    pub proc_error_dead: Option<u32>,
    pub proc_error_not_paused: Option<u32>,
    pub proc_error_already_paused: Option<u32>,
    pub proc_error_in_use: Option<u32>,
    /// The enum parent class indices, aligned with the arms above.
    pub option: Option<u32>,
    pub result: Option<u32>,
    pub io_error: Option<u32>,
    pub run_result: Option<u32>,
    pub step_event: Option<u32>,
    pub drive_event: Option<u32>,
    pub recv: Option<u32>,
    pub choice: Option<u32>,
    pub send_result: Option<u32>,
    pub proc_result: Option<u32>,
    pub proc_error: Option<u32>,
    /// The core class `Proc`, the parent of every proc class.
    pub proc_class: Option<u32>,
    pub snapshot_error: Option<u32>,
    pub snapshot_resource_active: Option<u32>,
    pub snapshot_limit_exceeded: Option<u32>,
    pub snapshot_bad_image: Option<u32>,
    pub restore_error: Option<u32>,
    pub restore_limit_exceeded: Option<u32>,
    pub fs_error: Option<u32>,
    pub fs_error_closed: Option<u32>,
    pub fs_error_failed: Option<u32>,
    pub open_options: Option<u32>,
    pub open_read_only: Option<u32>,
    pub open_write_only: Option<u32>,
    pub open_read_write: Option<u32>,
    pub open_create: Option<u32>,
    pub open_create_truncate: Option<u32>,
    pub open_append: Option<u32>,
    pub seek_from: Option<u32>,
    pub seek_start: Option<u32>,
    pub seek_current: Option<u32>,
    pub seek_end: Option<u32>,
}

/// The labels of the pinned core definitions, in pin-file order.
pub const PINNED_LABELS: [&str; 62] = [
    "Option",
    "Option.Some",
    "Option.None",
    "Result",
    "Result.Ok",
    "Result.Err",
    "IoError",
    "IoError.Failed",
    "RunResult",
    "RunResult.Done",
    "RunResult.Fault",
    "StepEvent",
    "StepEvent.Ran",
    "StepEvent.Waiting",
    "StepEvent.Done",
    "StepEvent.Fault",
    "DriveEvent",
    "DriveEvent.Asked",
    "DriveEvent.Done",
    "DriveEvent.Fault",
    "Recv",
    "Recv.Msg",
    "Recv.Closed",
    "SendResult",
    "SendResult.Sent",
    "SendResult.Closed",
    "SendResult.Fault",
    "ProcResult",
    "ProcResult.Done",
    "ProcResult.Fault",
    "ProcError",
    "ProcError.Dead",
    "ProcError.NotPaused",
    "ProcError.AlreadyPaused",
    "ProcError.InUse",
    "Proc",
    "SnapshotError",
    "SnapshotError.ResourceActive",
    "SnapshotError.SnapshotLimitExceeded",
    "SnapshotError.BadImage",
    "RestoreError",
    "RestoreError.RestoreLimitExceeded",
    "FsError",
    "FsError.Closed",
    "FsError.Failed",
    "OpenOptions",
    "OpenOptions.ReadOnly",
    "OpenOptions.WriteOnly",
    "OpenOptions.ReadWrite",
    "OpenOptions.Create",
    "OpenOptions.CreateTruncate",
    "OpenOptions.Append",
    "SeekFrom",
    "SeekFrom.Start",
    "SeekFrom.Current",
    "SeekFrom.End",
    "Choice",
    "Choice.First",
    "Choice.Second",
    "Int",
    "Bool",
    "String",
];

/// The core role of immediate integer values.
pub const ROLE_INT: usize = 59;

/// The core role of immediate Boolean values.
pub const ROLE_BOOL: usize = 60;

/// The core role of immutable String values.
pub const ROLE_STRING: usize = 61;

fn parse_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        let hex = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(hex, 16).ok()?;
    }
    Some(out)
}

/// The qualified key of one pinned core label.
pub fn pinned_key(label: &str) -> String {
    crate::qualified_key(crate::CORE_MODULE, label)
}

/// The pinned table: `(qualified key, structural hash)` to slot label.
fn pinned_map() -> &'static HashMap<(String, [u8; 32]), &'static str> {
    static MAP: OnceLock<HashMap<(String, [u8; 32]), &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        for line in PINNED.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (label, hex) = line
                .split_once(' ')
                .expect("a pinned core line is `label hash`");
            let hash = parse_hex(hex.trim()).expect("a pinned core hash is 64 hex digits");
            let label = PINNED_LABELS
                .iter()
                .copied()
                .find(|l| *l == label)
                .expect("a pinned core label is known");
            map.insert((pinned_key(label), hash), label);
        }
        assert_eq!(
            map.len(),
            PINNED_LABELS.len(),
            "the pinned core table must cover every label once"
        );
        map
    })
}

fn slot_mut<'a>(layout: &'a mut CoreLayout, label: &str) -> &'a mut Option<u32> {
    match label {
        "Int" => &mut layout.int,
        "Bool" => &mut layout.boolean,
        "String" => &mut layout.string,
        "Option" => &mut layout.option,
        "Option.Some" => &mut layout.option_some,
        "Option.None" => &mut layout.option_none,
        "Result" => &mut layout.result,
        "Result.Ok" => &mut layout.result_ok,
        "Result.Err" => &mut layout.result_err,
        "IoError" => &mut layout.io_error,
        "IoError.Failed" => &mut layout.io_error_failed,
        "RunResult" => &mut layout.run_result,
        "RunResult.Done" => &mut layout.run_done,
        "RunResult.Fault" => &mut layout.run_fault,
        "StepEvent" => &mut layout.step_event,
        "StepEvent.Ran" => &mut layout.step_ran,
        "StepEvent.Waiting" => &mut layout.step_waiting,
        "StepEvent.Done" => &mut layout.step_done,
        "StepEvent.Fault" => &mut layout.step_fault,
        "DriveEvent" => &mut layout.drive_event,
        "DriveEvent.Asked" => &mut layout.drive_asked,
        "DriveEvent.Done" => &mut layout.drive_done,
        "DriveEvent.Fault" => &mut layout.drive_fault,
        "Recv" => &mut layout.recv,
        "Recv.Msg" => &mut layout.recv_msg,
        "Recv.Closed" => &mut layout.recv_closed,
        "Choice" => &mut layout.choice,
        "Choice.First" => &mut layout.choice_first,
        "Choice.Second" => &mut layout.choice_second,
        "SendResult" => &mut layout.send_result,
        "SendResult.Sent" => &mut layout.send_sent,
        "SendResult.Closed" => &mut layout.send_closed,
        "SendResult.Fault" => &mut layout.send_fault,
        "ProcResult" => &mut layout.proc_result,
        "ProcResult.Done" => &mut layout.proc_done,
        "ProcResult.Fault" => &mut layout.proc_fault,
        "ProcError" => &mut layout.proc_error,
        "ProcError.Dead" => &mut layout.proc_error_dead,
        "ProcError.NotPaused" => &mut layout.proc_error_not_paused,
        "ProcError.AlreadyPaused" => &mut layout.proc_error_already_paused,
        "ProcError.InUse" => &mut layout.proc_error_in_use,
        "Proc" => &mut layout.proc_class,
        "SnapshotError" => &mut layout.snapshot_error,
        "SnapshotError.ResourceActive" => &mut layout.snapshot_resource_active,
        "SnapshotError.SnapshotLimitExceeded" => &mut layout.snapshot_limit_exceeded,
        "SnapshotError.BadImage" => &mut layout.snapshot_bad_image,
        "RestoreError" => &mut layout.restore_error,
        "RestoreError.RestoreLimitExceeded" => &mut layout.restore_limit_exceeded,
        "FsError" => &mut layout.fs_error,
        "FsError.Closed" => &mut layout.fs_error_closed,
        "FsError.Failed" => &mut layout.fs_error_failed,
        "OpenOptions" => &mut layout.open_options,
        "OpenOptions.ReadOnly" => &mut layout.open_read_only,
        "OpenOptions.WriteOnly" => &mut layout.open_write_only,
        "OpenOptions.ReadWrite" => &mut layout.open_read_write,
        "OpenOptions.Create" => &mut layout.open_create,
        "OpenOptions.CreateTruncate" => &mut layout.open_create_truncate,
        "OpenOptions.Append" => &mut layout.open_append,
        "SeekFrom" => &mut layout.seek_from,
        "SeekFrom.Start" => &mut layout.seek_start,
        "SeekFrom.Current" => &mut layout.seek_current,
        "SeekFrom.End" => &mut layout.seek_end,
        _ => unreachable!("only known labels enter the map"),
    }
}

fn set_slot(layout: &mut CoreLayout, label: &str, idx: u32) {
    *slot_mut(layout, label) = Some(idx);
}

fn slot_of(layout: &CoreLayout, label: &str) -> Option<u32> {
    // The read borrows nothing mutable; the clone keeps one label
    // table instead of two.
    let mut copy = *layout;
    *slot_mut(&mut copy, label)
}

/// The core role index of one pinned label, or `None` when the label
/// is unknown. The order is `PINNED_LABELS`.
pub fn role_index(label: &str) -> Option<usize> {
    PINNED_LABELS.iter().position(|l| *l == label)
}

/// Read the declared core layout of one module.
///
/// The artifact carries the table, the compiler filled it, and the
/// verifier proves the shape of every filled slot. This function reads
/// slots only: no name, no hash, and no position takes part.
pub fn declared_layout(module: &Module) -> CoreLayout {
    let mut layout = CoreLayout::default();
    for (role, slot) in module.core_roles.iter().enumerate() {
        if *slot == crate::NO_ROLE {
            continue;
        }
        set_slot(&mut layout, PINNED_LABELS[role], *slot);
    }
    layout
}

/// The core role table that matches one layout, for a module builder.
pub fn roles_of(layout: &CoreLayout) -> [u32; crate::CORE_ROLE_COUNT] {
    let mut roles = [crate::NO_ROLE; crate::CORE_ROLE_COUNT];
    for (role, label) in PINNED_LABELS.iter().enumerate() {
        if let Some(idx) = slot_of(layout, label) {
            roles[role] = idx;
        }
    }
    roles
}

/// The pinned structural hash of one label, for the determinism gate.
pub fn pinned_hash(label: &str) -> Option<[u8; 32]> {
    pinned_map()
        .iter()
        .find(|((key, _), found)| *found == &label && *key == pinned_key(label))
        .map(|((_, hash), _)| *hash)
}
