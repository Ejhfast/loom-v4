//! The bytecode virtual machine.
//!
//! One world owns every machine record: the root program plus the
//! nested machines that guest code creates through `sys.vm`. One
//! driver loop executes verified code over an explicit activation
//! stack, so guest call depth and nested VM depth never grow the Rust
//! stack. `run`, `step`, and `drive` are stop modes of that loop.
//!
//! `lm-vm` has no filesystem, clock, network, or compiler dependency.
//! Host operations cross one plain-data completion interface defined
//! in `host`.

mod executor;
mod host;
mod machine;
mod resource;
mod schedule;
pub mod snapshot;
mod typecheck;
mod world;

pub use executor::{execute, execute_turn, recall, ExecutionLease, ExecutionReport, ExecutionTurn};
pub use host::{
    CoreCtor, Host, HostArg, HostChildEnv, HostChildInput, HostChildOutput, HostCompileDefinition,
    HostCompileEnv, HostCompileModule, HostCompileOptions, HostCompileSlot, HostCompletion,
    HostExecSpec, HostIpAddress, HostOpenOptions, HostParseStatus, HostRenameMode, HostResource,
    HostSeekFrom, HostShutdown, HostSignalKind, HostSocketAddress, HostStart, HostStdStream,
    HostSyntaxDiagnostic, HostTcpKind, HostTcpResource, HostValue, HostWaitCancel, HostWake,
    NullHost, RecordingHost,
};
pub use machine::{
    Block, FaultRec, FunctionVersionId, MachineExecutionMetrics, MachineState, Mailbox, Ownership,
    VmId, VmState,
};
pub use resource::{ResourceKind, ResourceRecord, ResourceRegistry, ResourceState};
pub use schedule::{
    CompletionKey, ScheduleEvents, SliceExit, TaskKey, TaskStatus, WaitSetKey, WaitSourceKey,
    WakeKey,
};
pub use world::{
    MailboxMetrics, ParallelContinuation, ParallelDispatch, ParallelDrive, ParallelError,
    ParallelJob, ParallelParked, ParallelRequirement, ParallelReturned, ParallelStep, ParallelWait,
    RootEvent, StopMode, TraceBlock, TraceEvent, World, WorldMetrics,
};

/// The fault codes are manifest content, and the heap and the graph
/// engine name them too. They live in `lm-abi`.
pub use lm_abi::{FaultCode, SnapshotClass};
pub use lm_bytecode::closed::TypeEnvMetrics;
/// The heap, the native shapes, and the graph engine are separate
/// crates. `lm-vm` re-exports the parts its callers already name.
pub use lm_graph::{GraphCost, GraphLimits};
pub use lm_heap::{
    dump_shapes, BoundaryPolicy, Heap, HeapStats, Object, ShapeDesc, SharedBytes, SharedText,
};

use lm_bytecode::{CodeTable, CodeTables};
use lm_link::DispatchRow;
pub use lm_link::{CodeArena, CodeNamespace, NamespaceId};
use lm_value::Value;
pub use lm_verify::VerifyError;

/// A terminal execution result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Done(Value),
    Fault(FaultCode),
}

/// Resource limits and the fuel budget for one machine.
#[derive(Debug, Clone, Copy)]
pub struct VmConfig {
    /// Instruction budget. Each instruction costs one unit.
    ///
    /// A program that serves forever is an ordinary program
    /// (specification 7.2 declares `serve(): Never`), so a root
    /// program takes no instruction cap it did not ask for. A caller
    /// that runs code it does not trust states a bound.
    ///
    /// The default is the largest value, not the absence of a bound.
    /// One machine retires about 445 million instructions each second
    /// on the reference implementation, so the default lasts about
    /// 1300 years of continuous execution. This is a lifetime budget
    /// counting down; `Machine::exec_for_quantum` is the separate
    /// per-call bound, and it is the mechanism a caller must use to
    /// limit work it does not trust.
    pub fuel: u64,
    /// The largest frame-stack depth.
    pub max_frames: u32,
    /// The largest total operand-arena and local-arena size, in values.
    pub max_stack_values: u32,
    /// The hard heap cap in logical bytes.
    pub heap_bytes: usize,
    /// The object, edge, byte, and work limits of every graph mode.
    pub graph: GraphLimits,
    /// The largest number of child machines this machine may create.
    /// A parent reserves a child from its own budget.
    pub max_children: u32,
    /// The largest number of live host resources this machine may
    /// register at one time.
    pub max_resources: u32,
    /// The largest number of accepted messages one proc mailbox may
    /// hold. A send past the bound blocks the sender.
    pub mailbox_limit: u32,
    /// The largest snapshot container this machine may write, in
    /// bytes. A capture past the bound returns
    /// `SnapshotLimitExceeded` (specification 17.4).
    pub snapshot_bytes: usize,
    /// The largest number of closed type nodes one world may hold.
    ///
    /// The language permits polymorphic recursion, so a program can
    /// ask for closed types without bound. A call past the cap takes
    /// `BoundaryLimit`. The world reads the value from the config of
    /// its root machine, because one world holds one table.
    pub max_closed_types: u32,
    /// The largest number of type environment nodes one world may
    /// hold. The cap works exactly as the one above.
    pub max_type_envs: u32,
}

impl Default for VmConfig {
    fn default() -> VmConfig {
        VmConfig {
            fuel: u64::MAX,
            max_frames: 65_536,
            max_stack_values: 4_194_304,
            heap_bytes: 64 << 20,
            graph: GraphLimits::default(),
            max_children: 262_144,
            max_resources: 1_024,
            mailbox_limit: 64,
            snapshot_bytes: 64 << 20,
            max_closed_types: lm_bytecode::closed::DEFAULT_MAX_CLOSED_TYPES,
            max_type_envs: lm_bytecode::closed::DEFAULT_MAX_TYPE_ENVS,
        }
    }
}

/// Shared structural and resource limits for one machine world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldLimits {
    /// The largest machine record table.
    pub max_machines: u32,
    /// The largest live VM image record count.
    pub max_vm_images: u32,
    /// The largest live wait table of one machine.
    pub max_waits: u32,
    /// The largest live host resource count.
    pub max_resources: usize,
    /// The instruction budget shared by all machines.
    ///
    /// The default is the largest value, for the reason
    /// `VmConfig::fuel` states. An embedder that bounds one world
    /// states a value.
    pub fuel: u64,
    /// The largest stored proc trace.
    pub max_trace_events: usize,
    /// The largest retained admitted-image cache, in bytes.
    ///
    /// This limit controls eviction. It never rejects an image.
    pub max_cached_image_bytes: usize,
}

impl Default for WorldLimits {
    fn default() -> WorldLimits {
        WorldLimits {
            max_machines: 262_144,
            max_vm_images: 262_144,
            max_waits: 262_144,
            max_resources: 1 << 16,
            fuel: u64::MAX,
            max_trace_events: 1 << 20,
            max_cached_image_bytes: 256 << 20,
        }
    }
}

/// Runtime indexes for one verified `CodeNamespace`.
///
/// The dispatch table maps one class and selector to one function.
#[derive(Debug, Clone)]
pub(crate) struct NamespaceRuntime {
    code: std::sync::Arc<lm_link::CodeNamespace>,
    tables: std::sync::Arc<CodeTables>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
    dispatch: std::sync::Arc<CodeTable<DispatchRow>>,
    /// Functions that verified code can construct as closures.
    closure_bodies: std::sync::Arc<std::sync::OnceLock<Vec<bool>>>,
    core: lm_bytecode::corepin::CoreLayout,
    pub(crate) core_roles: [u32; lm_bytecode::CORE_ROLE_COUNT],
    pub(crate) entry: u32,
    pub(crate) bindings: std::sync::Arc<[lm_bytecode::FuncBinding]>,
    pub(crate) slot_initials: std::sync::Arc<[Option<lm_bytecode::SlotTarget>]>,
    identity: std::sync::Arc<lm_bytecode::identity::ModuleIdentity>,
}

impl std::ops::Deref for NamespaceRuntime {
    type Target = CodeTables;

    fn deref(&self) -> &CodeTables {
        &self.tables
    }
}

impl lm_bytecode::CodeTableView for NamespaceRuntime {
    fn strings(&self) -> lm_bytecode::CodeTableRef<'_, String> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.strings)
    }

    fn types(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::BcType> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.types)
    }

    fn apps(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::TypeApp> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.apps)
    }

    fn classes(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::BcClass> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.classes)
    }

    fn interfaces(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::BcInterface> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.interfaces)
    }

    fn conformances(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::BcConformance> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.conformances)
    }

    fn slots(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::SlotSpec> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.slots)
    }

    fn funcs(&self) -> lm_bytecode::CodeTableRef<'_, lm_bytecode::Func> {
        lm_bytecode::CodeTableRef::Chunks(&self.tables.funcs)
    }

    fn core_role(&self, index: usize) -> Option<u32> {
        self.core_roles
            .get(index)
            .copied()
            .filter(|class| *class != lm_bytecode::NO_ROLE)
    }
}

impl NamespaceRuntime {
    pub(crate) fn code_namespace(&self) -> &std::sync::Arc<lm_link::CodeNamespace> {
        &self.code
    }

    /// Return the immutable ABI bundle used to verify this module.
    pub fn bundle(&self) -> &std::sync::Arc<lm_abi::AbiBundle> {
        &self.bundle
    }

    /// The verified semantic identity of this module.
    pub fn identity(&self) -> Result<&lm_bytecode::identity::ModuleIdentity, FaultCode> {
        Ok(&self.identity)
    }

    pub(crate) fn dispatch_store(&self) -> std::sync::Arc<CodeTable<DispatchRow>> {
        self.dispatch.clone()
    }

    /// True when verified code can construct one function as a closure.
    pub(crate) fn is_closure_body(&self, function: u32) -> bool {
        self.closure_bodies
            .get_or_init(|| {
                let mut bodies = vec![false; self.tables.funcs.len()];
                for body in &self.tables.funcs {
                    for instruction in body.blocks.iter().flatten() {
                        if let lm_bytecode::Instr::MakeClosure { func, .. } = instruction {
                            bodies[*func as usize] = true;
                        }
                    }
                }
                bodies
            })
            .get(function as usize)
            .copied()
            .unwrap_or(false)
    }

    /// The core layout the artifact declares and the verifier proved.
    pub fn core_layout(&self) -> lm_bytecode::corepin::CoreLayout {
        self.core
    }

    /// The total dispatch-table cell count, for the memory gates.
    pub fn dispatch_cells(&self) -> usize {
        self.dispatch.iter().map(DispatchRow::cell_count).sum()
    }

    /// The number of sparse class-interface default witnesses.
    pub fn interface_witness_entries(&self) -> usize {
        self.dispatch.iter().map(DispatchRow::witness_count).sum()
    }
}

/// Build runtime indexes for one published namespace.
fn prepare_namespace(code: std::sync::Arc<lm_link::CodeNamespace>) -> NamespaceRuntime {
    let tables = code.table_store();
    let bundle = code.bundle().clone();
    let core_roles = *code.core_roles();
    let core = *code.core_layout();
    let dispatch = code.dispatch_store();
    NamespaceRuntime {
        code: code.clone(),
        tables,
        bundle,
        dispatch,
        closure_bodies: code.closure_body_store(),
        core,
        core_roles,
        entry: code.entry(),
        bindings: code.binding_store(),
        slot_initials: code.slot_initial_store(),
        identity: code.identity_store(),
    }
}

#[cfg(test)]
fn unit_from_module_for_test(
    module: lm_bytecode::Module,
) -> Result<std::sync::Arc<NamespaceRuntime>, String> {
    let unit = lm_bytecode::artifact::LinkUnit::from_module(
        lm_bytecode::artifact::CORE_MODULE_PATH,
        module,
        Vec::new(),
    )
    .map_err(|error| error.to_string())?;
    let artifact = lm_bytecode::artifact::Artifact::new(unit, Vec::new())
        .map_err(|error| error.to_string())?;
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact, None)
        .map_err(|error| error.to_string())?;
    let code = arena
        .namespace(namespace)
        .cloned()
        .ok_or_else(|| "the test namespace is missing".to_string())?;
    Ok(std::sync::Arc::new(prepare_namespace(code)))
}

#[cfg(test)]
fn arena_from_test_unit(
    code: &NamespaceRuntime,
) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .replay_namespace(code.code_namespace())
        .map_err(|error| error.to_string())?;
    Ok((arena, namespace))
}

/// A single-machine view over one world with a null host and no
/// grants. Pure programs and the pre-effect test suites use it.
pub struct Vm {
    world: World,
}

impl Vm {
    pub fn new(arena: lm_link::CodeArena, namespace: lm_link::NamespaceId, config: VmConfig) -> Vm {
        Vm {
            world: World::new(arena, namespace, config, Box::new(NullHost)),
        }
    }

    /// Read access to the root heap, for inspection and tests.
    pub fn heap(&self) -> &Heap {
        self.world.heap_of(0)
    }

    /// Return the sparse dispatch-table cell count.
    pub fn dispatch_cells(&self) -> usize {
        self.world.root_code().dispatch_cells()
    }

    /// Return the sparse interface witness count.
    pub fn interface_witness_entries(&self) -> usize {
        self.world.root_code().interface_witness_entries()
    }

    /// Return the verified core-role layout of the root namespace.
    pub fn core_layout(&self) -> lm_bytecode::corepin::CoreLayout {
        self.world.root_core()
    }

    /// Run the entry function to a terminal result.
    pub fn run(&mut self) -> Outcome {
        self.world.run_root()
    }

    /// Render a terminal outcome as stable text, for example
    /// `Done(3628800)` or `Fault(DivideByZero)`.
    pub fn show_outcome(&self, outcome: &Outcome) -> String {
        self.world.show_outcome(outcome)
    }

    /// Render one value in a stable readable form.
    pub fn show_value(&self, value: Value) -> String {
        self.world.show_value(value)
    }

    /// Render the live machine state.
    pub fn dump_live(&self, outcome: &Outcome) -> String {
        self.world.dump_live(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lm_bytecode::{BcClassKind, BcType, Func, Instr::*, Module};
    use std::sync::Arc;

    fn int_module(blocks: Vec<Vec<lm_bytecode::Instr>>) -> Arc<NamespaceRuntime> {
        unit_from_module_for_test(Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
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
                blocks,
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        })
        .unwrap()
    }

    fn test_vm(code: &NamespaceRuntime, config: VmConfig) -> Vm {
        let (arena, namespace) = arena_from_test_unit(code).expect("the test unit publishes");
        Vm::new(arena, namespace, config)
    }

    fn test_world(code: &NamespaceRuntime) -> World {
        let (arena, namespace) = arena_from_test_unit(code).expect("the test unit publishes");
        World::new(arena, namespace, VmConfig::default(), Box::new(NullHost))
    }

    #[test]
    fn runs_addition() {
        let loaded = int_module(vec![vec![ConstInt(40), ConstInt(2), Add, Return]]);
        let mut vm = test_vm(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Done(Value::Int(42)));
    }

    #[test]
    fn a_world_owns_its_verified_code_store() {
        let mut world = {
            let loaded = int_module(vec![vec![ConstInt(40), ConstInt(2), Add, Return]]);
            test_world(&loaded)
        };
        assert_eq!(world.run_root(), Outcome::Done(Value::Int(42)));
    }

    #[test]
    fn overflow_faults() {
        let loaded = int_module(vec![vec![ConstInt(i64::MAX), ConstInt(1), Add, Return]]);
        let mut vm = test_vm(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::IntegerOverflow));
    }

    #[test]
    fn divide_by_zero_faults() {
        let loaded = int_module(vec![vec![ConstInt(1), ConstInt(0), Div, Return]]);
        let mut vm = test_vm(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::DivideByZero));
    }

    #[test]
    fn division_truncates_toward_zero_and_rem_has_dividend_sign() {
        for (a, b, div, rem) in [
            (7, 2, 3, 1),
            (-7, 2, -3, -1),
            (7, -2, -3, 1),
            (-7, -2, 3, -1),
        ] {
            let loaded = int_module(vec![vec![ConstInt(a), ConstInt(b), Div, Return]]);
            let mut vm = test_vm(&loaded, VmConfig::default());
            assert_eq!(vm.run(), Outcome::Done(Value::Int(div)));
            let loaded = int_module(vec![vec![ConstInt(a), ConstInt(b), Rem, Return]]);
            let mut vm = test_vm(&loaded, VmConfig::default());
            assert_eq!(vm.run(), Outcome::Done(Value::Int(rem)));
        }
    }

    #[test]
    fn fuel_exhaustion_faults() {
        let loaded = int_module(vec![vec![Jump(0)]]);
        let mut vm = test_vm(
            &loaded,
            VmConfig {
                fuel: 1000,
                ..VmConfig::default()
            },
        );
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::OutOfFuel));
        let fault = vm.world.root_fault().expect("the root fault exists");
        assert_eq!(fault.trace.len(), 1);
        assert_eq!(fault.trace[0].function, 0);
        assert_eq!(fault.trace[0].block, 0);
        assert_eq!(fault.trace[0].instruction, 0);
    }

    #[test]
    fn load_rejects_invalid_module() {
        let module = Module {
            strings: vec![],
            bytes: vec![],
            types: vec![BcType::Unit, BcType::Bool, BcType::Int, BcType::Str],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![],
            func_bounds: vec![vec![]],
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
                blocks: vec![vec![Jump(9)]],
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        assert!(unit_from_module_for_test(module).is_err());
    }

    #[test]
    fn uninitialized_field_read_faults() {
        // Hand-built bytecode reads a field before any store. The
        // checker prevents this in source programs; the VM faults.
        let module = Module {
            strings: vec![],
            bytes: vec![],
            types: vec![
                BcType::Unit,
                BcType::Bool,
                BcType::Int,
                BcType::Str,
                BcType::Class(0),
            ],
            selectors: vec![],
            apps: vec![],
            interfaces: vec![],
            conformances: vec![],
            class_bounds: vec![vec![]],
            func_bounds: vec![vec![]],
            classes: vec![lm_bytecode::BcClass {
                name: "Point".to_string(),
                key: "Point".to_string(),
                is_final: false,
                is_frozen: false,
                parent: lm_bytecode::NO_PARENT,
                parent_args: Vec::new(),
                type_params: 0,
                kind: BcClassKind::Normal,
                fields: vec![("x".to_string(), 2)],
                field_defaults: vec![false],
                own_start: 0,
                has_init: false,
                methods: vec![],
            }],
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
                blocks: vec![vec![New(0), LoadField(0), Return]],
            }],
            imports: vec![],
            slots: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
            debug: Vec::new(),
        };
        let loaded = unit_from_module_for_test(module).unwrap();
        let mut vm = test_vm(&loaded, VmConfig::default());
        assert_eq!(
            vm.run(),
            Outcome::Fault(crate::FaultCode::UninitializedField)
        );
    }

    #[test]
    fn unreachable_instruction_faults() {
        let loaded = int_module(vec![vec![Unreachable]]);
        let mut vm = test_vm(&loaded, VmConfig::default());
        assert_eq!(vm.run(), Outcome::Fault(crate::FaultCode::UnreachableCode));
    }

    #[test]
    fn shows_outcomes() {
        let loaded = int_module(vec![vec![ConstInt(3), Return]]);
        let mut vm = test_vm(&loaded, VmConfig::default());
        let outcome = vm.run();
        assert_eq!(vm.show_outcome(&outcome), "Done(3)");
        assert_eq!(
            vm.show_outcome(&Outcome::Fault(crate::FaultCode::OutOfFuel)),
            "Fault(OutOfFuel)"
        );
    }
}
