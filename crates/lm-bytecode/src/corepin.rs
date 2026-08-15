//! Hash linking for the pinned core definitions.
//!
//! Core definitions receive definition hashes like every other
//! definition. The file `core/pinned-core-defs.txt` pins the hashes
//! of the core classes the verifier and the VM need. At load, the
//! module identity is computed and every class that matches a pinned
//! `(label, hash)` pair fills its layout slot.
//!
//! The key is the pair, not the hash alone. Class identity is nominal
//! since compiler ABI version 2, so the name is already inside the
//! hash; the explicit label check keeps the resolution correct if a
//! later identity change drops the name again. No position takes part
//! in the resolution.
//!
//! A module can hold two classes with one label and one hash, for
//! example a user enum that copies a core enum exactly. Such classes
//! are the same definition, so the choice between them is arbitrary.
//! The rule is fixed for determinism: the lowest class index wins.
//! Nothing depends on the emission order of the compiler.

use crate::identity::ModuleIdentity;
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
    /// The enum parent class indices, aligned with the arms above.
    pub option: Option<u32>,
    pub result: Option<u32>,
    pub io_error: Option<u32>,
    pub run_result: Option<u32>,
    pub step_event: Option<u32>,
    pub drive_event: Option<u32>,
}

/// The labels of the pinned core definitions, in pin-file order.
pub const PINNED_LABELS: [&str; 20] = [
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
];

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

/// The pinned table: definition hash to slot label.
fn pinned_map() -> &'static HashMap<[u8; 32], &'static str> {
    static MAP: OnceLock<HashMap<[u8; 32], &'static str>> = OnceLock::new();
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
            map.insert(hash, label);
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

/// Resolve the core layout of one module through its pinned
/// `(label, hash)` pairs. The verifier and the VM share the one table
/// built here. The lowest matching class index wins.
pub fn core_layout(module: &Module, identity: &ModuleIdentity) -> CoreLayout {
    debug_assert_eq!(identity.class_hashes.len(), module.classes.len());
    let map = pinned_map();
    let mut layout = CoreLayout::default();
    for (idx, hash) in identity.class_hashes.iter().enumerate() {
        let Some(label) = map.get(hash) else {
            continue;
        };
        if module.classes[idx].name != *label {
            continue;
        }
        if slot_of(&layout, label).is_none() {
            set_slot(&mut layout, label, idx as u32);
        }
    }
    layout
}
