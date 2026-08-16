//! The graph engine: one non-recursive traversal for every mode.
//!
//! Mark, deep freeze, frozen verification, boundary transfer,
//! structural copy, canonical digest, detached inspection, and
//! snapshot traversal all run the same walk. The walk owns
//! reachability, the child order each native shape declares, the
//! identity table, the canonical traversal ordinals, and the object,
//! edge, byte, and work limits. A mode adds only its own result
//! state.
//!
//! Depth never reaches the Rust stack. Transfer and copy commit
//! destination state only after the whole source graph passes, and a
//! rejected copy frees every shell it allocated.
//!
//! The crate reads the heap and the shape table of `lm-heap`. It has
//! no VM, bytecode, filesystem, clock, or network dependency.

pub mod digest;
pub mod engine;
pub mod mode;

pub use digest::CodeIdentity;
pub use engine::{walk, GraphCost, GraphLimits, Visitor};
pub use mode::{
    collect, copy_within, detach, digest_value, freeze, snapshot_ordinals, transfer, verify_frozen,
    verify_sendable, CopyMode,
};
