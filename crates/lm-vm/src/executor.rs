//! The machine executor boundary.
//!
//! The executor owns one machine and reads immutable code.
//! It returns before any operation needs world state.

use crate::machine::{ExecError, ExecOutcome, ImageSlotTarget, Machine, VmId};
use crate::resource::ResourceBudgetReservation;
use crate::{DispatchRow, FaultCode};
use lm_bytecode::closed::TypeEnvs;
use lm_bytecode::Module;
use lm_heap::HeapExecutionTicket;
use std::sync::Arc;

/// One immutable verified execution view.
pub(crate) struct ExecutionCode {
    module: Arc<Module>,
    dispatch: Arc<[DispatchRow]>,
}

impl ExecutionCode {
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
    restricted_world: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ExecutionLimits {
    pub(crate) instructions: u32,
    pub(crate) heap_trip_bytes: usize,
    pub(crate) heap_trip_objects: usize,
    pub(crate) exclusive_world: bool,
}

/// Coordinator-owned accounting for one execution lease.
///
/// The marker keeps this value outside worker jobs.
#[must_use = "the coordinator must commit or cancel this execution reservation"]
pub(crate) struct ExecutionReservation {
    token: ExecutionToken,
    heap: HeapExecutionTicket,
    resources: ResourceBudgetReservation,
    coordinator_only: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ExecutionReservation {
    /// Release all charges after the worker machine was destroyed.
    pub(crate) fn cancel_destroyed(self) {
        self.resources.cancel_destroyed();
        self.heap.cancel_destroyed();
    }
}

impl ExecutionLease {
    pub(crate) fn new(
        token: ExecutionToken,
        mut machine: Box<Machine>,
        code: Arc<ExecutionCode>,
        envs: Box<TypeEnvs>,
        slots: Option<Arc<Vec<ImageSlotTarget>>>,
        limits: ExecutionLimits,
    ) -> (ExecutionLease, ExecutionReservation) {
        let heap = machine.vm.heap.begin_execution_lease(
            token.lease,
            limits.heap_trip_bytes,
            limits.heap_trip_objects,
        );
        let resources = machine.resources.begin_execution_lease(token.lease);
        let reservation = ExecutionReservation {
            token,
            heap,
            resources,
            coordinator_only: std::marker::PhantomData,
        };
        (
            ExecutionLease {
                token,
                machine,
                code,
                envs,
                slots,
                instruction_limit: limits.instructions,
                restricted_world: !limits.exclusive_world,
            },
            reservation,
        )
    }
}

pub(crate) enum ExecutionStop {
    QuantumExpired,
    HeapTrip,
    NeedsQuiescence,
    Boundary(ExecOutcome),
    Fault(FaultCode),
}

pub(crate) struct ExecutionCommit {
    pub(crate) stop: ExecutionStop,
    pub(crate) retired: u32,
    pub(crate) reached_boundary: bool,
    pub(crate) charge_fuel: bool,
}

pub(crate) struct InlineExecutionReport {
    stop: ExecutionStop,
    retired_instructions: u32,
}

impl InlineExecutionReport {
    pub(crate) fn into_commit(self) -> ExecutionCommit {
        let boundary = !matches!(
            self.stop,
            ExecutionStop::QuantumExpired | ExecutionStop::HeapTrip
        );
        ExecutionCommit {
            stop: self.stop,
            retired: self.retired_instructions,
            reached_boundary: boundary,
            charge_fuel: true,
        }
    }
}

/// One complete machine execution report.
#[must_use = "the coordinator must commit this execution report"]
pub struct ExecutionReport {
    token: ExecutionToken,
    machine: Box<Machine>,
    code: Arc<ExecutionCode>,
    envs: Box<TypeEnvs>,
    stop: ExecutionStop,
    retired_instructions: u32,
    heap_before: usize,
    heap_after: usize,
    objects_before: usize,
    objects_after: usize,
}

impl ExecutionReport {
    pub(crate) fn token(&self) -> ExecutionToken {
        self.token
    }

    #[cfg(test)]
    pub(crate) fn replace_token_for_test(&mut self, token: ExecutionToken) {
        self.token = token;
    }

    /// The number of retired bytecode instructions.
    pub fn retired_instructions(&self) -> u32 {
        self.retired_instructions
    }

    /// True when execution reached a semantic boundary.
    pub fn reached_boundary(&self) -> bool {
        !matches!(
            self.stop,
            ExecutionStop::QuantumExpired
                | ExecutionStop::HeapTrip
                | ExecutionStop::NeedsQuiescence
        )
    }

    /// True when local heap growth crossed the soft trip point.
    pub fn stopped_at_heap_trip(&self) -> bool {
        matches!(self.stop, ExecutionStop::HeapTrip)
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

    pub(crate) fn into_parts(
        mut self,
        reservation: ExecutionReservation,
    ) -> (
        ExecutionToken,
        Box<Machine>,
        Arc<ExecutionCode>,
        Box<TypeEnvs>,
        ExecutionStop,
        u32,
    ) {
        debug_assert_eq!(self.token, reservation.token);
        let within_heap_limit = self.machine.vm.heap.end_execution_lease(reservation.heap);
        if !within_heap_limit {
            self.stop = ExecutionStop::Fault(FaultCode::HeapLimit);
        }
        self.machine
            .resources
            .end_execution_lease(reservation.resources);
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
        restricted_world,
    } = lease;
    let heap_before = machine.vm.heap.used_bytes();
    let objects_before = machine.vm.heap.stats().live;
    let slots = slots.as_deref().map(Vec::as_slice);
    let (outcome, retired_instructions) = if restricted_world {
        machine.exec_for_quantum_restricted(
            code.module.as_ref(),
            code.dispatch.as_ref(),
            &mut envs,
            slots,
            instruction_limit,
        )
    } else {
        machine.exec_for_quantum(
            code.module.as_ref(),
            code.dispatch.as_ref(),
            &mut envs,
            slots,
            instruction_limit,
        )
    };
    let heap_after = machine.vm.heap.used_bytes();
    let objects_after = machine.vm.heap.stats().live;
    let stop = match outcome {
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(ExecError::Fault(code)) => ExecutionStop::Fault(code),
        Err(ExecError::HeapTrip) => ExecutionStop::HeapTrip,
        Err(ExecError::NeedsQuiescence) => ExecutionStop::NeedsQuiescence,
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
        Err(ExecError::Fault(code)) => ExecutionStop::Fault(code),
        Err(ExecError::HeapTrip) => {
            unreachable!("inline execution has no heap trip point")
        }
        Err(ExecError::NeedsQuiescence) => {
            unreachable!("inline execution is already coordinator-resident")
        }
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
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 1,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Vec::new().into())),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                heap_trip_bytes: usize::MAX,
                heap_trip_objects: usize::MAX,
                exclusive_world: true,
            },
        );
        assert_eq!(budget.used_bytes(), 0);
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the worker returns one report");
        assert_eq!(report.heap_growth_bytes(), 0);
        assert_eq!(report.heap_released_bytes(), 0);
        assert_eq!(report.heap_growth_objects(), 0);
        assert_eq!(report.heap_released_objects(), 0);
        let (_, machine, _, _, stop, retired) = report.into_parts(reservation);
        assert_eq!(retired, 2);
        assert_eq!(budget.used_bytes(), 0);
        assert!(matches!(
            stop,
            ExecutionStop::Boundary(ExecOutcome::Terminal(Value::Int(42)))
        ));
        drop(machine);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[test]
    fn a_worker_heap_overshoot_faults_at_coordinator_commit() {
        let module = Arc::new(Module {
            strings: vec!["allocation".to_string()],
            bytes: vec![],
            types: vec![BcType::Str],
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
                ret: 0,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstStr(0), Instr::Return]],
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
        let budget = HeapBudget::new(1, 64);
        let mut machine = Box::new(Machine::empty_with_budgets(
            config,
            None,
            0,
            budget.clone(),
            ResourceBudget::new(config.max_resources as usize),
        ));
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 2,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Vec::new().into())),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                heap_trip_bytes: usize::MAX,
                heap_trip_objects: usize::MAX,
                exclusive_world: false,
            },
        );
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the worker returns one report");
        assert!(report.heap_growth_bytes() > 1);
        let (_, machine, _, _, stop, retired) = report.into_parts(reservation);
        assert_eq!(retired, 2);
        assert!(matches!(stop, ExecutionStop::Fault(FaultCode::HeapLimit)));
        assert!(budget.used_bytes() > 1);
        drop(machine);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[test]
    fn the_coordinator_cancels_a_worker_drop() {
        let module = Arc::new(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit],
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
                ret: 0,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstUnit, Instr::Return]],
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
        let heap_budget = HeapBudget::new(1024, 64);
        let resource_budget = ResourceBudget::new(4);
        let mut machine = Box::new(Machine::empty_with_budgets(
            config,
            None,
            0,
            heap_budget.clone(),
            resource_budget.clone(),
        ));
        machine
            .resources
            .register(crate::resource::ResourceKind::File, 0, 1, 0, 0)
            .expect("the resource fits");
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 2,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Vec::new().into())),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                heap_trip_bytes: usize::MAX,
                heap_trip_objects: usize::MAX,
                exclusive_world: true,
            },
        );
        assert_eq!(heap_budget.used_bytes(), 0);
        assert_eq!(resource_budget.used(), 1);
        std::thread::spawn(move || drop(lease))
            .join()
            .expect("the worker exits");
        assert_eq!(heap_budget.used_bytes(), 0);
        assert_eq!(resource_budget.used(), 1);
        reservation.cancel_destroyed();
        assert_eq!(heap_budget.used_bytes(), 0);
        assert_eq!(resource_budget.used(), 0);
    }
}
