//! The scheduler-record gate: a scheduler record holds no guest heap
//! reference.
//!
//! The rule is structural, so the test reads the structure. The crate
//! depends on the manifest and the VM only, and every record it
//! publishes is plain data of identifiers, ordinals, and counters.

use lm_proc::{BarrierError, BarrierReport, SchedulerStats};
use lm_vm::TraceEvent;

/// The crate manifest names no heap or value crate. A scheduler
/// record therefore cannot name a guest object at all.
#[test]
fn the_crate_depends_on_no_heap_or_value_crate() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate manifest reads");
    let deps: Vec<&str> = manifest
        .lines()
        .skip_while(|line| !line.starts_with("[dependencies]"))
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('=').next().unwrap_or("").trim())
        .collect();
    assert_eq!(deps, vec!["lm-abi", "lm-vm"], "{manifest}");
}

/// Every published record is plain copyable data whose fields are
/// machine identifiers, generations, ordinals, and counters.
#[test]
fn every_record_is_plain_data() {
    fn is_copy<T: Copy>() {}
    is_copy::<TraceEvent>();
    is_copy::<SchedulerStats>();
    // A machine identifier is one integer, so a whole trace event
    // stays small. A guest reference would not fit this budget.
    assert!(std::mem::size_of::<TraceEvent>() <= 16);
    assert!(std::mem::size_of::<SchedulerStats>() <= 32);
    // The barrier report names machines and counts only.
    let report = BarrierReport {
        set: vec![0, 1],
        cut: 3,
        objects: 9,
        resumed: true,
    };
    assert_eq!(
        format!("{report:?}"),
        "BarrierReport { set: [0, 1], cut: 3, objects: 9, resumed: true }"
    );
    assert_eq!(format!("{:?}", BarrierError::Overlaps(2)), "Overlaps(2)");
}
