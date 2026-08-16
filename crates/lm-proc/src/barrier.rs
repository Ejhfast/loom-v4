//! The scheduler barrier of specification 17.3.
//!
//! The barrier defines one moment of one machine world. It stops the
//! root and every reachable machine at an instruction boundary, closes
//! the set over the handles it finds in the stopped state, freezes
//! mailbox acceptance at one cut marker, records the machine states,
//! and preflights the host attachments.
//!
//! Week 8 defines no snapshot byte format, so the barrier always
//! resumes the world. It reports the closed set, the cut marker, and
//! the preflight result, which is what the week-9 encoder consumes.

use lm_vm::{FaultCode, VmId, World};

/// Why one barrier did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierError {
    /// Another barrier already holds one machine of the set. Barriers
    /// over overlapping worlds serialize; the caller retries.
    Overlaps(VmId),
    /// One machine of the set holds a live host attachment, so the
    /// codec has no bytes to copy (specification 17.4).
    ResourceActive(VmId),
    /// A traversal of one machine reached a graph limit.
    Limit(VmId, FaultCode),
}

/// What one finished barrier recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarrierReport {
    /// Every machine of the closed set, in ascending identifier
    /// order. Every handle in the stopped state targets one of them.
    pub set: Vec<VmId>,
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

    /// Run one barrier from `root`.
    ///
    /// The steps follow specification 17.3 in order: stop, close,
    /// freeze, record, preflight, resume. A failure resumes every
    /// machine the barrier stopped, so the original world is
    /// unchanged.
    pub fn run(&self, world: &mut World<'_>, root: VmId) -> Result<BarrierReport, BarrierError> {
        let mut set: Vec<VmId> = Vec::new();
        let mut queue: Vec<VmId> = vec![root];
        // Steps 1 and 2: stop the reachable machines and close the set
        // over the handles their stopped state holds.
        while let Some(vm) = queue.pop() {
            if set.contains(&vm) {
                continue;
            }
            if let Some(other) = world.barrier_of(vm) {
                if other != self.id {
                    self.release(world, &set, false);
                    return Err(BarrierError::Overlaps(vm));
                }
            }
            world.set_barrier(vm, Some(self.id));
            set.push(vm);
            if !world.is_live_machine(vm) {
                continue;
            }
            match world.machine_references(vm) {
                Ok(found) => queue.extend(found),
                Err(code) => {
                    self.release(world, &set, false);
                    return Err(BarrierError::Limit(vm, code));
                }
            }
        }
        set.sort_unstable();
        // Step 3: freeze mailbox acceptance for the whole set at one
        // cut marker.
        let cut = world.next_cut();
        for vm in &set {
            world.freeze_mailbox(*vm, true);
        }
        // Steps 4 and 5: record the states and preflight the host
        // attachments.
        let mut objects = 0;
        for vm in &set {
            if !world.is_live_machine(*vm) {
                continue;
            }
            match world.snapshot_preflight(*vm) {
                Ok(count) => objects += count,
                Err(FaultCode::BoundaryViolation) => {
                    self.release(world, &set, true);
                    return Err(BarrierError::ResourceActive(*vm));
                }
                Err(code) => {
                    self.release(world, &set, true);
                    return Err(BarrierError::Limit(*vm, code));
                }
            }
        }
        // Step 7: week 8 encodes nothing, so the barrier resumes the
        // original world here.
        self.release(world, &set, true);
        Ok(BarrierReport {
            set,
            cut,
            objects,
            resumed: true,
        })
    }

    /// Resume every machine the barrier stopped.
    fn release(&self, world: &mut World<'_>, set: &[VmId], thaw: bool) {
        for vm in set {
            if thaw {
                world.freeze_mailbox(*vm, false);
            }
            world.set_barrier(*vm, None);
        }
    }
}
