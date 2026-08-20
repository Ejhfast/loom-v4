//! The scheduler-facing entry of the snapshot barrier
//! (specification 17.3).
//!
//! The cut algorithm itself lives in `lm_vm::snapshot::write`, because
//! the guest operation `Vm.SnapshotHeld` runs inside the driver loop
//! and `lm-vm` depends on no scheduler. This module is the entry the
//! scheduler calls, and it reports what one cut recorded. One world
//! therefore has exactly one cut algorithm.

use lm_vm::snapshot::{CutError, SnapshotImage};
use lm_vm::{FaultCode, VmId, World};

/// Why one barrier did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierError {
    /// Another barrier already holds one machine of the set. Barriers
    /// over overlapping worlds serialize; the caller retries.
    Overlaps(VmId),
    /// One machine of the set holds a live host attachment, so the
    /// codec has no bytes to copy (specification 17.4). The path
    /// starts at the root and names machines by ordinal.
    ResourceActive { path: Vec<u32>, kind: String },
    /// A traversal of one machine reached a graph limit.
    Limit(VmId, FaultCode),
    /// The world holds a machine no cut can copy.
    NotCapturable(VmId, String),
    /// The container passed the configured snapshot byte limit.
    TooLarge,
}

impl From<CutError> for BarrierError {
    fn from(error: CutError) -> BarrierError {
        match error {
            CutError::Overlaps(vm) => BarrierError::Overlaps(vm),
            CutError::ResourceActive { path, kind } => BarrierError::ResourceActive { path, kind },
            CutError::Limit(vm, code) => BarrierError::Limit(vm, code),
            CutError::NotCapturable(vm, message) => BarrierError::NotCapturable(vm, message),
            CutError::TooLarge => BarrierError::TooLarge,
        }
    }
}

/// What one finished barrier recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarrierReport {
    /// Every machine of the closed set, in ascending identifier
    /// order. Every handle in the stopped state targets one of them.
    pub set: Vec<VmId>,
    /// The canonical machine order: ordinal `i` names `order[i]`.
    pub order: Vec<VmId>,
    /// The one mailbox acceptance cut of this barrier.
    pub cut: u64,
    /// The number of objects the canonical traversal ordered, summed
    /// over the set.
    pub objects: usize,
    /// True when the barrier resumed every machine it stopped.
    pub resumed: bool,
}

/// One barrier over one machine world.
pub struct Barrier {
    id: u32,
}

impl Barrier {
    /// A barrier with one identifier. Two live barriers must carry
    /// different identifiers.
    pub fn new(id: u32) -> Barrier {
        Barrier { id }
    }

    /// Run one barrier from `root` without encoding.
    ///
    /// The steps follow specification 17.3 in order: stop, close,
    /// freeze, record, preflight, resume. A failure resumes every
    /// machine the barrier stopped, so the original world is
    /// unchanged.
    pub fn run(&self, world: &mut World, root: VmId) -> Result<BarrierReport, BarrierError> {
        let report = world.run_cut(self.id, root)?;
        // The barrier itself encodes nothing, so it resumes the
        // original world here.
        world.release_cut(&report.set, true);
        Ok(BarrierReport {
            set: report.set,
            order: report.order,
            cut: report.cut,
            objects: report.objects,
            resumed: true,
        })
    }

    /// Run one barrier from `root` and encode the machine world.
    ///
    /// This is the writer entry of specification 17.3 step 6. The
    /// world resumes after the encoding, whatever the encoding
    /// answered.
    pub fn capture(
        &self,
        world: &mut World,
        root: VmId,
    ) -> Result<SnapshotImage, lm_vm::snapshot::SnapshotFail> {
        let image = world.capture_snapshot(self.id, root, false)?;
        world.trust_image(&image);
        Ok(image)
    }
}
