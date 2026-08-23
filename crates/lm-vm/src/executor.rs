//! The machine executor boundary.
//!
//! The executor reads one machine and immutable code.
//! It returns before any operation needs world state.

use crate::machine::{ExecOutcome, ImageSlotTarget, Machine};
use crate::{DispatchRow, FaultCode};
use lm_bytecode::closed::TypeEnvs;
use lm_bytecode::Module;

/// One exclusive machine execution lease.
///
/// Only `lm-vm` can create this value.
pub struct ExecutionLease<'a> {
    machine: &'a mut Machine,
    module: &'a Module,
    dispatch: &'a [DispatchRow],
    envs: &'a mut TypeEnvs,
    slots: Option<&'a [ImageSlotTarget]>,
    instruction_limit: u32,
}

impl<'a> ExecutionLease<'a> {
    pub(crate) fn new(
        machine: &'a mut Machine,
        module: &'a Module,
        dispatch: &'a [DispatchRow],
        envs: &'a mut TypeEnvs,
        slots: Option<&'a [ImageSlotTarget]>,
        instruction_limit: u32,
    ) -> ExecutionLease<'a> {
        ExecutionLease {
            machine,
            module,
            dispatch,
            envs,
            slots,
            instruction_limit,
        }
    }
}

pub(crate) enum ExecutionStop {
    QuantumExpired,
    Boundary(ExecOutcome),
    Fault(FaultCode),
}

/// One complete machine execution report.
pub struct ExecutionReport {
    stop: ExecutionStop,
    retired_instructions: u32,
    heap_before: usize,
    heap_after: usize,
}

impl ExecutionReport {
    /// The number of retired bytecode instructions.
    pub fn retired_instructions(&self) -> u32 {
        self.retired_instructions
    }

    /// True when execution reached a semantic boundary.
    pub fn reached_boundary(&self) -> bool {
        !matches!(self.stop, ExecutionStop::QuantumExpired)
    }

    /// The positive local heap growth during this execution.
    pub fn heap_growth_bytes(&self) -> usize {
        self.heap_after.saturating_sub(self.heap_before)
    }

    pub(crate) fn into_parts(self) -> (ExecutionStop, u32) {
        (self.stop, self.retired_instructions)
    }
}

/// Execute one lease until a boundary or instruction limit.
pub fn execute(lease: ExecutionLease<'_>) -> ExecutionReport {
    let heap_before = lease.machine.vm.heap.used_bytes();
    let (outcome, retired_instructions) = lease.machine.exec_for_quantum(
        lease.module,
        lease.dispatch,
        lease.envs,
        lease.slots,
        lease.instruction_limit,
    );
    let heap_after = lease.machine.vm.heap.used_bytes();
    let stop = match outcome {
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(code) => ExecutionStop::Fault(code),
    };
    ExecutionReport {
        stop,
        retired_instructions,
        heap_before,
        heap_after,
    }
}
