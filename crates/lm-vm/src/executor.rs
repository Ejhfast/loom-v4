//! The machine executor boundary.
//!
//! The executor owns one machine and reads immutable code.
//! It returns before any operation needs world state.

use crate::machine::{ExecOutcome, ImageSlotTarget, Machine, VmId};
use crate::{DispatchRow, FaultCode};
use lm_bytecode::closed::TypeEnvs;
use lm_bytecode::Module;
use std::sync::Arc;

/// One immutable verified execution view.
pub(crate) struct ExecutionCode {
    module: Arc<Module>,
    dispatch: Arc<[DispatchRow]>,
}

impl ExecutionCode {
    // Stage 3 uses this constructor when worker dispatch starts.
    #[allow(dead_code)]
    pub(crate) fn new(module: Arc<Module>, dispatch: Arc<[DispatchRow]>) -> ExecutionCode {
        ExecutionCode { module, dispatch }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExecutionToken {
    pub(crate) world: u64,
    pub(crate) machine: VmId,
    pub(crate) generation: u32,
    pub(crate) lease: u64,
}

/// One exclusive machine execution lease.
///
/// Only `lm-vm` can create this value.
pub struct ExecutionLease {
    token: ExecutionToken,
    machine: Box<Machine>,
    code: Arc<ExecutionCode>,
    envs: Box<TypeEnvs>,
    slots: Option<Arc<Vec<ImageSlotTarget>>>,
    instruction_limit: u32,
}

impl ExecutionLease {
    // Stage 3 uses this constructor when worker dispatch starts.
    #[allow(dead_code)]
    pub(crate) fn new(
        token: ExecutionToken,
        mut machine: Box<Machine>,
        code: Arc<ExecutionCode>,
        envs: Box<TypeEnvs>,
        slots: Option<Arc<Vec<ImageSlotTarget>>>,
        instruction_limit: u32,
    ) -> ExecutionLease {
        machine.vm.heap.begin_execution_lease();
        ExecutionLease {
            token,
            machine,
            code,
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

pub(crate) struct InlineExecutionReport {
    stop: ExecutionStop,
    retired_instructions: u32,
}

impl InlineExecutionReport {
    pub(crate) fn into_parts(self) -> (ExecutionStop, u32, bool) {
        let boundary = !matches!(self.stop, ExecutionStop::QuantumExpired);
        (self.stop, self.retired_instructions, boundary)
    }
}

/// One complete machine execution report.
pub struct ExecutionReport {
    #[allow(dead_code)]
    token: ExecutionToken,
    #[allow(dead_code)]
    machine: Box<Machine>,
    #[allow(dead_code)]
    code: Arc<ExecutionCode>,
    #[allow(dead_code)]
    envs: Box<TypeEnvs>,
    stop: ExecutionStop,
    retired_instructions: u32,
    heap_before: usize,
    heap_after: usize,
    objects_before: usize,
    objects_after: usize,
    unused_heap_bytes: usize,
    unused_heap_objects: usize,
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

    /// The local heap bytes released during this execution.
    pub fn heap_released_bytes(&self) -> usize {
        self.heap_before.saturating_sub(self.heap_after)
    }

    /// The positive local object growth during this execution.
    pub fn heap_growth_objects(&self) -> usize {
        self.objects_after.saturating_sub(self.objects_before)
    }

    /// The local objects released during this execution.
    pub fn heap_released_objects(&self) -> usize {
        self.objects_before.saturating_sub(self.objects_after)
    }

    /// The unused byte allowance in this execution lease.
    pub fn unused_heap_bytes(&self) -> usize {
        self.unused_heap_bytes
    }

    /// The unused object allowance in this execution lease.
    pub fn unused_heap_objects(&self) -> usize {
        self.unused_heap_objects
    }

    // Stage 3 commits these parts after a worker returns them.
    #[allow(dead_code)]
    pub(crate) fn into_parts(
        mut self,
    ) -> (
        ExecutionToken,
        Box<Machine>,
        Arc<ExecutionCode>,
        Box<TypeEnvs>,
        ExecutionStop,
        u32,
    ) {
        self.machine.vm.heap.end_execution_lease();
        (
            self.token,
            self.machine,
            self.code,
            self.envs,
            self.stop,
            self.retired_instructions,
        )
    }
}

/// Execute one lease until a boundary or instruction limit.
pub fn execute(lease: ExecutionLease) -> ExecutionReport {
    let ExecutionLease {
        token,
        mut machine,
        code,
        mut envs,
        slots,
        instruction_limit,
    } = lease;
    let heap_before = machine.vm.heap.used_bytes();
    let objects_before = machine.vm.heap.stats().live;
    let slots = slots.as_deref().map(Vec::as_slice);
    let (outcome, retired_instructions) = machine.exec_for_quantum(
        code.module.as_ref(),
        code.dispatch.as_ref(),
        &mut envs,
        slots,
        instruction_limit,
    );
    let heap_after = machine.vm.heap.used_bytes();
    let objects_after = machine.vm.heap.stats().live;
    let (unused_heap_bytes, unused_heap_objects) =
        machine.vm.heap.execution_lease_unused().unwrap_or_default();
    let stop = match outcome {
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(code) => ExecutionStop::Fault(code),
    };
    ExecutionReport {
        token,
        machine,
        code,
        envs,
        stop,
        retired_instructions,
        heap_before,
        heap_after,
        objects_before,
        objects_after,
        unused_heap_bytes,
        unused_heap_objects,
    }
}

/// Execute one deterministic slice through borrowed state.
pub(crate) fn execute_inline(
    machine: &mut Machine,
    module: &Module,
    dispatch: &[DispatchRow],
    envs: &mut TypeEnvs,
    slots: Option<&[ImageSlotTarget]>,
    instruction_limit: u32,
) -> InlineExecutionReport {
    let (outcome, retired_instructions) =
        machine.exec_for_quantum(module, dispatch, envs, slots, instruction_limit);
    let stop = match outcome {
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(code) => ExecutionStop::Fault(code),
    };
    InlineExecutionReport {
        stop,
        retired_instructions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceBudget;
    use crate::VmConfig;
    use lm_bytecode::{BcType, Func, Instr, Module};
    use lm_heap::{Heap, HeapBudget};
    use lm_value::{TypeEnvId, Value};

    fn assert_send<T: Send>() {}

    #[test]
    fn every_execution_lease_field_is_send() {
        assert_send::<ExecutionToken>();
        assert_send::<Machine>();
        assert_send::<Box<Machine>>();
        assert_send::<Arc<Module>>();
        assert_send::<Arc<[DispatchRow]>>();
        assert_send::<Arc<ExecutionCode>>();
        assert_send::<TypeEnvs>();
        assert_send::<Box<TypeEnvs>>();
        assert_send::<Arc<Vec<ImageSlotTarget>>>();
        assert_send::<u32>();
        assert_send::<ExecutionLease>();
        assert_send::<ExecutionReport>();
    }

    #[test]
    fn heap_budget_leases_are_send() {
        assert_send::<HeapBudget>();
        assert_send::<Heap>();
    }

    #[test]
    fn one_owned_lease_executes_on_another_thread() {
        let module = Arc::new(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Int],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            classes: vec![],
            funcs: vec![Func {
                name: "main".to_string(),
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 1,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(42), Instr::Return]],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        });
        let config = VmConfig {
            heap_bytes: 1024,
            ..VmConfig::default()
        };
        let budget = HeapBudget::new(1024, 64);
        let mut machine = Box::new(Machine::empty_with_budgets(
            config,
            None,
            0,
            budget.clone(),
            ResourceBudget::new(config.max_resources as usize),
        ));
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let lease = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 1,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Vec::new().into())),
            Box::new(TypeEnvs::default()),
            None,
            16,
        );
        assert_eq!(budget.used_bytes(), 1024);
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the worker returns one report");
        assert_eq!(report.heap_growth_bytes(), 0);
        assert_eq!(report.heap_released_bytes(), 0);
        assert_eq!(report.heap_growth_objects(), 0);
        assert_eq!(report.heap_released_objects(), 0);
        assert_eq!(report.unused_heap_bytes(), 1024);
        assert_eq!(report.unused_heap_objects(), 64);
        let (_, machine, _, _, stop, retired) = report.into_parts();
        assert_eq!(retired, 2);
        assert_eq!(budget.used_bytes(), 0);
        assert!(matches!(
            stop,
            ExecutionStop::Boundary(ExecOutcome::Terminal(Value::Int(42)))
        ));
        drop(machine);
        assert_eq!(budget.used_bytes(), 0);
    }
}
