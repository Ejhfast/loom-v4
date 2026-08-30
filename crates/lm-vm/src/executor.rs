//! The machine executor boundary.
//!
//! The executor owns one machine and reads immutable code.
//! It returns before any operation needs world state.

use crate::engine::{Engine, EngineMode};
use crate::machine::{ExecError, ExecOutcome, ImageSlotTarget, Machine, VmId};
use crate::resource::ResourceBudgetReservation;
use crate::{DispatchRow, FaultCode, NamespaceRuntime};
use lm_bytecode::closed::TypeEnvs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// The exact instruction budget shared by one machine world.
#[derive(Debug)]
pub(crate) struct ExecutionFuel {
    remaining: AtomicU64,
}

impl ExecutionFuel {
    pub(crate) fn new(remaining: u64) -> ExecutionFuel {
        ExecutionFuel {
            remaining: AtomicU64::new(remaining),
        }
    }

    pub(crate) fn remaining(&self) -> u64 {
        self.remaining.load(Ordering::Relaxed)
    }

    fn claim(&self, limit: u32) -> u32 {
        let mut current = self.remaining();
        loop {
            let claimed = current.min(u64::from(limit)) as u32;
            if claimed == 0 {
                return 0;
            }
            match self.remaining.compare_exchange_weak(
                current,
                current - u64::from(claimed),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return claimed,
                Err(actual) => current = actual,
            }
        }
    }

    fn release(&self, count: u32) {
        if count == 0 {
            return;
        }
        let result = self
            .remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(u64::from(count))
            });
        assert!(result.is_ok(), "the world fuel counter cannot overflow");
    }

    pub(crate) fn charge(&self, count: u32) {
        if count == 0 {
            return;
        }
        let result = self
            .remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_sub(u64::from(count))
            });
        assert!(result.is_ok(), "the world fuel counter cannot underflow");
    }

    pub(crate) fn charge_unique(&mut self, count: u32) {
        let remaining = self.remaining.get_mut();
        *remaining = remaining
            .checked_sub(u64::from(count))
            .expect("the world fuel counter cannot underflow");
    }
}

struct ExecutionFuelClaim {
    fuel: Arc<ExecutionFuel>,
    claimed: u32,
    retired: u32,
}

impl ExecutionFuelClaim {
    fn new(fuel: Arc<ExecutionFuel>, limit: u32) -> ExecutionFuelClaim {
        let claimed = fuel.claim(limit);
        ExecutionFuelClaim {
            fuel,
            claimed,
            retired: 0,
        }
    }
}

impl Drop for ExecutionFuelClaim {
    fn drop(&mut self) {
        self.fuel.release(self.claimed.saturating_sub(self.retired));
    }
}

/// One immutable verified execution view.
pub(crate) struct ExecutionCode {
    module: Arc<NamespaceRuntime>,
    dispatch: Arc<lm_bytecode::CodeTable<DispatchRow>>,
    native: crate::jit::NativeCodeState,
}

impl ExecutionCode {
    #[cfg(test)]
    pub(crate) fn new(
        module: Arc<NamespaceRuntime>,
        dispatch: Arc<lm_bytecode::CodeTable<DispatchRow>>,
    ) -> ExecutionCode {
        let native = crate::jit::NativeCodeState::new(&module);
        ExecutionCode {
            module,
            dispatch,
            native,
        }
    }

    pub(crate) fn with_native(
        module: Arc<NamespaceRuntime>,
        dispatch: Arc<lm_bytecode::CodeTable<DispatchRow>>,
        native: crate::jit::NativeCodeState,
    ) -> ExecutionCode {
        ExecutionCode {
            module,
            dispatch,
            native,
        }
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
    fuel: Arc<ExecutionFuel>,
    instruction_limit: u32,
    retired_instructions: u32,
    turns: u32,
    local_continuations: u32,
    local_rotations: u32,
    heap_before: usize,
    objects_before: usize,
    restricted_world: bool,
    engine: Arc<Engine>,
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutionLimits {
    pub(crate) instructions: u32,
    pub(crate) exclusive_world: bool,
    pub(crate) fuel: Arc<ExecutionFuel>,
    pub(crate) engine: Arc<Engine>,
}

/// Coordinator-owned accounting for one execution lease.
///
/// The marker keeps this value outside worker jobs.
#[must_use = "the coordinator must commit or cancel this execution reservation"]
pub(crate) struct ExecutionReservation {
    token: ExecutionToken,
    resources: ResourceBudgetReservation,
    coordinator_only: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ExecutionReservation {
    /// Release all charges after the worker machine was destroyed.
    pub(crate) fn cancel_destroyed(self) {
        self.resources.cancel_destroyed();
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
        let resources = machine.resources.begin_execution_lease(token.lease);
        let heap_before = machine.vm.heap.used_bytes();
        let objects_before = machine.vm.heap.stats().live;
        let reservation = ExecutionReservation {
            token,
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
                fuel: limits.fuel,
                instruction_limit: limits.instructions,
                retired_instructions: 0,
                turns: 0,
                local_continuations: 0,
                local_rotations: 0,
                heap_before,
                objects_before,
                restricted_world: !limits.exclusive_world,
                engine: limits.engine,
            },
            reservation,
        )
    }
}

pub(crate) enum ExecutionStop {
    QuantumExpired,
    Recalled,
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
            ExecutionStop::QuantumExpired | ExecutionStop::Recalled
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
    turns: u32,
    local_continuations: u32,
    local_rotations: u32,
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
            ExecutionStop::QuantumExpired | ExecutionStop::Recalled
        )
    }

    /// True when the pool recalled this lease.
    pub fn stopped_by_recall(&self) -> bool {
        matches!(self.stop, ExecutionStop::Recalled)
    }

    /// The worker-local turns in this lease.
    pub fn turns(&self) -> u32 {
        self.turns
    }

    /// The turns that continued without a queue rotation.
    pub fn local_continuations(&self) -> u32 {
        self.local_continuations
    }

    /// The turns that rotated this lease behind waiting work.
    pub fn local_rotations(&self) -> u32 {
        self.local_rotations
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

impl ExecutionLease {
    /// Record one turn that continued on the same worker.
    pub fn note_local_continuation(&mut self) {
        self.local_continuations = self.local_continuations.saturating_add(1);
    }

    /// Record one turn that moved behind waiting work.
    pub fn note_local_rotation(&mut self) {
        self.local_rotations = self.local_rotations.saturating_add(1);
    }

    fn into_report(mut self, mut stop: ExecutionStop) -> ExecutionReport {
        if !matches!(stop, ExecutionStop::QuantumExpired) && self.machine.has_native_continuation()
        {
            if let Err(code) = self.engine.materialize_native_state(&mut self.machine) {
                stop = ExecutionStop::Fault(code);
            }
        }
        let heap_after = self.machine.vm.heap.used_bytes();
        let objects_after = self.machine.vm.heap.stats().live;
        ExecutionReport {
            token: self.token,
            machine: self.machine,
            code: self.code,
            envs: self.envs,
            stop,
            retired_instructions: self.retired_instructions,
            heap_before: self.heap_before,
            heap_after,
            objects_before: self.objects_before,
            objects_after,
            turns: self.turns,
            local_continuations: self.local_continuations,
            local_rotations: self.local_rotations,
        }
    }
}

/// The result of one worker-local execution turn.
pub enum ExecutionTurn {
    /// Keep the lease inside the worker pool.
    Continue(ExecutionLease),
    /// Return the lease to its world coordinator.
    Report(ExecutionReport),
}

/// Execute one bounded worker-local turn.
pub fn execute_turn(mut lease: ExecutionLease, turn_limit: u32) -> ExecutionTurn {
    let remaining = lease
        .instruction_limit
        .saturating_sub(lease.retired_instructions);
    if remaining == 0 {
        return ExecutionTurn::Report(lease.into_report(ExecutionStop::QuantumExpired));
    }
    let requested = remaining.min(turn_limit.max(1));
    let mut claim = ExecutionFuelClaim::new(Arc::clone(&lease.fuel), requested);
    if claim.claimed == 0 {
        return ExecutionTurn::Report(
            lease.into_report(ExecutionStop::Fault(FaultCode::OutOfFuel)),
        );
    }
    let (outcome, retired) = {
        let ExecutionLease {
            machine,
            code,
            envs,
            slots,
            restricted_world,
            engine,
            ..
        } = &mut lease;
        let slots = slots.as_deref().map(Vec::as_slice);
        let result = run_engine_turn(
            machine,
            claim.claimed,
            EngineTurnContext {
                module: code.module.as_ref(),
                dispatch: code.dispatch.as_ref(),
                native: &code.native,
                envs,
                slots,
                restricted_world: *restricted_world,
                engine,
            },
        );
        claim.retired = result.1;
        result
    };
    lease.retired_instructions = lease.retired_instructions.saturating_add(retired);
    lease.turns = lease.turns.saturating_add(1);
    let stop = match outcome {
        Ok(None) if lease.retired_instructions < lease.instruction_limit => {
            return ExecutionTurn::Continue(lease);
        }
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(ExecError::Fault(code)) => ExecutionStop::Fault(code),
    };
    ExecutionTurn::Report(lease.into_report(stop))
}

/// Return one lease at a pool recall point.
pub fn recall(lease: ExecutionLease) -> ExecutionReport {
    lease.into_report(ExecutionStop::Recalled)
}

/// Execute one lease until a boundary or instruction limit.
pub fn execute(mut lease: ExecutionLease) -> ExecutionReport {
    loop {
        match execute_turn(lease, u32::MAX) {
            ExecutionTurn::Continue(next) => lease = next,
            ExecutionTurn::Report(report) => return report,
        }
    }
}

/// Execute one deterministic slice through borrowed state.
pub(crate) fn execute_inline(
    machine: &mut Machine,
    code: &ExecutionCode,
    envs: &mut TypeEnvs,
    slots: Option<&[ImageSlotTarget]>,
    instruction_limit: u32,
    engine: &Engine,
) -> InlineExecutionReport {
    let (outcome, retired_instructions) = run_engine_turn(
        machine,
        instruction_limit,
        EngineTurnContext {
            module: code.module.as_ref(),
            dispatch: code.dispatch.as_ref(),
            native: &code.native,
            envs,
            slots,
            restricted_world: false,
            engine,
        },
    );
    let stop = match outcome {
        Ok(None) => ExecutionStop::QuantumExpired,
        Ok(Some(outcome)) => ExecutionStop::Boundary(outcome),
        Err(ExecError::Fault(code)) => ExecutionStop::Fault(code),
    };
    InlineExecutionReport {
        stop,
        retired_instructions,
    }
}

/// Run one machine turn through the selected execution engine.
struct EngineTurnContext<'a> {
    module: &'a NamespaceRuntime,
    dispatch: &'a lm_bytecode::CodeTable<DispatchRow>,
    native: &'a crate::jit::NativeCodeState,
    envs: &'a mut TypeEnvs,
    slots: Option<&'a [ImageSlotTarget]>,
    restricted_world: bool,
    engine: &'a Engine,
}

fn run_engine_turn(
    machine: &mut Machine,
    instruction_limit: u32,
    mut context: EngineTurnContext<'_>,
) -> (Result<Option<ExecOutcome>, ExecError>, u32) {
    if instruction_limit == 0 {
        return (Ok(None), 0);
    }
    if context.engine.mode() == EngineMode::Interpreter {
        if let Err(code) = context.engine.materialize_native_state(machine) {
            return (Err(ExecError::Fault(code)), 0);
        }
        return run_interpreter_turn(machine, instruction_limit, &mut context, None);
    }
    let mut native_scratch = crate::jit::NativeScratch::default();
    let mut native_metrics = context.engine.turn_metrics();
    let mut retired_total = 0;
    loop {
        let remaining = instruction_limit - retired_total;
        match context.engine.execute_native(
            machine,
            context.module,
            context.native,
            &mut native_scratch,
            &mut native_metrics,
            remaining,
        ) {
            crate::jit::NativeAttempt::Complete { outcome, retired } => {
                retired_total += retired;
                return (outcome, retired_total);
            }
            crate::jit::NativeAttempt::AdvanceToEntry { instructions } => {
                let advance = instructions.min(remaining).max(1);
                let (outcome, interpreted) =
                    run_interpreter_turn(machine, advance, &mut context, None);
                retired_total += interpreted;
                if matches!(outcome, Ok(Some(ExecOutcome::Continue))) {
                    if retired_total < instruction_limit {
                        continue;
                    }
                    return (Ok(None), retired_total);
                }
                if advance < remaining && matches!(outcome, Ok(None)) {
                    continue;
                }
                return (outcome, retired_total);
            }
            crate::jit::NativeAttempt::Continue { retired } => {
                retired_total += retired;
                let remaining = instruction_limit - retired_total;
                if remaining == 0 {
                    return (Ok(None), retired_total);
                }
                let resume_depth = machine.vm.frames.len();
                let (outcome, interpreted) =
                    run_interpreter_turn(machine, remaining, &mut context, Some(resume_depth));
                retired_total += interpreted;
                if matches!(outcome, Ok(Some(ExecOutcome::Continue))) {
                    if retired_total < instruction_limit {
                        continue;
                    }
                    return (Ok(None), retired_total);
                }
                return (outcome, retired_total);
            }
            crate::jit::NativeAttempt::InterpretOne { retired } => {
                retired_total += retired;
                let remaining = instruction_limit - retired_total;
                if remaining == 0 {
                    return (Ok(None), retired_total);
                }
                let (outcome, interpreted) = run_interpreter_turn(machine, 1, &mut context, None);
                retired_total += interpreted;
                if interpreted == 1
                    && retired_total < instruction_limit
                    && matches!(outcome, Ok(None))
                {
                    continue;
                }
                return (outcome, retired_total);
            }
            crate::jit::NativeAttempt::Reenter { retired } => {
                retired_total += retired;
                if retired_total < instruction_limit {
                    continue;
                }
                return (Ok(None), retired_total);
            }
            crate::jit::NativeAttempt::Fallback => {
                let (outcome, interpreted) =
                    run_interpreter_turn(machine, remaining, &mut context, None);
                retired_total += interpreted;
                if matches!(outcome, Ok(Some(ExecOutcome::Continue))) {
                    if retired_total < instruction_limit {
                        continue;
                    }
                    return (Ok(None), retired_total);
                }
                return (outcome, retired_total);
            }
        }
    }
}

fn run_interpreter_turn(
    machine: &mut Machine,
    instruction_limit: u32,
    context: &mut EngineTurnContext<'_>,
    resume_native_depth: Option<usize>,
) -> (Result<Option<ExecOutcome>, ExecError>, u32) {
    let native = match context.engine.mode() {
        EngineMode::Interpreter => crate::machine::NativeResume::Disabled,
        EngineMode::Auto => crate::machine::NativeResume::Tiered {
            state: context.native,
            resume_depth: resume_native_depth,
            profile: context.engine.jit_profiling(),
        },
        EngineMode::Native => crate::machine::NativeResume::EveryDirectCall,
    };
    if context.restricted_world {
        machine.exec_for_quantum_restricted(
            context.module,
            context.dispatch,
            &mut *context.envs,
            context.slots,
            instruction_limit,
            native,
        )
    } else {
        machine.exec_for_quantum(
            context.module,
            context.dispatch,
            &mut *context.envs,
            context.slots,
            instruction_limit,
            native,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceBudget;
    use crate::VmConfig;
    use lm_bytecode::{BcRow, BcType, Func, Instr, Module};
    use lm_heap::Heap;
    use lm_value::{TypeEnvId, Value};

    fn assert_send<T: Send>() {}

    #[test]
    fn every_execution_lease_field_is_send() {
        assert_send::<ExecutionToken>();
        assert_send::<Machine>();
        assert_send::<Box<Machine>>();
        assert_send::<Arc<NamespaceRuntime>>();
        assert_send::<Arc<lm_bytecode::CodeTable<DispatchRow>>>();
        assert_send::<Arc<ExecutionCode>>();
        assert_send::<TypeEnvs>();
        assert_send::<Box<TypeEnvs>>();
        assert_send::<Arc<Vec<ImageSlotTarget>>>();
        assert_send::<Arc<Engine>>();
        assert_send::<u32>();
        assert_send::<ExecutionLease>();
        assert_send::<ExecutionReport>();
    }

    #[test]
    fn local_heaps_are_send() {
        assert_send::<Heap>();
    }

    #[test]
    fn one_owned_lease_executes_on_another_thread() {
        let module = crate::unit_from_module_for_test(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
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
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(42), Instr::Return]],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        })
        .expect("the worker test unit verifies");
        let config = VmConfig {
            heap_bytes: 1024,
            ..VmConfig::default()
        };
        let mut machine = Box::new(Machine::empty_with_resource_budget(
            config,
            None,
            0,
            ResourceBudget::new(config.max_resources as usize),
        ));
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 1,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Arc::new(Vec::new().into()))),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                exclusive_world: true,
                fuel: Arc::new(ExecutionFuel::new(u64::MAX)),
                engine: Arc::clone(&engine),
            },
        );
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the worker returns one report");
        assert_eq!(report.heap_growth_bytes(), 0);
        assert_eq!(report.heap_released_bytes(), 0);
        assert_eq!(report.heap_growth_objects(), 0);
        assert_eq!(report.heap_released_objects(), 0);
        let (_, machine, _, _, stop, retired) = report.into_parts(reservation);
        assert_eq!(retired, 2);
        assert!(matches!(
            stop,
            ExecutionStop::Boundary(ExecOutcome::Terminal(Value::Int(42)))
        ));
        assert_eq!(engine.metrics().compiled_regions, 1);
        assert_eq!(engine.metrics().native_retired_instructions, 2);
        drop(machine);
    }

    #[test]
    fn a_native_worker_returns_one_materialized_effect_boundary() {
        let module = crate::unit_from_module_for_test(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
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
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![BcRow::Op(lm_abi::OP_CLOCK_NOW)],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![
                    Instr::Perform {
                        op: lm_abi::OP_CLOCK_NOW,
                        argc: 0,
                        reply_ty: 2,
                    },
                    Instr::Return,
                ]],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        })
        .expect("the effect worker unit verifies");
        let config = VmConfig {
            heap_bytes: 1024,
            ..VmConfig::default()
        };
        let mut machine = Box::new(Machine::empty_with_resource_budget(
            config,
            None,
            0,
            ResourceBudget::new(config.max_resources as usize),
        ));
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 3,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Arc::new(Vec::new().into()))),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                exclusive_world: false,
                fuel: Arc::new(ExecutionFuel::new(u64::MAX)),
                engine: Arc::clone(&engine),
            },
        );
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the effect worker returns one report");
        let (_, machine, _, _, stop, retired) = report.into_parts(reservation);
        assert_eq!(retired, 1);
        assert!(matches!(
            stop,
            ExecutionStop::Boundary(ExecOutcome::Perform { op, args })
                if op == lm_abi::OP_CLOCK_NOW && args.is_empty()
        ));
        let frame = machine.vm.frames.last().expect("the effect frame remains");
        assert_eq!((frame.block, frame.ip), (0, 1));
        assert!(machine.vm.operands.is_empty());
        let metrics = engine.metrics();
        assert_eq!(metrics.compiled_effect_sites, 1);
        assert_eq!(metrics.native_effect_exits, 1);
    }

    #[test]
    fn a_native_worker_turn_materializes_before_recall() {
        let module = crate::unit_from_module_for_test(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
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
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![2],
                blocks: vec![
                    vec![Instr::ConstInt(0), Instr::StoreLocal(0), Instr::Jump(1)],
                    vec![
                        Instr::LoadLocal(0),
                        Instr::ConstInt(100),
                        Instr::LtInt,
                        Instr::JumpIfFalse(3),
                        Instr::Jump(2),
                    ],
                    vec![
                        Instr::LoadLocal(0),
                        Instr::ConstInt(1),
                        Instr::Add,
                        Instr::StoreLocal(0),
                        Instr::Jump(1),
                    ],
                    vec![Instr::LoadLocal(0), Instr::Return],
                ],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        })
        .expect("the recall test unit verifies");
        let config = VmConfig {
            heap_bytes: 1024,
            ..VmConfig::default()
        };
        let mut machine = Box::new(Machine::empty_with_resource_budget(
            config,
            None,
            0,
            ResourceBudget::new(config.max_resources as usize),
        ));
        machine.load_frame(&module, 0, vec![], None, TypeEnvId::EMPTY);
        let engine = Arc::new(Engine::new(EngineMode::Native));
        let (lease, reservation) = ExecutionLease::new(
            ExecutionToken {
                world: 7,
                machine: 0,
                generation: 0,
                lease: 2,
            },
            machine,
            Arc::new(ExecutionCode::new(module, Arc::new(Vec::new().into()))),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 64,
                exclusive_world: true,
                fuel: Arc::new(ExecutionFuel::new(u64::MAX)),
                engine: Arc::clone(&engine),
            },
        );
        let turn = std::thread::spawn(move || execute_turn(lease, 8))
            .join()
            .expect("the worker returns one turn");
        let ExecutionTurn::Continue(lease) = turn else {
            panic!("the native lease must remain active");
        };
        let report = recall(lease);
        assert!(report.stopped_by_recall());
        assert_eq!(report.retired_instructions(), 8);
        let (_, machine, _, _, _, retired) = report.into_parts(reservation);
        assert_eq!(retired, 8);
        assert_eq!(machine.vm.locals, vec![Value::Int(0)]);
        let frame = machine.vm.frames.last().expect("the frame remains live");
        assert_eq!((frame.block, frame.ip), (2, 0));
        assert_eq!(engine.metrics().native_retired_instructions, 8);
    }

    #[test]
    fn a_worker_enforces_its_local_heap_limit() {
        let module = crate::unit_from_module_for_test(Module {
            strings: vec!["allocation".to_string()],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
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
                param_names: vec![],
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 3,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstStr(0), Instr::Return]],
            }],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: vec![],
        })
        .expect("the allocation test unit verifies");
        let config = VmConfig {
            heap_bytes: 1,
            ..VmConfig::default()
        };
        let mut machine = Box::new(Machine::empty_with_resource_budget(
            config,
            None,
            0,
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
            Arc::new(ExecutionCode::new(module, Arc::new(Vec::new().into()))),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                exclusive_world: false,
                fuel: Arc::new(ExecutionFuel::new(u64::MAX)),
                engine: Arc::new(Engine::default()),
            },
        );
        let report = std::thread::spawn(move || execute(lease))
            .join()
            .expect("the worker returns one report");
        let (_, machine, _, _, stop, retired) = report.into_parts(reservation);
        assert_eq!(retired, 1);
        assert!(matches!(stop, ExecutionStop::Fault(FaultCode::HeapLimit)));
        drop(machine);
    }

    #[test]
    fn the_coordinator_cancels_a_worker_drop() {
        let module = crate::unit_from_module_for_test(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
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
                param_names: vec![],
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
        })
        .expect("the resource test unit verifies");
        let config = VmConfig {
            heap_bytes: 1024,
            ..VmConfig::default()
        };
        let resource_budget = ResourceBudget::new(4);
        let mut machine = Box::new(Machine::empty_with_resource_budget(
            config,
            None,
            0,
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
            Arc::new(ExecutionCode::new(module, Arc::new(Vec::new().into()))),
            Box::default(),
            None,
            ExecutionLimits {
                instructions: 16,
                exclusive_world: true,
                fuel: Arc::new(ExecutionFuel::new(u64::MAX)),
                engine: Arc::new(Engine::default()),
            },
        );
        assert_eq!(resource_budget.used(), 1);
        std::thread::spawn(move || drop(lease))
            .join()
            .expect("the worker exits");
        assert_eq!(resource_budget.used(), 1);
        reservation.cancel_destroyed();
        assert_eq!(resource_budget.used(), 0);
    }
}
