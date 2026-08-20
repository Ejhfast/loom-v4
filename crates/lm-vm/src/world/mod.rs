//! The world: every machine, the one driver loop, policy resolution,
//! the boundary transfer, and the host completion channel.
//!
//! The driver executes nested machines through an explicit activation
//! stack. Machine records are data; the Rust stack never grows with
//! guest call depth or with nested VM depth. `run`, `step`, and
//! `drive` are stop modes of this one loop.

mod capture;
mod machines;
mod procs;
mod query;
mod reply;
mod resources;
mod route;
mod run;
mod sched;
mod show;
mod waits;
use resources::{handle_op_errors, ResourceErrors};
pub(crate) use show::show_trace_event;

use crate::host::{
    CoreCtor, Host, HostArg, HostCompletion, HostOpenOptions, HostSeekFrom, HostStart, HostValue,
};
use crate::machine::{
    Action, Block, ExecOutcome, FaultRec, Machine, MachineState, Mailbox, Ownership, Pending,
    PolicyCursor, RoutedRequest, Terminal, VmId, WaitEntry, WaitSource, MAX_LIVE_WAITS,
};
use crate::schedule::{
    ActiveProcs, CompletionKey, ScheduleEvents, SliceExit, TaskKey, TaskStatus, WaitSetKey,
    WaitSourceKey, WakeKey,
};
use crate::{FaultCode, LoadedModule, Outcome, VmConfig, WorldLimits};
use lm_bytecode::closed::{ClosedType, ClosedTypeId};
use lm_bytecode::corepin::CoreLayout;
use lm_bytecode::{BcClassKind, BcType, Module};
use lm_heap::{Heap, HeapBudget, Object, StructuralEpoch};
use lm_value::{ObjRef, TypeEnvId, Value};

/// The fuel budget of one mock handler run.
const MOCK_FUEL: u64 = 1_000_000;

/// How one activation stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    RunToTerminal,
    OneStep,
    DriveToAsk,
}

/// The event family the consumer expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Run,
    Step,
    Drive,
    /// A bounded drive turn. It answers the same `DriveEvent` as
    /// `Drive`, wrapped in `Option`. `None` reports that the turn
    /// spent its instruction bound and the machine can run again.
    DriveFor,
    Mock,
}

/// One activation on the driver stack.
#[derive(Debug, Clone, Copy)]
struct Activation {
    vm: VmId,
    mode: StopMode,
    family: Family,
    /// The machine whose pending perform consumes the exit event.
    /// `None` delivers to the world caller.
    reply_to: Option<VmId>,
    /// True when this activation retired one instruction. `OneStep`
    /// stops when the flag is set and the machine can pause.
    retired: bool,
    /// The guest instructions this activation may still retire.
    /// `None` means no bound. `Vm.DriveFor` sets it, and the bound
    /// covers every activation above this one.
    fuel: Option<u32>,
}

/// Why one scheduler task saved its activation stack.
#[derive(Debug, Clone, Copy)]
enum SuspendReason {
    Yielded,
    Blocked {
        machine: VmId,
        wake: WakeKey,
    },
    Waiting {
        machine: VmId,
        completion: CompletionKey,
    },
    Parked {
        machine: VmId,
        wait: WaitSetKey,
    },
}

/// One saved activation stack and its scheduler condition.
#[derive(Debug)]
struct SuspendedStack {
    activations: Vec<Activation>,
    reason: SuspendReason,
}

/// The machines held behind one restored execution gate.
#[derive(Debug)]
pub(crate) struct GateGroup {
    id: u32,
    members: Vec<VmId>,
}

/// Why one activation exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitKind {
    Terminal,
    Ran,
    Waiting,
    /// A bounded drive turn spent its instructions. The machine can
    /// run again, so the holder receives no event.
    Bounded,
}

/// How one policy dispatch handles a nested VM execution operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchMode {
    /// Continue nested execution in the current driver call.
    Continue,
    /// Record nested execution for the next control call.
    DeferNested,
}

/// One validated destination for a single operation reply.
///
/// The value lives on the Rust stack. It grants no authority beyond
/// the request and route that `reply_sink` already validated.
#[derive(Debug, Clone, Copy)]
struct ReplySink {
    surface: VmId,
    target: VmId,
    ordinal: u64,
    op: u32,
    cursor: PolicyCursor,
}

/// One world-caller event.
#[derive(Debug)]
pub enum RootEvent {
    Ran,
    Waiting,
    Asked(u64),
    /// The driven machine blocked on another machine of this world.
    /// The activation stack is stored; the scheduler resumes it.
    Blocked,
    Done(Value),
    Fault(FaultRec),
}

/// The verified semantic identity of the loaded code, for the
/// canonical digest.
///
/// A closure holds a numeric function slot and an instance holds a
/// numeric class slot. Both slots belong to this linked program only,
/// so the digest encoder reads the definition hash instead.
struct ModuleCodes<'m> {
    identity: &'m lm_bytecode::identity::ModuleIdentity,
    module: &'m Module,
    envs: &'m mut lm_bytecode::closed::TypeEnvs,
    core: CoreLayout,
}

/// The aggregate ledgers of one root VM and its spawned procs.
struct WorldBudget {
    limits: WorldLimits,
    heap: HeapBudget,
    resources: crate::resource::ResourceBudget,
    fuel: u64,
}

/// One successful restore reply held before restore commit.
struct PreparedRestoreReply {
    value: Value,
    handle: ObjRef,
    reply: ObjRef,
}

#[derive(Debug, Clone, Copy)]
enum ResourceBacking {
    Host(u64),
    Driver(VmId),
}

#[derive(Debug, Clone, Copy)]
struct BoundResource {
    owner: VmId,
    kind: crate::ResourceKind,
    backing: ResourceBacking,
}

#[derive(Debug, Clone, Copy)]
enum ShowExpected {
    Module { ty: u32, env: TypeEnvId },
    Closed(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShowOption {
    Family,
    Some,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WaitLeaf {
    Receive,
    Drive { target: VmId },
}

#[derive(Debug)]
struct WaitLeafPath {
    leaf: WaitLeaf,
    /// False selects `Choice.First`. True selects `Choice.Second`.
    path: Vec<bool>,
}

impl WorldBudget {
    fn new(mut limits: WorldLimits) -> WorldBudget {
        limits.max_machines = limits.max_machines.max(1);
        WorldBudget {
            heap: HeapBudget::new(limits.max_heap_bytes, limits.max_heap_objects),
            resources: crate::resource::ResourceBudget::new(limits.max_resources),
            fuel: limits.fuel,
            limits,
        }
    }
}

impl lm_graph::CodeIdentity for ModuleCodes<'_> {
    fn func_hash(&self, func: u32) -> Result<[u8; 32], FaultCode> {
        self.identity
            .func_hashes
            .get(func as usize)
            .copied()
            .ok_or(FaultCode::BoundaryViolation)
    }

    fn class_hash(&self, class: u32) -> Result<[u8; 32], FaultCode> {
        self.identity
            .class_hashes
            .get(class as usize)
            .copied()
            .ok_or(FaultCode::BoundaryViolation)
    }

    fn type_hash(&self, ty: u32) -> Result<[u8; 32], FaultCode> {
        self.envs
            .cached_digest(ty)
            .ok_or(FaultCode::BoundaryViolation)
    }

    fn option_shape(&mut self, ty: u32) -> Result<Option<lm_graph::DigestOption>, FaultCode> {
        let Some(ClosedType::Inst(class, args)) = self.envs.ty(ty).cloned() else {
            return Ok(None);
        };
        if args.len() != 1 {
            return Ok(None);
        }
        let option = self.core.option.ok_or(FaultCode::BoundaryViolation)?;
        let some = self.core.option_some.ok_or(FaultCode::BoundaryViolation)?;
        let none = self.core.option_none.ok_or(FaultCode::BoundaryViolation)?;
        let case = if class == option {
            lm_graph::DigestOptionCase::Family
        } else if class == some {
            lm_graph::DigestOptionCase::Some
        } else if class == none {
            lm_graph::DigestOptionCase::None
        } else {
            return Ok(None);
        };
        let family = if class == option {
            ty
        } else {
            self.envs
                .intern(ClosedType::Inst(option, vec![args[0]]))
                .map_err(|_| FaultCode::BoundaryLimit)?
        };
        self.envs
            .digest(self.module, &self.identity.class_hashes, family);
        Ok(Some(lm_graph::DigestOption {
            case,
            family,
            payload: args[0],
        }))
    }

    fn child_types(
        &mut self,
        object: &Object,
        expected: Option<u32>,
    ) -> Result<Vec<Option<u32>>, FaultCode> {
        let typed = |types: Vec<u32>| types.into_iter().map(Some).collect();
        match object {
            Object::List { items, .. } => {
                let Some(ClosedType::List(element)) =
                    expected.and_then(|ty| self.envs.ty(ty)).cloned()
                else {
                    return Err(FaultCode::BoundaryViolation);
                };
                Ok(vec![Some(element); items.len()])
            }
            Object::Map { entries, .. } => {
                let Some(ClosedType::Map(key, value)) =
                    expected.and_then(|ty| self.envs.ty(ty)).cloned()
                else {
                    return Err(FaultCode::BoundaryViolation);
                };
                let mut types = Vec::with_capacity(entries.len().saturating_mul(2));
                for _ in entries {
                    types.push(Some(key));
                    types.push(Some(value));
                }
                Ok(types)
            }
            Object::Tuple { items } => {
                let Some(ClosedType::Tuple(types)) =
                    expected.and_then(|ty| self.envs.ty(ty)).cloned()
                else {
                    return Err(FaultCode::BoundaryViolation);
                };
                if types.len() != items.len() {
                    return Err(FaultCode::BoundaryViolation);
                }
                Ok(typed(types))
            }
            Object::Instance { class, fields, env } => {
                let layout = self
                    .module
                    .classes
                    .get(*class as usize)
                    .ok_or(FaultCode::BoundaryViolation)?;
                if layout.fields.len() != fields.len() {
                    return Err(FaultCode::BoundaryViolation);
                }
                let witness_args = self
                    .envs
                    .env(env.env())
                    .map(|held| held.types.clone())
                    .ok_or(FaultCode::BoundaryViolation)?;
                let args = if witness_args.len() == layout.type_params as usize {
                    witness_args
                } else {
                    let expected = expected.ok_or(FaultCode::BoundaryViolation)?;
                    let (want_class, want_args) = self
                        .envs
                        .as_instance(expected)
                        .ok_or(FaultCode::BoundaryViolation)?;
                    if *class == want_class || layout.type_params as usize == want_args.len() {
                        want_args
                    } else {
                        return Err(FaultCode::BoundaryViolation);
                    }
                };
                let field_env = self
                    .envs
                    .env_of(args, Vec::new())
                    .map_err(|_| FaultCode::BoundaryLimit)?;
                let mut types = Vec::with_capacity(layout.fields.len());
                for (_, field_ty) in &layout.fields {
                    let closed = self
                        .envs
                        .close(self.module, *field_ty, field_env)
                        .map_err(|_| FaultCode::BoundaryLimit)?;
                    types.push(closed);
                }
                Ok(typed(types))
            }
            Object::Closure {
                func,
                captures,
                env,
            } => {
                let body = self
                    .module
                    .funcs
                    .get(*func as usize)
                    .ok_or(FaultCode::BoundaryViolation)?;
                if body.captures.len() != captures.len() {
                    return Err(FaultCode::BoundaryViolation);
                }
                let mut types = Vec::with_capacity(body.captures.len());
                for capture in &body.captures {
                    let closed = self
                        .envs
                        .close(self.module, *capture, env.env())
                        .map_err(|_| FaultCode::BoundaryLimit)?;
                    types.push(closed);
                }
                Ok(typed(types))
            }
            _ => Ok(Vec::new()),
        }
    }
}

/// The world: the loaded code plus every machine.
pub struct World {
    loaded: LoadedModule,
    pub(crate) module: std::sync::Arc<Module>,
    dispatch: std::sync::Arc<[crate::DispatchRow]>,
    core: CoreLayout,
    pub(crate) machines: Vec<Machine>,
    /// Retired mock-handler slots, ready for reuse.
    ///
    /// A mock machine is ephemeral: no guest value names it, it takes
    /// no child, and it cannot reach an asked state. One mocked
    /// perform therefore leaves nothing behind, and the next mock
    /// takes the same slot. Without the list, a loop of mocked
    /// performs grows the machine table without any bound.
    mock_free: Vec<VmId>,
    /// Reclaimed machine slots, ready for the next child record.
    ///
    /// `collect_machines` fills this list. A slot here holds an empty
    /// record whose generation already moved past the freed machine.
    vm_free: Vec<VmId>,
    /// Suspended activation stacks, keyed by the machine the stack
    /// started from.
    ///
    /// A machine that blocks on another machine of this world stops
    /// its whole activation stack. The scheduler runs other machines
    /// and resumes the stored stack when the block clears. The record
    /// holds machine identifiers and stop modes only, never a guest
    /// heap reference.
    suspended: std::collections::BTreeMap<VmId, SuspendedStack>,
    /// Scheduler-owned procs that have not reached a terminal state.
    scheduler_procs: ActiveProcs,
    /// Batched task and wake changes for `lm-proc`.
    schedule_events: ScheduleEvents,
    /// Ready host replies that another task does not await.
    host_completions: std::collections::BTreeMap<CompletionKey, HostCompletion>,
    /// Active restored execution gates and their exact members.
    gate_groups: Vec<GateGroup>,
    /// The closed type table and the type environment table of this
    /// world (`docs/specs/sidecar/snapshot-image-admission.md` section 5.6).
    ///
    /// A frame, a closure, an instance, and a machine store one index
    /// into it. The table belongs to one world, so an untrusted
    /// restore never grows shared module state.
    pub(crate) envs: lm_bytecode::closed::TypeEnvs,
    host: Box<dyn Host>,
    /// Open external resources, keyed by unforgeable world identifiers.
    bound_resources: std::collections::BTreeMap<u64, BoundResource>,
    /// The next resource identifier. Zero marks a closed handle.
    next_resource: u64,
    config: VmConfig,
    /// Aggregate limits and current resource charges.
    budget: WorldBudget,
    /// True when each machine heap charges the aggregate ledger.
    ///
    /// One machine needs only its local counters. The world attaches
    /// the shared ledger before it creates another machine.
    heap_shared: bool,
    /// The proc trace, when tracing is on.
    trace: Option<Vec<TraceEvent>>,
    /// The monotone mailbox cut marker of this world.
    cut: u64,
    /// The monotone world-gate marker of this world.
    ///
    /// A restore puts every machine it builds behind one gate. The
    /// first `run`, `step`, or `drive` of the restored root opens that
    /// gate for the whole restored world (specification 17.5).
    gate: u32,
    /// True after one restore committed a machine into this world.
    ///
    /// The boundary check of `docs/specs/sidecar/snapshot-image-admission.md`
    /// section 5.2 proves that a value carries the type its receiving
    /// code expects. Ordinary execution builds every value through
    /// verified code, and the verifier already proved those types, so
    /// the check answers a question that is already settled.
    ///
    /// A restore is the one path that states a value the verifier
    /// never saw. Until a restore commits, therefore, no value of this
    /// world can carry a type its code did not prove, and the check
    /// costs work without adding a rule.
    ///
    /// The flag names the whole world rather than one machine. A
    /// machine-level rule must follow the source of each value, and a
    /// value reaches a boundary from a heap, a mailbox, or a host
    /// reply, so the source needs its own record. The world flag needs
    /// none, and it holds for every program that restores nothing.
    restored_any: bool,
    /// The number of whole-image admissions this world ran.
    ///
    /// The count instruments the rule of specification 17.8: external
    /// bytes are admitted once, and a later restore repeats nothing.
    checks: u64,
    /// The admitted images this execution already holds, newest first.
    ///
    /// A guest holds a snapshot as container bytes. A restore looks
    /// the bytes up by container hash: a hit is an image this process
    /// captured or already admitted, so the restore reads the admitted
    /// state and repeats no check. A miss runs the external loader
    /// once. The table stores `SnapshotImage`, so the type system
    /// records the admission fact; a bare `Image` can never enter it.
    /// A byte budget bounds retained decoded graphs. It never rejects
    /// an image. An evicted image runs admission again at its next
    /// restore.
    /// The insertion order of the trusted cache, newest first. It
    /// decides eviction alone; a lookup reads the index below.
    trusted: std::collections::VecDeque<([u8; 32], crate::snapshot::SnapshotImage, usize)>,
    /// The trusted cache by container hash.
    ///
    /// A restore of an in-process capture reaches this index once per
    /// restored world, so the lookup must not scan the cache.
    trusted_index: std::collections::HashMap<[u8; 32], crate::snapshot::SnapshotImage>,
    /// The canonical byte size charged by the trusted image cache.
    trusted_bytes: usize,
    /// The admitted images this world holds, by slot.
    ///
    /// A guest snapshot value names a slot. A capture stores its
    /// admitted world here and writes no container, so a restore of
    /// that value copies no bytes.
    images: Vec<Option<crate::snapshot::SnapshotImage>>,
    /// Reclaimed image slots.
    image_free: Vec<u32>,
    /// The last image a guest capture produced in this world.
    ///
    /// `lm snapshot save` writes it, so a program states in its own
    /// source which world a checkpoint holds.
    last_image: Option<crate::snapshot::SnapshotImage>,
    /// The reusable buffers of the boundary type check.
    ///
    /// One world runs one check at a time, so one buffer set serves
    /// every boundary. A scalar reply touches none of it.
    check: crate::typecheck::BoundaryScratch,
}

/// One recorded scheduler event. A trace record names machines by
/// identifier and generation, never by a guest reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceEvent {
    Spawn {
        parent: VmId,
        proc: VmId,
        generation: u32,
    },
    Send {
        from: VmId,
        to: VmId,
        accepted: bool,
    },
    Receive {
        proc: VmId,
        closed: bool,
    },
    Close {
        proc: VmId,
        first: bool,
    },
    Block {
        vm: VmId,
        kind: TraceBlock,
        target: VmId,
    },
    Unblock {
        vm: VmId,
    },
    Pause {
        proc: VmId,
    },
    Resume {
        proc: VmId,
    },
    Terminal {
        proc: VmId,
        faulted: bool,
    },
}

/// One compact block kind in a scheduler trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceBlock {
    Receive,
    Send,
    Done,
    Wait,
    Snapshot,
}

/// The mailbox counters of one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxMetrics {
    pub limit: u32,
    pub queued: u32,
    pub accepted: u64,
    pub delivered: u64,
    pub closed: bool,
    pub frozen: bool,
}

/// Copy one value that needs no heap traversal.
#[inline]
fn scalar_copy(value: Value) -> Option<Result<Value, FaultCode>> {
    match value {
        Value::Unit
        | Value::Bool(_)
        | Value::Int(_)
        | Value::Char(_)
        | Value::Op(_)
        | Value::EmptyCase { .. } => Some(Ok(value)),
        Value::Callback(_) | Value::Uninit => Some(Err(FaultCode::BoundaryViolation)),
        Value::Obj(_) => None,
    }
}

/// The message of one failed boundary copy.
///
/// A copy fails for two different reasons. The graph may hold a value
/// that never crosses, or the copy may pass a limit. A limit failure
/// is not a sendability failure, so the two texts differ. The stable
/// fault code does not change.
fn copy_failure(code: FaultCode, what: &str) -> String {
    match code {
        FaultCode::HeapLimit => format!("the {what} copy exceeded the heap limit"),
        FaultCode::BoundaryLimit => format!("the {what} copy exceeded the boundary limit"),
        _ => format!("the {what} is not sendable"),
    }
}

/// The stored argument list of one pending request.
///
/// A restored machine states its own list, so a kernel rule must read
/// a position the list may not hold. The index answers the
/// uninitialized marker there, and every shape test of a kernel rule
/// rejects that marker.
#[derive(Clone, Copy)]
struct Args<'a>(&'a [Value]);

impl std::ops::Index<usize> for Args<'_> {
    type Output = Value;

    fn index(&self, at: usize) -> &Value {
        self.0.get(at).unwrap_or(&Value::Uninit)
    }
}

/// One resolution of a policy walk.
enum Resolution {
    Denied,
    /// The pass-through chain reached a parent machine that is gone.
    /// The request fails closed (specification 18.6).
    DeadParent,
    Mock {
        owner: VmId,
        closure: ObjRef,
    },
    /// The table owner has an active manual driver.
    Driver {
        surface: VmId,
        cursor: PolicyCursor,
    },
    Root,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::NullHost;
    use crate::machine::Pending;
    use crate::{load, VmConfig, WorldLimits};
    use lm_bytecode::{BcType, Func, Instr, Module};

    fn trivial_loaded() -> crate::LoadedModule {
        load(Module {
            strings: vec![],
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
                type_params: 0,
                effect_params: 0,
                params: vec![],
                param_muts: vec![],
                ret: 2,
                row: vec![],
                captures: vec![],
                local_types: vec![],
                blocks: vec![vec![Instr::ConstInt(1), Instr::Return]],
            }],
            imports: vec![],
            core_roles: [lm_bytecode::NO_ROLE; lm_bytecode::CORE_ROLE_COUNT],
            entry: 0,
            exports: vec![],
            bindings: vec![],
        })
        .expect("the trivial module verifies")
    }

    /// Give machine 0 a pending VM-control perform over a handle to
    /// machine `target`.
    fn arm_pending(world: &mut World, op: u32, extra: Vec<Value>, target: VmId) {
        let handle = world.machines[0]
            .alloc(Object::NativeRun { vm: target })
            .expect("the handle allocates");
        let mut args = vec![handle];
        args.extend(extra);
        world.machines[0].vm.pending = Some(Pending {
            op,
            args,
            ordinal: 1,
        });
    }

    #[test]
    fn control_of_an_active_machine_faults_the_caller_only() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.vm.state = MachineState::Ready;
        child.active = 1;
        world.machines.push(child);
        arm_pending(&mut world, lm_abi::OP_VM_RUN, vec![], 1);
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_RUN, DispatchMode::Continue);
        assert_eq!(world.machines[0].vm.state, MachineState::Faulted);
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => assert_eq!(rec.code, FaultCode::InvalidVmState),
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine did not change.
        assert_eq!(world.machines[1].vm.state, MachineState::Ready);
        assert_eq!(world.machines[1].active, 1);
        assert!(stack.is_empty());
    }

    #[test]
    fn a_stale_token_faults_the_caller_and_keeps_the_request() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut child = Machine::empty(VmConfig::default(), Some(0));
        child.vm.state = MachineState::Asked;
        child.vm.pending = Some(Pending {
            op: lm_abi::OP_CLOCK_NOW,
            args: vec![],
            ordinal: 7,
        });
        world.machines.push(child);
        // A call token with a stale ordinal.
        let token = world.machines[0]
            .alloc(Object::NativeCall {
                vm: 1,
                ordinal: 6,
                op: lm_abi::OP_CLOCK_NOW,
            })
            .expect("the token allocates");
        arm_pending(
            &mut world,
            lm_abi::OP_VM_ANSWER,
            vec![token, Value::Int(1)],
            1,
        );
        let mut stack = Vec::new();
        world.kernel_exec(&mut stack, 0, lm_abi::OP_VM_ANSWER, DispatchMode::Continue);
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        // The controlled machine keeps its state and its request.
        assert_eq!(world.machines[1].vm.state, MachineState::Asked);
        let pending = world.machines[1]
            .vm
            .pending
            .as_ref()
            .expect("still pending");
        assert_eq!(pending.ordinal, 7);
    }

    #[test]
    fn a_cross_machine_token_faults_the_caller() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        for _ in 0..2 {
            let mut child = Machine::empty(VmConfig::default(), Some(0));
            child.vm.state = MachineState::Asked;
            child.vm.pending = Some(Pending {
                op: lm_abi::OP_CLOCK_NOW,
                args: vec![],
                ordinal: 1,
            });
            world.machines.push(child);
        }
        // The token belongs to machine 2, the receiver is machine 1.
        let token = world.machines[0]
            .alloc(Object::NativeRequest { vm: 2, ordinal: 1 })
            .expect("the token allocates");
        arm_pending(&mut world, lm_abi::OP_VM_DISPATCH, vec![token], 1);
        let mut stack = Vec::new();
        world.kernel_exec(
            &mut stack,
            0,
            lm_abi::OP_VM_DISPATCH,
            DispatchMode::Continue,
        );
        match &world.machines[0].vm.terminal {
            Some(Terminal::Fault(rec)) => {
                assert_eq!(rec.code, FaultCode::InvalidRequestToken);
            }
            other => panic!("expected a fault, got {other:?}"),
        }
        assert_eq!(world.machines[1].vm.state, MachineState::Asked);
        assert_eq!(world.machines[2].vm.state, MachineState::Asked);
    }

    /// A proc may hold its own handle, so a send can name the sending
    /// machine. The message stays in one heap, and it copies there.
    ///
    /// Without the one-heap copy the sender and the mailbox would
    /// share one mutable graph.
    #[test]
    fn a_self_send_copies_inside_one_heap() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        // A scalar carries no reference.
        assert_eq!(world.boundary_copy(0, 0, Value::Int(7)), Ok(Value::Int(7)));
        // A mutable graph copies, so the message is a second object.
        let mutable = world.machines[0]
            .alloc(Object::List {
                items: vec![Value::Int(1)],
                epoch: Default::default(),
            })
            .expect("the list allocates");
        let moved = world
            .boundary_copy(0, 0, mutable)
            .expect("a mutable message copies");
        assert_ne!(moved, mutable);
        let source = mutable.as_obj().expect("the source is an object");
        let copy = moved.as_obj().expect("the copy is an object");
        assert!(!world.machines[0].vm.heap.is_frozen(copy));
        // A later write through the source misses the copy.
        if let Object::List { items, .. } = world.machines[0].vm.heap.get_mut(source) {
            items.push(Value::Int(2));
        }
        world.machines[0].vm.heap.recharge(source);
        match world.machines[0].vm.heap.get(copy) {
            Object::List { items, .. } => assert_eq!(items, &vec![Value::Int(1)]),
            other => panic!("expected a list, got {other:?}"),
        }
    }

    #[test]
    fn scalar_boundary_copies_need_no_heap_graph() {
        assert_eq!(scalar_copy(Value::Int(7)), Some(Ok(Value::Int(7))));
        assert_eq!(
            scalar_copy(Value::Uninit),
            Some(Err(FaultCode::BoundaryViolation))
        );
        assert_eq!(
            scalar_copy(Value::Obj(ObjRef {
                slot: 3,
                generation: 1,
            })),
            None
        );
    }

    /// The self send applies the shape rule, so it accepts exactly
    /// what a cross-heap copy accepts.
    ///
    /// A machine handle is born frozen and holder local. The frozen
    /// bit alone would let it into a mailbox, and a cross-heap send
    /// of the same value rejects it.
    #[test]
    fn a_self_send_rejects_a_holder_local_value() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        assert!(world.share_heap_budget());
        let mock = world.empty_machine(VmConfig::default(), None, 0);
        world.machines.push(mock);
        let handle = world.machines[0]
            .alloc(Object::NativeVm { vm: 1 })
            .expect("the handle allocates");
        // The frozen bit is set, so only the shape rule can refuse it.
        let r = handle.as_obj().expect("a handle is a heap object");
        assert!(world.machines[0].vm.heap.is_frozen(r));
        assert_eq!(
            world.boundary_copy(0, 0, handle),
            Err(FaultCode::UnsendableValue)
        );
        // The cross-heap path gives the same answer.
        assert_eq!(
            world.boundary_copy(0, 1, handle),
            Err(FaultCode::UnsendableValue)
        );
        // A holder-local object inside a frozen container is refused
        // just as it is at the top of a message.
        let wrapper = world.machines[0]
            .alloc(Object::Tuple {
                items: vec![handle],
            })
            .expect("the tuple allocates");
        assert_eq!(
            world.boundary_copy(0, 0, wrapper),
            Err(FaultCode::UnsendableValue)
        );
        assert_eq!(
            world.boundary_copy(0, 1, wrapper),
            Err(FaultCode::UnsendableValue)
        );
    }

    /// A failed copy reports its cause. A limit failure must not read
    /// like a sendability failure.
    #[test]
    fn a_copy_failure_names_its_cause() {
        assert_eq!(
            copy_failure(FaultCode::UnsendableValue, "message"),
            "the message is not sendable"
        );
        assert_eq!(
            copy_failure(FaultCode::HeapLimit, "message"),
            "the message copy exceeded the heap limit"
        );
        assert_eq!(
            copy_failure(FaultCode::BoundaryLimit, "message"),
            "the message copy exceeded the boundary limit"
        );
    }

    /// A stale generation names a dead proc, never the machine that
    /// took the slot later.
    ///
    /// A proc slot is not reused today, so no guest program reaches
    /// this state. The rule is the defense that keeps it that way, so
    /// it is tested at the record level.
    #[test]
    fn a_stale_generation_names_a_dead_proc() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        let mut proc = Machine::empty_at(VmConfig::default(), Some(0), 3);
        proc.vm.state = MachineState::Ready;
        proc.owner = crate::machine::Ownership::Scheduler;
        world.machines.push(proc);
        assert!(world.proc_alive(1, 3));
        assert!(world.proc_running(1, 3));
        // A reference minted before the slot moved on is stale.
        assert!(!world.proc_alive(1, 2));
        assert!(!world.proc_running(1, 2));
        // A reference past the table is stale as well, and the bound
        // check runs before the generation read.
        assert!(!world.proc_alive(9, 0));
    }

    /// A retired mock slot takes a new generation, so a reference to
    /// the retired record never names its replacement.
    #[test]
    fn a_retired_slot_takes_a_new_generation() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        assert!(world.share_heap_budget());
        let mock = world.empty_machine(VmConfig::default(), None, 0);
        world.machines.push(mock);
        assert_eq!(world.generation_of(1), 0);
        world.retire_mock(1);
        assert_eq!(world.generation_of(1), 1);
        assert!(!world.proc_alive(1, 0));
        assert!(world.proc_alive(1, 1));
    }

    /// A reclaimed slot takes a new generation, so a key minted for
    /// the freed record never names the machine that reuses the slot.
    ///
    /// This is the whole safety argument of the reclamation pass. A
    /// completion key and a wake key both compare the generation
    /// beside the machine identifier, so a slot that came back at its
    /// old generation would deliver a reply to the wrong machine.
    /// `a_retired_slot_takes_a_new_generation` states the same rule
    /// for a retired mock.
    #[test]
    fn a_reclaimed_slot_takes_a_new_generation() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        assert!(world.share_heap_budget());
        let child = world.install_child(VmConfig::default(), 0);
        // An empty record is one the pass always keeps, because it
        // cannot tell a new machine from an abandoned one. This one
        // ran and finished, and no value names it.
        world.machines[child as usize].vm.state = MachineState::Done;
        assert_eq!(world.generation_of(child), 0);
        assert!(world.proc_alive(child, 0));

        assert_eq!(world.collect_machines(), 1, "the dead record frees");
        assert_eq!(world.generation_of(child), 1);
        assert!(
            !world.proc_alive(child, 0),
            "the stale key names a dead machine"
        );

        // The next child takes the freed slot back. The key minted
        // before the pass must not name it.
        let reused = world.install_child(VmConfig::default(), 0);
        assert_eq!(reused, child, "the pass returned the slot");
        assert_eq!(world.generation_of(reused), 1);
        assert!(!world.proc_alive(child, 0));
        assert!(world.proc_alive(reused, 1));
    }

    /// A mock start that fails returns its machine slot at once.
    ///
    /// The slot is taken before the handler and the arguments cross,
    /// so both failure paths must retire it. Without that, one failed
    /// mock start per perform grows the machine table without bound.
    #[test]
    fn a_failed_mock_start_returns_its_machine_slot() {
        let loaded = trivial_loaded();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));
        // A machine handle is holder-local, so it never crosses a
        // boundary. It stands in for any unsendable handler here.
        let handle = world.machines[0]
            .alloc(Object::NativeVm { vm: 0 })
            .expect("the handle allocates");
        let unsendable = handle.as_obj().expect("a handle is a heap object");
        let before = world.machine_count();
        let mut stack = Vec::new();
        for ordinal in 1..4 {
            world.machines[0].vm.terminal = None;
            world.machines[0].vm.state = MachineState::Ready;
            world.machines[0].vm.pending = Some(Pending {
                op: lm_abi::OP_CLOCK_NOW,
                args: vec![],
                ordinal,
            });
            world.start_mock(&mut stack, 0, 0, unsendable);
            match &world.machines[0].vm.terminal {
                Some(Terminal::Fault(rec)) => {
                    assert_eq!(rec.code, FaultCode::UnsendableValue);
                }
                other => panic!("expected a fault, got {other:?}"),
            }
            // One slot exists and waits on the free list. A later
            // failed start reuses it instead of adding another.
            assert_eq!(world.machine_count(), before + 1, "start {ordinal}");
            assert_eq!(world.mock_free.len(), 1, "start {ordinal}");
            assert!(stack.is_empty(), "start {ordinal}");
        }
    }

    #[test]
    fn the_world_machine_limit_bounds_sibling_growth() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            max_machines: 2,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        assert!(world.new_child(0).is_some());
        assert!(world.new_child(0).is_none());
        assert_eq!(world.machine_count(), 2);
        assert_eq!(world.child_count(0), 1);
    }

    #[test]
    fn two_machine_heaps_share_one_world_limit() {
        let loaded = trivial_loaded();
        let object = Object::Str("one".into());
        let limits = WorldLimits {
            max_heap_bytes: object.cost(),
            max_heap_objects: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        world.machines[0]
            .alloc(object.clone())
            .expect("the first heap charge fits");
        let child = world.new_child(0).expect("the child record fits");
        assert_eq!(
            world.machines[child as usize].alloc(object),
            Err(FaultCode::HeapLimit)
        );
        assert_eq!(world.world_heap_objects(), 1);
    }

    #[test]
    fn a_second_machine_attaches_the_aggregate_heap_ledger() {
        let loaded = trivial_loaded();
        let object = Object::Str("one".into());
        let bytes = object.cost();
        let mut world = World::new(&loaded, VmConfig::default(), Box::new(NullHost));

        assert!(!world.heap_shared);
        world.machines[0]
            .alloc(object.clone())
            .expect("the root allocation fits");
        assert_eq!(world.world_heap_bytes(), bytes);
        assert_eq!(world.world_heap_objects(), 1);

        let child = world.new_child(0).expect("the child record fits");
        assert!(world.heap_shared);
        assert_eq!(world.world_heap_bytes(), bytes);
        assert_eq!(world.world_heap_objects(), 1);

        world.machines[child as usize]
            .alloc(object)
            .expect("the child allocation fits");
        assert_eq!(world.world_heap_bytes(), bytes * 2);
        assert_eq!(world.world_heap_objects(), 2);
    }

    #[test]
    fn all_machines_share_one_instruction_budget() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            fuel: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        assert_eq!(world.run_root(), Outcome::Fault(FaultCode::OutOfFuel));
        assert_eq!(world.world_fuel(), 0);
    }

    #[test]
    fn the_proc_trace_stops_at_its_world_limit() {
        let loaded = trivial_loaded();
        let limits = WorldLimits {
            max_trace_events: 1,
            ..WorldLimits::default()
        };
        let mut world =
            World::new_with_limits(&loaded, VmConfig::default(), limits, Box::new(NullHost));
        world.trace_procs();
        world.record(TraceEvent::Pause { proc: 0 });
        world.record(TraceEvent::Resume { proc: 0 });
        assert_eq!(world.trace(), &[TraceEvent::Pause { proc: 0 }]);
    }

    #[test]
    fn a_failed_restore_reply_leaves_no_handle_object() {
        let loaded = trivial_loaded();
        let handle_bytes = Object::NativeRun { vm: 1 }.cost();
        let config = VmConfig {
            heap_bytes: handle_bytes,
            ..VmConfig::default()
        };
        let mut world = World::new(&loaded, config, Box::new(NullHost));
        world.core.result_ok = Some(0);
        assert!(matches!(
            world.prepare_restore_reply(0, 1),
            Err(FaultCode::HeapLimit)
        ));
        assert_eq!(world.machines[0].vm.heap.live_count(), 0);
        assert_eq!(world.world_heap_bytes(), 0);
    }
}
