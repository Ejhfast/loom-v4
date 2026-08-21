//! One machine record: heap, frames, arenas, fuel, policy table,
//! pending perform, and terminal storage.
//!
//! `exec_instr` retires exactly one instruction of this machine. It
//! never runs another machine and never recurses: every operation
//! that reaches outside the machine returns an `ExecOutcome` for the
//! world driver.

use crate::resource::{ResourceBudget, ResourceRegistry};
use crate::{FaultCode, VmConfig};
use lm_bytecode::closed::{ClosedType, ClosedTypeId, TypeEnvFull, TypeEnvs};
use lm_bytecode::{ExtendedInstr, Instr, Module};
use lm_heap::{
    process_lookup_hash, Heap, HeapBudget, MapIndex, NativeByteBuffer, NativeStringBuilder, Object,
    SharedBytes, SharedText, StructuralEpoch,
};
use lm_value::{CallbackRef, ObjRef, TypeEnvId, Value, Witness};

/// The largest typed wait table of one machine.
pub const MAX_LIVE_WAITS: usize = 1_024;

/// The fault one machine takes when the type environment table of its
/// world reaches a cap.
///
/// The language permits polymorphic recursion, so a program can ask
/// for closed types and environments without bound. The cap turns
/// that into one local resource fault.
fn env_fault(_: TypeEnvFull) -> FaultCode {
    FaultCode::BoundaryLimit
}

/// A value did not carry the type this program point expects.
///
/// Verified code never reaches it. A restored machine can: the
/// container states the values, and admission proves their structure
/// alone. Every accessor below tests the tag and raises this code, so
/// the machine stops and the host keeps running.
const BAD_TYPE: FaultCode = FaultCode::TypeMismatch;

/// The stored state of this machine does not match the code it runs.
///
/// A short operand stack, a frame that names no instruction, and a
/// local slot outside the arena all raise it.
const BAD_STATE: FaultCode = FaultCode::MalformedState;

/// The method one class answers for one selector.
///
/// The receiver class comes from a heap object, so a restored machine
/// can name a class whose dispatch row does not answer the selector.
/// The lookup therefore tests the row instead of indexing it.
#[inline]
fn method_of(dispatch: &[crate::DispatchRow], class: u32, selector: u32) -> Result<u32, FaultCode> {
    dispatch
        .get(class as usize)
        .and_then(|row| row.method(selector))
        .ok_or(BAD_TYPE)
}

/// A dense machine identifier inside one world.
pub type VmId = u32;

/// One generation-checked reference to a persistent VM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct VmImageKey {
    pub image: u32,
    pub generation: u32,
}

/// An append-only function slot in one VM code store.
pub type FunctionVersionId = u32;

/// The current target of one late-bound slot in a VM image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageSlotTarget {
    Empty,
    Function(FunctionVersionId),
    Class { class: u32, constructor: u32 },
    Value(Value),
    Process { proc: VmId, generation: u32 },
}

/// The lifecycle state of one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineState {
    Empty,
    Ready,
    Running,
    Asked,
    Waiting,
    /// A pending proc operation waits on another machine of this
    /// world: a message, a queue slot, or a terminal result. The
    /// block is machine state, not a host attachment, because the
    /// machine it waits on is part of the same machine world.
    Blocked,
    Done,
    Faulted,
}

/// Why one machine is blocked on another machine of this world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    /// A proc waits for its own mailbox to hold a message or to close.
    Receive,
    /// A sender waits for one free slot in the target mailbox.
    Send { target: VmId, generation: u32 },
    /// A holder waits for the terminal result of one proc.
    Done { target: VmId, generation: u32 },
    /// A holder waits for a proc world to reach a safe snapshot state.
    Snapshot {
        target: VmId,
        generation: u32,
        remaining: u64,
        retry: bool,
    },
    /// A proc waits for one source in a typed wait tree.
    Wait { token: u64 },
}

/// One prepared typed wait source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitSource {
    /// Read one message from the owning proc mailbox.
    Receive,
    /// Drive one holder-owned child machine.
    Drive { target: VmId },
    /// Select between two existing wait trees.
    Choice { first: u64, second: u64 },
}

/// One wait entry in its owner machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitEntry {
    pub source: WaitSource,
    /// A choice owns linked entries. Their old tokens are stale.
    pub linked: bool,
}

/// Which side owns the execution of one machine (specification 18.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// The guest holder drives the machine through `Vm.*`.
    Holder,
    /// The scheduler drives the machine. Control and inspection
    /// through the dormant `Vm` handle fault until `pause()` returns
    /// ownership.
    Scheduler,
}

/// One bounded FIFO mailbox.
///
/// The accepted messages live in the receiving machine's own heap, so
/// they are ordinary machine state: a snapshot copies them with the
/// heap, and no scheduler record holds a guest reference.
#[derive(Debug)]
pub struct Mailbox {
    /// The largest number of accepted messages the queue may hold.
    pub limit: u32,
    /// The accepted messages, in host acceptance order.
    pub queue: std::collections::VecDeque<Value>,
    /// True when the mailbox accepts no further message. Queued
    /// messages still drain, and `Closed` arrives after the drain.
    pub closed: bool,
    /// True when a barrier froze acceptance at one cut marker.
    pub frozen: bool,
    /// The number of messages the mailbox accepted, for metrics.
    pub accepted: u64,
    /// The number of messages `receive` delivered, for metrics.
    pub delivered: u64,
}

impl Mailbox {
    pub fn new(limit: u32) -> Mailbox {
        Mailbox {
            limit,
            queue: std::collections::VecDeque::new(),
            closed: false,
            frozen: false,
            accepted: 0,
            delivered: 0,
        }
    }

    /// True when the mailbox accepts one more message now.
    pub fn accepts(&self) -> bool {
        !self.closed && !self.frozen && (self.queue.len() as u32) < self.limit
    }

    /// Add one accepted message and update its metric.
    pub(crate) fn push(&mut self, value: Value) {
        self.queue.push_back(value);
        self.accepted = self.accepted.saturating_add(1);
    }

    /// Remove one message and update its metric.
    pub(crate) fn pop(&mut self) -> Option<Value> {
        let value = self.queue.pop_front();
        if value.is_some() {
            self.delivered = self.delivered.saturating_add(1);
        }
        value
    }
}

/// The one pending-perform record of a machine.
#[derive(Debug)]
pub struct Pending {
    pub op: u32,
    /// The popped arguments, in declaration order, in this machine's
    /// heap.
    pub args: Vec<Value>,
    /// The holder-token ordinal of this request.
    pub ordinal: u64,
}

/// Where automatic policy resolution continues after a routed ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyCursor {
    /// Read this machine's table next.
    Table(VmId),
    /// Dispatch the operation to the root host next.
    Root,
}

/// One descendant request exposed through a driven ancestor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutedRequest {
    /// The machine that performed the operation.
    pub target: VmId,
    /// The next automatic policy decision after this driver.
    pub cursor: PolicyCursor,
}

/// A stored machine fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRec {
    pub code: FaultCode,
    pub message: String,
    pub op: Option<u32>,
}

/// The stored terminal result of a machine.
#[derive(Debug)]
pub enum Terminal {
    Done(Value),
    Fault(FaultRec),
}

/// One policy action.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Pass,
    Block,
    /// A mock handler closure stored in the owning machine's heap and
    /// rooted by the table.
    Mock(ObjRef),
}

/// One native policy table: dense exact and group action vectors with
/// an implicit default of block.
#[derive(Debug)]
pub struct PolicyTable {
    pub exact: Vec<Option<Action>>,
    pub group: Vec<Option<Action>>,
}

impl Default for PolicyTable {
    fn default() -> PolicyTable {
        PolicyTable {
            exact: vec![None; lm_abi::OP_COUNT as usize],
            group: vec![None; lm_abi::GROUP_COUNT as usize],
        }
    }
}

impl PolicyTable {
    /// Find the action for one exact operation.
    ///
    /// An exact entry has precedence. A block from any containing
    /// effect set has precedence over set passes. `None` is the
    /// default block.
    ///
    /// The loop reads only the groups that contain the operation.
    /// Most operations name one namespace group, so the loop runs
    /// one time.
    ///
    /// Keep this function out of line. The body is small enough to
    /// inline into the instruction loop, and that growth costs more
    /// on every other opcode than it saves here. Measurement shows
    /// 4 ns on `byte_buffer` and 6 ns on `text_compare`.
    #[inline(never)]
    pub fn lookup(&self, op: u32) -> Option<Action> {
        if let Some(action) = self.exact[op as usize] {
            return Some(action);
        }
        let mut passed = false;
        for group in lm_abi::groups_containing_op(op) {
            match self.group[*group as usize] {
                Some(Action::Block) => return Some(Action::Block),
                Some(Action::Pass) => passed = true,
                Some(Action::Mock(closure)) => return Some(Action::Mock(closure)),
                None => {}
            }
        }
        passed.then_some(Action::Pass)
    }
}

/// One explicit VM frame.
pub struct Frame {
    pub func: FunctionVersionId,
    pub block: u32,
    pub ip: u32,
    pub base_local: u32,
    pub base_operand: u32,
    /// The active closure or callback for `LoadCapture`.
    pub closure: Option<FrameCapture>,
    /// The type environment of this activation.
    ///
    /// The call site supplies it. A monomorphic call copies the empty
    /// environment, so a monomorphic frame does no type work.
    pub env: TypeEnvId,
}

/// One compact capture source for an active frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameCapture {
    Closure(ObjRef),
    Callback(CallbackRef),
}

impl FrameCapture {
    pub fn from_value(value: Value) -> Option<Self> {
        match value {
            Value::Obj(reference) => Some(Self::Closure(reference)),
            Value::Callback(reference) => Some(Self::Callback(reference)),
            _ => None,
        }
    }

    pub fn value(self) -> Value {
        match self {
            Self::Closure(reference) => Value::Obj(reference),
            Self::Callback(reference) => Value::Callback(reference),
        }
    }
}

#[derive(Clone, Copy)]
enum OptionCollectionOp {
    OptionNone(u32),
    OptionPayload(u32),
    ListGet(u32),
    MapGet(u32),
}

#[derive(Clone, Copy)]
enum CollectionIterationOp {
    ListEpoch,
    ListIterLen,
    MapEpoch,
    MapIterLen,
    MapEntry { value: bool },
}

#[derive(Clone, Copy)]
enum CollectionExtensionOp {
    ListCapacity,
    ListSet,
    ListPop(u32),
    ListInsert,
    ListRemove { swap: bool },
    ListReserve,
    ListTruncate,
    ListContains,
    ListReorder,
    MapRemove(u32),
    MapClear,
    MapReserve,
}

/// One live nonescaping callback descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackDescriptor {
    pub func: FunctionVersionId,
    pub captures: Vec<Value>,
    pub env: TypeEnvId,
    pub owner_depth: u32,
}

/// One reusable callback arena slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackSlot {
    pub generation: u32,
    pub descriptor: Option<CallbackDescriptor>,
}

/// Why one instruction left the plain path.
pub enum ExecOutcome {
    /// The instruction retired inside the machine.
    Continue,
    /// The last frame returned this terminal value.
    Terminal(Value),
    /// A perform: the arguments are recorded in `Pending` by the
    /// driver.
    Perform { op: u32, args: Vec<Value> },
    /// Copy one image-owned value into this run.
    LoadSlot { slot: u32 },
    /// A policy-table edit through a table handle.
    TableEdit {
        table: ObjRef,
        action: u32,
        kind: u32,
        slot: u32,
        mock: Option<Value>,
    },
    /// The operation identity test of a `Call` pattern.
    AsCall {
        request: ObjRef,
        op: u32,
        ty: u32,
        env: TypeEnvId,
    },
    /// `request.op_name()`.
    RequestOp { request: ObjRef },
    /// `call.args()`.
    CallArgs { call: ObjRef },
    /// `value.digest()`. The world resolves code and class identity,
    /// so the digest never names a numeric slot.
    Digest {
        value: ObjRef,
        ty: u32,
        env: TypeEnvId,
    },
    /// Render one value through its stored closed static type.
    DynamicRender { value: Value, ty: u32 },
}

/// The serializable state of one machine.
///
/// These fields contain the compact machine state from specification 16.4.
/// `Machine` stores the cold callback arena separately.
///
/// The interpreter still runs as a method of `Machine`, because
/// allocation needs the policy roots as well. `docs/notes/week7.md`
/// records that difference from the specification `execute` entry.
pub struct VmState {
    pub heap: Heap,
    pub frames: Vec<Frame>,
    pub locals: Vec<Value>,
    pub operands: Vec<Value>,
    pub fuel: u64,
    pub state: MachineState,
    pub pending: Option<Pending>,
    /// The child that must finish this machine's pending VM control
    /// operation before this machine can continue.
    pub nested: Option<VmId>,
    /// A descendant request that this machine's driver received.
    pub routed: Option<RoutedRequest>,
    pub terminal: Option<Terminal>,
    pub parent: Option<VmId>,
    pub next_ordinal: u64,
    /// The next holder-local wait token.
    pub next_wait: u64,
    /// Prepared and active typed wait descriptions.
    pub waits: std::collections::BTreeMap<u64, WaitEntry>,
    /// The bounded mailbox of this machine. A machine that is not a
    /// proc keeps an empty closed mailbox.
    pub mailbox: Mailbox,
    /// Why this machine is blocked, when its state is `Blocked`.
    pub block: Option<Block>,
    /// The per-machine literal string table, indexed by the module
    /// string pool. A literal interns on its first load, stays frozen
    /// from birth, and stays rooted for the machine lifetime, so a
    /// repeated `ConstStr` reuses one object.
    pub literals: Vec<Option<ObjRef>>,
}

/// One machine with compact state, callback state, and host state.
pub struct Machine {
    /// The serializable machine state.
    pub vm: VmState,
    /// Policy state: the effect table this machine owns. A snapshot
    /// excludes it (specification 17.2).
    pub table: PolicyTable,
    /// Execution ownership: the number of live activation references
    /// to this machine on the driver stack. A machine with references
    /// rejects control methods.
    pub active: u32,
    /// True when one active `drive` activation controls this machine.
    ///
    /// The activation stack owns this flag. A snapshot records the
    /// routed request or nested control edge instead.
    pub driven: bool,
    /// Active host work and scoped host resources.
    pub resources: ResourceRegistry,
    /// Resource control: the limits this machine runs under.
    pub config: VmConfig,
    /// Resource control: the number of child machines this machine
    /// reserved from its own budget.
    pub children: u32,
    /// Execution ownership: the holder or the scheduler.
    pub owner: Ownership,
    /// The generation of this machine slot. A handle carries the
    /// generation it was minted with, so a stale handle names a dead
    /// proc instead of a later machine in the same slot.
    pub generation: u32,
    /// True when the scheduler paused this proc and gave the holder a
    /// live `Vm` handle back.
    pub paused: bool,
    /// The barrier that stopped this machine, when one holds it.
    ///
    /// A barrier over an overlapping set finds the marker and waits,
    /// so two barriers never share a machine. A barrier over a
    /// disjoint set proceeds.
    pub barrier: Option<u32>,
    /// The body function of this machine, as a function slot.
    ///
    /// The declared result type of the machine is the result type of
    /// this function, and the first parameter of this function is the
    /// proc instance of a proc. A machine drops its body closure and
    /// its frames, so the record is the one lasting evidence of both
    /// types. A machine that never loaded a frame records `None`.
    pub body_func: Option<FunctionVersionId>,
    /// The type environment of the machine body activation.
    ///
    /// The machine witness. The two types above close through it, so a
    /// machine past its constructor still names both.
    pub witness: TypeEnvId,
    /// True when `Proc.Spawn` launched this machine.
    ///
    /// The flag names the machines that received the birth grant of
    /// specification 18.3, so a restore mints exactly the same grant.
    /// It is not derived from the ownership, because `Proc.Run`
    /// transfers a plain machine to the scheduler and mints no grant.
    pub is_proc: bool,
    /// The persistent image that owns this run.
    ///
    /// Host root machines and legacy host restore targets have no
    /// image. Activated runs and their spawned procs name one image.
    pub image: Option<VmImageKey>,
    /// The world gate a restore put this machine behind.
    ///
    /// Restored procs are scheduler-owned but stopped until the
    /// first `run`, `step`, or `drive` of the restored root opens the
    /// gate (specification 17.5). Zero means the gate is open.
    pub gate: u32,
    /// The proc body to enter after the constructor frame returns.
    ///
    /// A proc constructs its instance inside its own machine
    /// (specification 18.1), so the launch runs two frames: the
    /// constructor, then `on_spawn` over the constructed value.
    pub start_body: Option<ObjRef>,
    /// Machine-local descriptors for active nonescaping callbacks.
    pub callbacks: Vec<CallbackSlot>,
}

impl Machine {
    /// Return the class method table for one runtime value.
    fn virtual_class(&self, module: &Module, value: Value) -> Result<u32, FaultCode> {
        match value {
            Value::Int(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_INT];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Bool(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_BOOL];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Char(_) => {
                let class = module.core_roles[lm_bytecode::corepin::ROLE_CHAR];
                if class == lm_bytecode::NO_ROLE {
                    Err(BAD_TYPE)
                } else {
                    Ok(class)
                }
            }
            Value::Obj(reference) => match self.vm.heap.get(reference) {
                Object::Instance { class, .. } => Ok(*class),
                Object::Str(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_STRING];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Substring(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_SUBSTRING];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Bytes(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_BYTES];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::StrBuilder(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_STRING_BUILDER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::ByteBuf(_) => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_BYTE_BUFFER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::List { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_LIST];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::Map { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_MAP];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTcpStream { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TCP_STREAM];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTcpListener { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TCP_LISTENER];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeTlsStream { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_TLS_STREAM];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeCode(code) => {
                    let role = match code.kind {
                        lm_heap::PortableCodeKind::Artifact => lm_bytecode::corepin::ROLE_ARTIFACT,
                        lm_heap::PortableCodeKind::VerifiedModule => {
                            lm_bytecode::corepin::ROLE_VERIFIED_MODULE
                        }
                        lm_heap::PortableCodeKind::SlotSpec => lm_bytecode::corepin::ROLE_SLOT_SPEC,
                    };
                    let class = module.core_roles[role];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::NativeCodeHandle { kind, .. } => {
                    let role = match kind {
                        lm_heap::CodeHandleKind::Instance => lm_bytecode::corepin::ROLE_INSTANCE,
                        lm_heap::CodeHandleKind::Slot => lm_bytecode::corepin::ROLE_SLOT,
                        lm_heap::CodeHandleKind::Function => {
                            lm_bytecode::corepin::ROLE_FUNCTION_DEF
                        }
                        lm_heap::CodeHandleKind::Class => lm_bytecode::corepin::ROLE_CLASS_DEF,
                    };
                    let class = module.core_roles[role];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                Object::DynValue { .. } => {
                    let class = module.core_roles[lm_bytecode::corepin::ROLE_DYN_VALUE];
                    if class == lm_bytecode::NO_ROLE {
                        Err(BAD_TYPE)
                    } else {
                        Ok(class)
                    }
                }
                _ => Err(BAD_TYPE),
            },
            _ => Err(BAD_TYPE),
        }
    }

    /// A machine without a loaded entry.
    #[cfg(test)]
    pub fn empty(config: VmConfig, parent: Option<VmId>) -> Machine {
        Machine::empty_at(config, parent, 0)
    }

    /// A machine without a loaded entry, at one slot generation.
    #[cfg(test)]
    pub fn empty_at(config: VmConfig, parent: Option<VmId>, generation: u32) -> Machine {
        Machine::empty_with_optional_budgets(config, parent, generation, None, None)
    }

    /// Create an empty machine that charges the world ledgers.
    pub(crate) fn empty_with_budgets(
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
        heap_budget: HeapBudget,
        resource_budget: ResourceBudget,
    ) -> Machine {
        Machine::empty_with_optional_budgets(
            config,
            parent,
            generation,
            Some(heap_budget),
            Some(resource_budget),
        )
    }

    /// Create an empty machine with local heap accounting.
    ///
    /// The resource ledger remains shared. The world attaches the
    /// heap ledger before it creates a second machine.
    pub(crate) fn empty_with_resource_budget(
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
        resource_budget: ResourceBudget,
    ) -> Machine {
        Machine::empty_with_optional_budgets(
            config,
            parent,
            generation,
            None,
            Some(resource_budget),
        )
    }

    fn empty_with_optional_budgets(
        config: VmConfig,
        parent: Option<VmId>,
        generation: u32,
        heap_budget: Option<HeapBudget>,
        resource_budget: Option<ResourceBudget>,
    ) -> Machine {
        Machine {
            vm: VmState {
                heap: match heap_budget {
                    Some(budget) => Heap::with_budget(config.heap_bytes, budget),
                    None => Heap::new(config.heap_bytes),
                },
                frames: Vec::new(),
                locals: Vec::new(),
                operands: Vec::new(),
                fuel: config.fuel,
                state: MachineState::Empty,
                pending: None,
                nested: None,
                routed: None,
                terminal: None,
                parent,
                next_ordinal: 1,
                next_wait: 1,
                waits: std::collections::BTreeMap::new(),
                // A machine that never becomes a proc keeps a closed
                // mailbox, so no send can reach it.
                mailbox: {
                    let mut mailbox = Mailbox::new(0);
                    mailbox.closed = true;
                    mailbox
                },
                block: None,
                literals: Vec::new(),
            },
            table: PolicyTable::default(),
            active: 0,
            driven: false,
            resources: match resource_budget {
                Some(budget) => ResourceRegistry::with_budget(config.max_resources, budget),
                None => ResourceRegistry::new(config.max_resources),
            },
            config,
            children: 0,
            owner: Ownership::Holder,
            generation,
            paused: false,
            barrier: None,
            body_func: None,
            witness: TypeEnvId::EMPTY,
            is_proc: false,
            image: None,
            gate: 0,
            start_body: None,
            callbacks: Vec::new(),
        }
    }

    /// Install the initial frame for a function with its locals
    /// already evaluated. `closure` supplies capture context.
    ///
    /// The arena limit is checked before the slot allocation is sized
    /// from the code. The verifier bounds `local_count` for admitted
    /// modules; this check is the runtime backstop.
    pub fn load_frame(
        &mut self,
        module: &Module,
        func: u32,
        args: Vec<Value>,
        closure: Option<ObjRef>,
        env: TypeEnvId,
    ) {
        let local_count = match module.funcs.get(func as usize) {
            Some(code) => code.local_count() as usize,
            None => {
                self.set_fault(BAD_STATE, "the frame names no function", None);
                return;
            }
        };
        if local_count > self.config.max_stack_values as usize {
            self.set_fault(
                FaultCode::StackLimit,
                "the initial frame exceeds the arena",
                None,
            );
            return;
        }
        self.body_func = Some(func);
        self.witness = env;
        self.vm.locals = args;
        // A slot past the parameters holds no value yet. The marker
        // states that fact, so a snapshot never spells an
        // uninitialized slot as a real unit value. The verifier proves
        // that no read reaches such a slot before its first store.
        self.vm.locals.resize(local_count, Value::Uninit);
        self.vm.operands.clear();
        self.vm.frames.push(Frame {
            func,
            block: 0,
            ip: 0,
            base_local: 0,
            base_operand: 0,
            closure: closure.map(FrameCapture::Closure),
            env,
        });
        self.vm.state = MachineState::Ready;
    }

    pub fn set_done(&mut self, value: Value) {
        self.vm.terminal = Some(Terminal::Done(value));
        self.vm.state = MachineState::Done;
        self.vm.pending = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.callbacks.clear();
        self.close_resources();
        self.compact_terminal_proc();
    }

    pub fn set_fault(&mut self, code: FaultCode, message: impl Into<String>, op: Option<u32>) {
        self.vm.terminal = Some(Terminal::Fault(FaultRec {
            code,
            message: message.into(),
            op,
        }));
        self.vm.state = MachineState::Faulted;
        self.vm.pending = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.callbacks.clear();
        self.close_resources();
        self.compact_terminal_proc();
    }

    /// Remove state that a terminal proc cannot use again.
    pub(crate) fn compact_terminal_proc(&mut self) {
        if !self.is_proc {
            return;
        }
        self.vm.frames = Vec::new();
        self.vm.locals = Vec::new();
        self.vm.operands = Vec::new();
        self.vm.literals = Vec::new();
        self.vm.pending = None;
        self.vm.nested = None;
        self.vm.routed = None;
        self.vm.block = None;
        self.vm.waits.clear();
        self.vm.mailbox.queue = std::collections::VecDeque::new();
        self.start_body = None;
        self.table.exact.fill(None);
        self.table.group.fill(None);
        self.resources.compact_closed();
        self.collect_garbage(&[]);
        let root = match self.vm.terminal.as_ref() {
            Some(Terminal::Done(Value::Obj(reference))) => Some(*reference),
            _ => None,
        };
        match root {
            Some(mut reference) => {
                if self
                    .vm
                    .heap
                    .compact_live(std::slice::from_mut(&mut reference))
                    .is_ok()
                {
                    self.vm.terminal = Some(Terminal::Done(Value::Obj(reference)));
                }
            }
            None => {
                let _ = self.vm.heap.compact_live(&mut []);
            }
        }
    }

    /// Take the next request ordinal without wrapping it.
    pub fn take_request_ordinal(&mut self) -> Result<u64, FaultCode> {
        let ordinal = self.vm.next_ordinal;
        self.vm.next_ordinal = ordinal.checked_add(1).ok_or(FaultCode::IntegerOverflow)?;
        Ok(ordinal)
    }

    /// Close every scoped host resource this machine registered.
    ///
    /// Termination calls this. It invokes no guest callback, and it
    /// never replaces the stored terminal result, so a cleanup does
    /// not hide an existing machine fault.
    pub fn close_resources(&mut self) -> usize {
        self.resources.close_all()
    }

    /// Collect garbage now. `extra` holds additional roots that are
    /// not yet stored in the arenas.
    pub fn collect_garbage(&mut self, extra: &[ObjRef]) {
        let roots = self.gc_roots(extra);
        lm_graph::collect(&mut self.vm.heap, roots);
    }

    /// Every collection root this machine holds outside its heap.
    ///
    /// A boundary transfer into this machine reads the list before it
    /// borrows the heap, because a destination collection during the
    /// copy needs the same roots.
    pub fn gc_roots(&self, extra: &[ObjRef]) -> Vec<ObjRef> {
        let mut roots: Vec<ObjRef> = Vec::new();
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Closure(reference)) = frame.closure {
                roots.push(reference);
            }
        }
        for slot in &self.callbacks {
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Obj(reference) = value {
                        roots.push(*reference);
                    }
                }
            }
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                if let Value::Obj(r) = value {
                    roots.push(*r);
                }
            }
        }
        if let Some(Terminal::Done(Value::Obj(r))) = &self.vm.terminal {
            roots.push(*r);
        }
        // An accepted message lives in this machine's heap until
        // `receive` delivers it, so the queue is a collection root.
        for value in &self.vm.mailbox.queue {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        for action in self.table.exact.iter().chain(self.table.group.iter()) {
            if let Some(Action::Mock(r)) = action {
                roots.push(*r);
            }
        }
        // The proc body waits for the constructor frame to return.
        if let Some(r) = self.start_body {
            roots.push(r);
        }
        // Interned literals stay alive for the machine lifetime.
        roots.extend(self.vm.literals.iter().flatten().copied());
        roots.extend_from_slice(extra);
        roots
    }

    /// The canonical snapshot roots of this machine, in canonical
    /// order.
    ///
    /// The order is the one declaration point of snapshot
    /// reachability: frame closures, locals, operands, pending
    /// arguments, the terminal value, the mailbox queue, the proc
    /// body, and the interned literals.
    ///
    /// The list is the collection roots minus the policy-table
    /// entries. Specification 17.2 excludes policy tables from a
    /// snapshot, so a machine or an object that only a table-held mock
    /// closure names is not snapshot content. `machine_references` and
    /// `snapshot_preflight` read this list, so the closed set and the
    /// encoder agree on what the world holds.
    pub fn snapshot_roots(&self) -> Vec<ObjRef> {
        let mut roots: Vec<ObjRef> = Vec::new();
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Closure(reference)) = frame.closure {
                roots.push(reference);
            }
        }
        for slot in &self.callbacks {
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Obj(reference) = value {
                        roots.push(*reference);
                    }
                }
            }
        }
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                if let Value::Obj(r) = value {
                    roots.push(*r);
                }
            }
        }
        if let Some(Terminal::Done(Value::Obj(r))) = &self.vm.terminal {
            roots.push(*r);
        }
        for value in &self.vm.mailbox.queue {
            if let Value::Obj(r) = value {
                roots.push(*r);
            }
        }
        if let Some(r) = self.start_body {
            roots.push(r);
        }
        roots.extend(self.vm.literals.iter().flatten().copied());
        roots
    }

    /// Return active callbacks in canonical root order.
    pub fn snapshot_callbacks(&self) -> Vec<CallbackRef> {
        let mut callbacks = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let push = |value: Value,
                    callbacks: &mut Vec<CallbackRef>,
                    seen: &mut std::collections::HashSet<CallbackRef>| {
            if let Value::Callback(reference) = value {
                if seen.insert(reference) {
                    callbacks.push(reference);
                }
            }
        };
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Callback(reference)) = frame.closure {
                push(Value::Callback(reference), &mut callbacks, &mut seen);
            }
        }
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            push(*value, &mut callbacks, &mut seen);
        }
        if let Some(pending) = &self.vm.pending {
            for value in &pending.args {
                push(*value, &mut callbacks, &mut seen);
            }
        }
        if let Some(Terminal::Done(value)) = &self.vm.terminal {
            push(*value, &mut callbacks, &mut seen);
        }
        for value in &self.vm.mailbox.queue {
            push(*value, &mut callbacks, &mut seen);
        }
        let mut cursor = 0;
        while cursor < callbacks.len() {
            let reference = callbacks[cursor];
            cursor += 1;
            if let Ok(descriptor) = self.callback(reference) {
                for value in &descriptor.captures {
                    push(*value, &mut callbacks, &mut seen);
                }
            }
        }
        callbacks
    }

    /// Allocate one object. When the cap would be exceeded, collect
    /// first. The children of the new object are roots during the
    /// collection because they are not yet reachable from the arenas.
    pub fn alloc(&mut self, object: Object) -> Result<Value, FaultCode> {
        let mut cost = self.vm.heap.allocation_cost(&object);
        if self.vm.heap.would_exceed(cost) {
            let mut extra = Vec::new();
            object.children(&mut extra);
            self.collect_garbage(&extra);
            cost = self.vm.heap.allocation_cost(&object);
            if self.vm.heap.would_exceed(cost) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(Value::Obj(self.vm.heap.alloc(object)))
    }

    /// Make room for `delta` more bytes of growth on an existing
    /// object. `temps` holds values already popped from the arenas.
    fn reserve(&mut self, delta: usize, temps: &[Value]) -> Result<(), FaultCode> {
        if self.vm.heap.would_exceed_growth(delta) {
            let extra: Vec<ObjRef> = temps.iter().filter_map(|v| v.as_obj()).collect();
            self.collect_garbage(&extra);
            if self.vm.heap.would_exceed_growth(delta) {
                return Err(FaultCode::HeapLimit);
            }
        }
        Ok(())
    }

    /// Compare two map keys. Scalars compare by value; strings by
    /// content.
    fn key_eq(&self, a: Value, b: Value) -> bool {
        match (a, b) {
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Obj(x), Value::Obj(y)) => {
                if x == y {
                    return true;
                }
                match (self.vm.heap.get(x), self.vm.heap.get(y)) {
                    (Object::Str(s1), Object::Str(s2))
                    | (Object::Str(s1), Object::Substring(s2))
                    | (Object::Substring(s1), Object::Str(s2))
                    | (Object::Substring(s1), Object::Substring(s2)) => s1 == s2,
                    (Object::Bytes(b1), Object::Bytes(b2)) => b1 == b2,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Get the lookup hash of one map key.
    ///
    /// Strings and bytes hash by content. The hash agrees with
    /// `key_eq`. It stays inside the process.
    #[inline(never)]
    fn key_hash(&self, key: Value) -> u64 {
        match key {
            Value::Bool(value) => process_lookup_hash((0u8, value)),
            Value::Int(value) => process_lookup_hash((1u8, value)),
            Value::Obj(r) => match self.vm.heap.get(r) {
                Object::Str(s) | Object::Substring(s) => s.lookup_hash() ^ 0x4c_6f_6f_6d_54_65_78,
                Object::Bytes(b) => b.lookup_hash() ^ 0x4c_6f_6f_6d_42_79_74,
                _ => process_lookup_hash(3u8),
            },
            _ => process_lookup_hash(4u8),
        }
    }

    /// Find the entry position of a key in the map object `r` through
    /// the hash index. The index is a cache: the call first indexes
    /// the entries appended since the last lookup.
    fn map_lookup(&mut self, r: ObjRef, key: Value) -> Result<Option<usize>, FaultCode> {
        let (built, len) = match self.vm.heap.get(r) {
            Object::Map { entries, index, .. } => (index.built as usize, entries.len()),
            _ => return Err(FaultCode::TypeMismatch),
        };
        if built < len {
            let mut hashes = Vec::with_capacity(len - built);
            for i in built..len {
                let k = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } => entries[i].0,
                    _ => return Err(FaultCode::TypeMismatch),
                };
                hashes.push(self.key_hash(k));
            }
            if let Object::Map { index, .. } = self.vm.heap.get_mut(r) {
                for (offset, hash) in hashes.into_iter().enumerate() {
                    index.insert(hash, (built + offset) as u32);
                }
            }
        }
        let hash = self.key_hash(key);
        let (entries, candidates) = match self.vm.heap.get(r) {
            Object::Map { entries, index, .. } => (entries, index.candidates(hash)),
            _ => return Err(FaultCode::TypeMismatch),
        };
        for i in candidates {
            let k = match entries.get(i as usize) {
                Some(entry) => entry.0,
                None => continue,
            };
            if self.key_eq(k, key) {
                return Ok(Some(i as usize));
            }
        }
        Ok(None)
    }

    fn frozen_guard(&self, r: ObjRef) -> Result<(), FaultCode> {
        if self.vm.heap.is_frozen(r) {
            Err(FaultCode::FrozenWrite)
        } else {
            Ok(())
        }
    }

    /// The type environment of the running frame.
    #[inline]
    fn frame_env(&self) -> TypeEnvId {
        self.vm.frames.last().map(|f| f.env).unwrap_or_default()
    }

    /// Push the frame of one generic call.
    ///
    /// The three generic instructions live outside `exec_instr`, so
    /// the hot instruction body stays the size it had before the
    /// witness landed. A monomorphic program never reaches them.
    #[inline(never)]
    fn call_generic(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        callee: u32,
        app: u32,
    ) -> Result<(), FaultCode> {
        let argc = module
            .funcs
            .get(callee as usize)
            .ok_or(BAD_STATE)?
            .params
            .len();
        let parent = self.frame_env();
        let env = envs.derive(module, parent, app).map_err(env_fault)?;
        self.push_frame(module, callee, argc, None, env)
    }

    /// Push the frame of one generic virtual call.
    ///
    /// The receiver object carries its class arguments, so the
    /// environment binds them first and the own arguments of the
    /// method after them.
    #[inline(never)]
    fn call_virtual_generic(
        &mut self,
        module: &Module,
        dispatch: &[crate::DispatchRow],
        envs: &mut TypeEnvs,
        selector: u32,
        argc: u32,
        app: u32,
    ) -> Result<(), FaultCode> {
        let argc = argc as usize;
        let recv = self.peek(argc)?;
        let (class, class_env) = match self.vm.heap.get(recv.as_obj().ok_or(BAD_TYPE)?) {
            Object::Instance { class, env, .. } => (*class, env.env()),
            _ => return Err(BAD_TYPE),
        };
        let target = method_of(dispatch, class, selector)?;
        let parent = self.frame_env();
        let own = envs.derive(module, parent, app).map_err(env_fault)?;
        let env = envs
            .method_env(module, target, class, class_env, own)
            .map_err(env_fault)?;
        self.push_frame(module, target, argc + 1, None, env)
    }

    /// Push one method frame selected through an interface bound.
    #[inline(never)]
    fn call_interface(
        &mut self,
        module: &Module,
        dispatch: &[crate::DispatchRow],
        envs: &mut TypeEnvs,
        selector: u32,
        argc: u32,
        recv_ty: u32,
    ) -> Result<(), FaultCode> {
        let argc = argc as usize;
        let recv = self.peek(argc)?;
        let class = self.virtual_class(module, recv)?;
        let target = method_of(dispatch, class, selector)?;
        let parent = self.frame_env();
        let receiver = envs.close(module, recv_ty, parent).map_err(env_fault)?;
        let env = envs
            .interface_method_env(module, target, class, receiver)
            .map_err(env_fault)?
            .ok_or(BAD_TYPE)?;
        self.push_frame(module, target, argc + 1, None, env)
    }

    /// Allocate one instance of a generic class.
    ///
    /// The instance records its own class arguments, so a later
    /// dispatch and a later reflection query read them from the object
    /// itself.
    #[inline(never)]
    fn new_generic(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        class: u32,
        app: u32,
    ) -> Result<Value, FaultCode> {
        let field_count = module
            .classes
            .get(class as usize)
            .ok_or(BAD_STATE)?
            .fields
            .len();
        let parent = self.frame_env();
        let env = envs.derive(module, parent, app).map_err(env_fault)?;
        self.alloc(Object::Instance {
            class,
            fields: vec![Value::Uninit; field_count],
            env: Witness(env),
        })
    }

    /// Execute one native value instruction outside the hot dispatch body.
    #[inline(never)]
    fn exec_native_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::EqStr) => self.str_compare(true),
            Instr::Native(lm_bytecode::NativeInstr::NeStr) => self.str_compare(false),
            Instr::Native(
                lm_bytecode::NativeInstr::StrByteLen
                | lm_bytecode::NativeInstr::StrCharCount
                | lm_bytecode::NativeInstr::StrConcat
                | lm_bytecode::NativeInstr::StrStartsWith
                | lm_bytecode::NativeInstr::StrEndsWith
                | lm_bytecode::NativeInstr::StrContains
                | lm_bytecode::NativeInstr::StrFindIndex
                | lm_bytecode::NativeInstr::TextFindByteIndex
                | lm_bytecode::NativeInstr::TextAtByte
                | lm_bytecode::NativeInstr::TextAt
                | lm_bytecode::NativeInstr::TextSlice
                | lm_bytecode::NativeInstr::TextIsBoundary
                | lm_bytecode::NativeInstr::TextSliceBytes
                | lm_bytecode::NativeInstr::TextBytes
                | lm_bytecode::NativeInstr::TextLt
                | lm_bytecode::NativeInstr::TextLe
                | lm_bytecode::NativeInstr::TextGt
                | lm_bytecode::NativeInstr::TextGe
                | lm_bytecode::NativeInstr::TextTrim
                | lm_bytecode::NativeInstr::TextTrimStart
                | lm_bytecode::NativeInstr::TextTrimEnd
                | lm_bytecode::NativeInstr::TextToLowerAscii
                | lm_bytecode::NativeInstr::TextToUpperAscii
                | lm_bytecode::NativeInstr::TextReplace
                | lm_bytecode::NativeInstr::TextParseIntStatus
                | lm_bytecode::NativeInstr::TextParseIntValue
                | lm_bytecode::NativeInstr::SubstringToString,
            ) => self.exec_string_instr(instr),
            Instr::Native(
                lm_bytecode::NativeInstr::CharCodepoint
                | lm_bytecode::NativeInstr::CharUtf8Len
                | lm_bytecode::NativeInstr::EqChar
                | lm_bytecode::NativeInstr::NeChar
                | lm_bytecode::NativeInstr::LtChar
                | lm_bytecode::NativeInstr::LeChar
                | lm_bytecode::NativeInstr::GtChar
                | lm_bytecode::NativeInstr::GeChar,
            ) => self.exec_char_instr(instr),
            Instr::Native(_) => self.exec_bytes_builder_instr(instr),
            _ => unreachable!("the native dispatcher receives one native instruction"),
        }
    }

    /// Execute one immutable String instruction outside the hot dispatch body.
    #[inline(never)]
    fn exec_string_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::StrByteLen) => {
                let string = self.pop_obj()?;
                let len = self.text_value(string)?.len();
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrCharCount) => {
                let string = self.pop_obj()?;
                let count = self.text_value(string)?.char_count();
                let count = i64::try_from(count).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(count))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrConcat) => {
                let other = self.pop_obj()?;
                let string = self.pop_obj()?;
                let string_text = self.text_value(string)?.clone();
                let other_text = self.text_value(other)?.clone();
                let len = string_text
                    .len()
                    .checked_add(other_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(string), Value::Obj(other)])?;
                let text = string_text
                    .try_concat(&other_text)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrStartsWith) => {
                let prefix = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .starts_with(self.text_value(prefix)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrEndsWith) => {
                let suffix = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .ends_with(self.text_value(suffix)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrContains) => {
                let needle = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .contains(self.text_value(needle)?.as_str());
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::StrFindIndex) => {
                let needle = self.pop_obj()?;
                let string = self.pop_obj()?;
                let found = self
                    .text_value(string)?
                    .find_scalar(self.text_value(needle)?);
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextFindByteIndex) => {
                let needle = self.pop_obj()?;
                let text = self.pop_obj()?;
                let found = self.text_value(text)?.find_byte(self.text_value(needle)?);
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextAtByte) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let value = self
                    .text_value(text)?
                    .scalar_at_byte(index)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Char(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextAt) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let value = self
                    .text_value(text)?
                    .scalar_at(index)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Char(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextSlice) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let text = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let slice = self
                    .text_value(text)?
                    .scalar_slice(start, length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextIsBoundary) => {
                let index = self.pop_int()?;
                let text = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let boundary = self.text_value(text)?.is_char_boundary(index);
                self.push(Value::Bool(boundary))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextSliceBytes) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let text = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let end = start
                    .checked_add(length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let slice = self
                    .text_value(text)?
                    .slice(start, end)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextBytes) => {
                let text = self.pop_obj()?;
                let bytes = self.text_value(text)?.bytes();
                let value = self.alloc(Object::Bytes(bytes))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextTrim
                | lm_bytecode::NativeInstr::TextTrimStart
                | lm_bytecode::NativeInstr::TextTrimEnd,
            ) => {
                let text = self.pop_obj()?;
                let value = self.text_value(text)?;
                let source = value.as_str();
                // Both bounds come from the trimmed views of the same
                // text, so each one sits on a scalar boundary.
                let start = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextTrimEnd) => 0,
                    _ => source.len() - source.trim_start().len(),
                };
                let end = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextTrimStart) => source.len(),
                    _ => source.trim_end().len(),
                };
                let end = end.max(start);
                let slice = value.slice(start, end).ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Substring(slice))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextToLowerAscii
                | lm_bytecode::NativeInstr::TextToUpperAscii,
            ) => {
                let text = self.pop_obj()?;
                let value = self.text_value(text)?;
                // ASCII case mapping keeps every byte width, so the
                // result has the byte length of the input.
                let len = value.len();
                let lower = matches!(
                    instr,
                    Instr::Native(lm_bytecode::NativeInstr::TextToLowerAscii)
                );
                let mapped = if lower {
                    value.as_str().to_ascii_lowercase()
                } else {
                    value.as_str().to_ascii_uppercase()
                };
                self.reserve(len, &[Value::Obj(text)])?;
                let mapped =
                    SharedText::try_from_string(mapped).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(mapped))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::TextReplace) => {
                let replacement = self.pop_obj()?;
                let needle = self.pop_obj()?;
                let text = self.pop_obj()?;
                let source = self.text_value(text)?.as_str();
                let needle_text = self.text_value(needle)?.as_str();
                let replacement_text = self.text_value(replacement)?.as_str();
                // Size the result before the allocation. An empty
                // needle matches at every scalar boundary, so the
                // count comes from the match walk and never from a
                // caller-supplied length.
                let matches = source.match_indices(needle_text).count();
                let removed = matches
                    .checked_mul(needle_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                let added = matches
                    .checked_mul(replacement_text.len())
                    .ok_or(FaultCode::HeapLimit)?;
                let len = source
                    .len()
                    .checked_sub(removed)
                    .and_then(|kept| kept.checked_add(added))
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(
                    len,
                    &[
                        Value::Obj(text),
                        Value::Obj(needle),
                        Value::Obj(replacement),
                    ],
                )?;
                let source = self.text_value(text)?.as_str();
                let needle_text = self.text_value(needle)?.as_str();
                let replacement_text = self.text_value(replacement)?.as_str();
                let joined = source.replace(needle_text, replacement_text);
                let joined =
                    SharedText::try_from_string(joined).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(joined))?;
                self.push(value)?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextParseIntStatus
                | lm_bytecode::NativeInstr::TextParseIntValue,
            ) => {
                let radix = self.pop_int()?;
                let text = self.pop_obj()?;
                let status = matches!(
                    instr,
                    Instr::Native(lm_bytecode::NativeInstr::TextParseIntStatus)
                );
                // Both operands can come from data, so neither one
                // faults. A radix outside 2 to 36 reports status 3.
                let radix = u32::try_from(radix)
                    .ok()
                    .filter(|radix| (2..=36).contains(radix));
                let Some(radix) = radix else {
                    self.push(Value::Int(if status { 3 } else { 0 }))?;
                    return Ok(());
                };
                let parsed = i64::from_str_radix(self.text_value(text)?.as_str(), radix);
                let answer = match (status, parsed) {
                    (true, Ok(_)) => 0,
                    (true, Err(error)) => match error.kind() {
                        std::num::IntErrorKind::PosOverflow
                        | std::num::IntErrorKind::NegOverflow => 2,
                        _ => 1,
                    },
                    (false, Ok(value)) => value,
                    (false, Err(_)) => 0,
                };
                self.push(Value::Int(answer))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextLt
                | lm_bytecode::NativeInstr::TextLe
                | lm_bytecode::NativeInstr::TextGt
                | lm_bytecode::NativeInstr::TextGe,
            ) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let ordering = self
                    .text_value(left)?
                    .as_str()
                    .cmp(self.text_value(right)?.as_str());
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::TextLt) => ordering.is_lt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextLe) => !ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextGt) => ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::TextGe) => !ordering.is_lt(),
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SubstringToString) => {
                let substring = self.pop_obj()?;
                let text = match self.vm.heap.get(substring) {
                    Object::Substring(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                if !text.has_bounded_retention() {
                    self.reserve(text.len(), &[Value::Obj(substring)])?;
                }
                let text = text.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            _ => unreachable!("the String dispatcher receives one String instruction"),
        }
        Ok(())
    }

    /// Execute one immediate Char instruction.
    #[inline(never)]
    fn exec_char_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::CharCodepoint) => {
                let value = self.pop_char()?;
                self.push(Value::Int(i64::from(u32::from(value))))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::CharUtf8Len) => {
                let value = self.pop_char()?;
                self.push(Value::Int(value.len_utf8() as i64))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::EqChar
                | lm_bytecode::NativeInstr::NeChar
                | lm_bytecode::NativeInstr::LtChar
                | lm_bytecode::NativeInstr::LeChar
                | lm_bytecode::NativeInstr::GtChar
                | lm_bytecode::NativeInstr::GeChar,
            ) => {
                let right = self.pop_char()?;
                let left = self.pop_char()?;
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::EqChar) => left == right,
                    Instr::Native(lm_bytecode::NativeInstr::NeChar) => left != right,
                    Instr::Native(lm_bytecode::NativeInstr::LtChar) => left < right,
                    Instr::Native(lm_bytecode::NativeInstr::LeChar) => left <= right,
                    Instr::Native(lm_bytecode::NativeInstr::GtChar) => left > right,
                    Instr::Native(lm_bytecode::NativeInstr::GeChar) => left >= right,
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            _ => unreachable!("the Char dispatcher receives one Char instruction"),
        }
        Ok(())
    }

    /// Execute one Bytes or builder instruction outside the hot dispatch body.
    #[inline(never)]
    fn exec_bytes_builder_instr(&mut self, instr: Instr) -> Result<(), FaultCode> {
        match instr {
            Instr::Native(lm_bytecode::NativeInstr::SbNew) => {
                let value = self.alloc(Object::StrBuilder(NativeStringBuilder::new()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendStr) => {
                let string = self.pop_obj()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let text_len = match self.vm.heap.get(string) {
                    Object::Str(text) | Object::Substring(text) => text.len(),
                    _ => return Err(BAD_TYPE),
                };
                let growth = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => builder.reserve_growth(text_len),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(builder), Value::Obj(string)])?;
                    match self.vm.heap.get_mut(builder) {
                        Object::StrBuilder(builder) => {
                            if !builder
                                .try_reserve(text_len)
                                .map_err(|_| FaultCode::HeapLimit)?
                            {
                                return Err(FaultCode::InvalidVmState);
                            }
                        }
                        _ => return Err(BAD_TYPE),
                    }
                }
                if !self.vm.heap.append_string(builder, string) {
                    return Err(FaultCode::InvalidVmState);
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(builder);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendInt) => {
                let value = self.pop_int()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                self.sb_append_int(builder, value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendBool) => {
                let value = self.pop_bool()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let text = if value { "true" } else { "false" };
                self.sb_append(builder, text)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbBuild) => {
                let builder = self.pop_obj()?;
                let (len, scalar_count, ascii) = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => {
                        let len = builder.byte_len().ok_or(FaultCode::InvalidVmState)?;
                        let scalar_count = builder.scalar_len().ok_or(FaultCode::InvalidVmState)?;
                        let ascii = builder.is_ascii().ok_or(FaultCode::InvalidVmState)?;
                        (len, scalar_count, ascii)
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(len, &[Value::Obj(builder)])?;
                let source = match self.vm.heap.get(builder) {
                    Object::StrBuilder(builder) => {
                        builder.buffer().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                let text = SharedText::try_from_str_parts(source, scalar_count, ascii)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbLen) => {
                let builder = self.pop_obj()?;
                let len = match self.vm.heap.get(builder) {
                    Object::StrBuilder(text) => {
                        text.scalar_len().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbByteLen) => {
                let builder = self.pop_obj()?;
                let len = match self.vm.heap.get(builder) {
                    Object::StrBuilder(text) => text.byte_len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbAppendChar) => {
                let value = self.pop_char()?;
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let len = value.len_utf8();
                let growth = match self.vm.heap.get(builder) {
                    Object::StrBuilder(target) => target.reserve_growth(len),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(builder)])?;
                }
                match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(target) => {
                        if growth != 0
                            && !target.try_reserve(len).map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !target.push(value) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(builder);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbFinish) => {
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let (text, scalar_count, ascii) = match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(builder) => {
                        builder.finish().ok_or(FaultCode::InvalidVmState)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge_local(builder);
                let text = SharedText::try_from_string_parts(text, scalar_count, ascii)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::SbClear) => {
                let builder = self.pop_obj()?;
                self.frozen_guard(builder)?;
                let cleared = match self.vm.heap.get_mut(builder) {
                    Object::StrBuilder(text) => text.clear(),
                    _ => return Err(BAD_TYPE),
                };
                if !cleared {
                    return Err(FaultCode::InvalidVmState);
                }
                self.push(Value::Obj(builder))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbNew) => {
                let value = self.alloc(Object::ByteBuf(NativeByteBuffer::new()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbAppend) => {
                let value = self.pop_int()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let byte = u8::try_from(value).map_err(|_| FaultCode::IntegerOverflow)?;
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.reserve_growth(1),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => {
                        if growth != 0 && !bytes.try_reserve(1).map_err(|_| FaultCode::HeapLimit)? {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !bytes.push(byte) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbLen) => {
                let buffer = self.pop_obj()?;
                let len = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbBuild) => {
                let buffer = self.pop_obj()?;
                let len = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.len().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(len, &[Value::Obj(buffer)])?;
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(len)
                    .map_err(|_| FaultCode::HeapLimit)?;
                match self.vm.heap.get(buffer) {
                    Object::ByteBuf(source) => {
                        bytes.extend_from_slice(source.buffer().ok_or(FaultCode::InvalidVmState)?)
                    }
                    _ => return Err(BAD_TYPE),
                }
                let value = self.alloc(Object::Bytes(bytes.into()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbExtend) => {
                let source = self.pop_obj()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let bytes = match self.vm.heap.get(source) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(target) => target.reserve_growth(bytes.len()),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer), Value::Obj(source)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(target) => {
                        if growth != 0
                            && !target
                                .try_reserve(bytes.len())
                                .map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                        if !target.extend(&bytes) {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbReserve) => {
                let additional = self.pop_int()?;
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IntegerOverflow)?;
                let growth = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) => bytes.reserve_growth(additional),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::InvalidVmState)?;
                if growth != 0 {
                    self.reserve(growth, &[Value::Obj(buffer)])?;
                }
                match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => {
                        if growth != 0
                            && !bytes
                                .try_reserve(additional)
                                .map_err(|_| FaultCode::HeapLimit)?
                        {
                            return Err(FaultCode::InvalidVmState);
                        }
                    }
                    _ => return Err(BAD_TYPE),
                }
                if growth != 0 {
                    self.vm.heap.recharge_local(buffer);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbClear) => {
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let cleared = match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(bytes) => bytes.clear(),
                    _ => return Err(BAD_TYPE),
                };
                if !cleared {
                    return Err(FaultCode::InvalidVmState);
                }
                self.push(Value::Obj(buffer))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbFinish) => {
                let buffer = self.pop_obj()?;
                self.frozen_guard(buffer)?;
                let bytes = match self.vm.heap.get_mut(buffer) {
                    Object::ByteBuf(buffer) => buffer.finish().ok_or(FaultCode::InvalidVmState)?,
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge_local(buffer);
                let value = self.alloc(Object::Bytes(SharedBytes::from(bytes)))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbAt) => {
                let index = self.pop_int()?;
                let buffer = self.pop_obj()?;
                let bytes = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) if bytes.buffer().is_some() => bytes,
                    Object::ByteBuf(_) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_TYPE),
                };
                let value = usize::try_from(index)
                    .ok()
                    .and_then(|index| bytes.at(index))
                    .map(i64::from)
                    .unwrap_or(-1);
                self.push(Value::Int(value))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BbFindFrom) => {
                let start = self.pop_int()?;
                let needle = self.pop_obj()?;
                let buffer = self.pop_obj()?;
                let needle = match self.vm.heap.get(needle) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let bytes = match self.vm.heap.get(buffer) {
                    Object::ByteBuf(bytes) if bytes.buffer().is_some() => bytes,
                    Object::ByteBuf(_) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_TYPE),
                };
                let found = usize::try_from(start)
                    .ok()
                    .and_then(|start| bytes.find_from(&needle, start))
                    .and_then(|index| i64::try_from(index).ok())
                    .unwrap_or(-1);
                self.push(Value::Int(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesNew) => {
                let string = self.pop_obj()?;
                let text = match self.vm.heap.get(string) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Bytes(text.bytes()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesLen) => {
                let bytes = self.pop_obj()?;
                let len = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.len(),
                    _ => return Err(BAD_TYPE),
                };
                let len = i64::try_from(len).map_err(|_| FaultCode::IntegerOverflow)?;
                self.push(Value::Int(len))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesText) => {
                let bytes_ref = self.pop_obj()?;
                let bytes = match self.vm.heap.get(bytes_ref) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let view = bytes.utf8_view().ok_or(FaultCode::BadCast)?;
                if !view.has_bounded_retention() {
                    self.reserve(view.len(), &[Value::Obj(bytes_ref)])?;
                }
                let text = view.try_bounded().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesAt) => {
                let index = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let index = usize::try_from(index).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let byte = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.as_slice().get(index).copied(),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                self.push(Value::Int(i64::from(byte)))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesGet) => {
                let index = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let byte =
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| match self.vm.heap.get(bytes) {
                            Object::Bytes(bytes) => bytes.as_slice().get(index).copied(),
                            _ => None,
                        });
                if !matches!(self.vm.heap.get(bytes), Object::Bytes(_)) {
                    return Err(BAD_TYPE);
                }
                self.push(Value::Int(byte.map(i64::from).unwrap_or(-1)))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesSlice) => {
                let length = self.pop_int()?;
                let start = self.pop_int()?;
                let bytes = self.pop_obj()?;
                let start = usize::try_from(start).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let end = start
                    .checked_add(length)
                    .ok_or(FaultCode::IndexOutOfBounds)?;
                let slice = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.slice(start, end),
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = self.alloc(Object::Bytes(slice))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesConcat) => {
                let other = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let (left, right) = match (self.vm.heap.get(bytes), self.vm.heap.get(other)) {
                    (Object::Bytes(left), Object::Bytes(right)) => (left.clone(), right.clone()),
                    _ => return Err(BAD_TYPE),
                };
                let len = left
                    .len()
                    .checked_add(right.len())
                    .ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(bytes), Value::Obj(other)])?;
                let joined = left.try_concat(&right).map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Bytes(joined))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith) => {
                let prefix = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(prefix)) {
                    (Object::Bytes(bytes), Object::Bytes(prefix)) => {
                        bytes.as_slice().starts_with(prefix.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::TextSplit | lm_bytecode::NativeInstr::TextLines,
            ) => {
                let split = matches!(instr, Instr::Native(lm_bytecode::NativeInstr::TextSplit));
                let separator = if split { Some(self.pop_obj()?) } else { None };
                let text = self.pop_obj()?;
                // Collect the byte ranges first. Each piece shares the
                // source allocation, so the walk sizes the result
                // before it allocates anything.
                let ranges: Vec<(usize, usize)> = {
                    let source = self.text_value(text)?;
                    let visible = source.as_str();
                    match separator {
                        Some(reference) => {
                            let needle = self.text_value(reference)?.as_str();
                            let mut ranges = Vec::new();
                            if needle.is_empty() {
                                // An empty separator matches at every
                                // scalar boundary, so the result holds
                                // one empty piece at each end and one
                                // piece for each scalar.
                                ranges.push((0, 0));
                                let mut start = 0;
                                for (at, scalar) in visible.char_indices() {
                                    ranges.push((at, at + scalar.len_utf8()));
                                    start = at + scalar.len_utf8();
                                }
                                ranges.push((start, visible.len()));
                            } else {
                                let mut start = 0;
                                while let Some(at) = visible[start..].find(needle) {
                                    let at = start + at;
                                    ranges.push((start, at));
                                    start = at + needle.len();
                                }
                                ranges.push((start, visible.len()));
                            }
                            ranges
                        }
                        None => {
                            let mut ranges = Vec::new();
                            let mut start = 0;
                            while start < visible.len() {
                                let end = visible[start..]
                                    .find('\n')
                                    .map(|at| start + at)
                                    .unwrap_or(visible.len());
                                // A carriage return before the newline
                                // belongs to the separator.
                                let stop = if visible[start..end].ends_with('\r') {
                                    end - 1
                                } else {
                                    end
                                };
                                ranges.push((start, stop));
                                start = end + 1;
                            }
                            ranges
                        }
                    }
                };
                // One Substring object and one list slot per piece.
                // `alloc` charges the exact cost; this bound only
                // keeps the walk from starting work it cannot finish.
                let cost = ranges
                    .len()
                    .checked_mul(2 * lm_heap::MIN_OBJECT_COST)
                    .ok_or(FaultCode::HeapLimit)?;
                let mut roots = vec![Value::Obj(text)];
                if let Some(reference) = separator {
                    roots.push(Value::Obj(reference));
                }
                self.reserve(cost, &roots)?;
                let mut items = Vec::with_capacity(ranges.len());
                for (start, end) in ranges {
                    let piece = self
                        .text_value(text)?
                        .slice(start, end)
                        .ok_or(FaultCode::IndexOutOfBounds)?;
                    let value = self.alloc(Object::Substring(piece))?;
                    items.push(value);
                    // Every piece stays reachable through the operand
                    // stack until the list owns it.
                    self.push(value)?;
                }
                for _ in 0..items.len() {
                    self.vm.operands.pop();
                }
                let value = self.alloc(Object::List {
                    items,
                    epoch: StructuralEpoch::default(),
                })?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesEndsWith) => {
                let suffix = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(suffix)) {
                    (Object::Bytes(bytes), Object::Bytes(suffix)) => {
                        bytes.as_slice().ends_with(suffix.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesContains) => {
                let needle = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(needle)) {
                    (Object::Bytes(bytes), Object::Bytes(needle)) => {
                        let needle = needle.as_slice();
                        needle.is_empty()
                            || bytes
                                .as_slice()
                                .windows(needle.len())
                                .any(|window| window == needle)
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(found))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex) => {
                let needle = self.pop_obj()?;
                let bytes = self.pop_obj()?;
                let found = match (self.vm.heap.get(bytes), self.vm.heap.get(needle)) {
                    (Object::Bytes(bytes), Object::Bytes(needle)) => {
                        let needle = needle.as_slice();
                        if needle.is_empty() {
                            Some(0)
                        } else {
                            bytes
                                .as_slice()
                                .windows(needle.len())
                                .position(|window| window == needle)
                        }
                    }
                    _ => return Err(BAD_TYPE),
                };
                let index = match found {
                    Some(index) => i64::try_from(index).map_err(|_| FaultCode::IntegerOverflow)?,
                    None => -1,
                };
                self.push(Value::Int(index))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesHex) => {
                let bytes_ref = self.pop_obj()?;
                let bytes = match self.vm.heap.get(bytes_ref) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let len = bytes.len().checked_mul(2).ok_or(FaultCode::HeapLimit)?;
                self.reserve(len, &[Value::Obj(bytes_ref)])?;
                let mut text = String::new();
                text.try_reserve_exact(len)
                    .map_err(|_| FaultCode::HeapLimit)?;
                const HEX: &[u8; 16] = b"0123456789abcdef";
                for byte in bytes.as_slice() {
                    text.push(char::from(HEX[(byte >> 4) as usize]));
                    text.push(char::from(HEX[(byte & 0x0f) as usize]));
                }
                let value = self.alloc(Object::Str(text.into()))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8) => {
                let bytes = self.pop_obj()?;
                let valid = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.is_utf8(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(valid))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::EqBytes)
            | Instr::Native(lm_bytecode::NativeInstr::NeBytes) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let equal = match (self.vm.heap.get(left), self.vm.heap.get(right)) {
                    (Object::Bytes(left), Object::Bytes(right)) => left == right,
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(
                    equal == matches!(instr, Instr::Native(lm_bytecode::NativeInstr::EqBytes)),
                ))?;
            }
            Instr::Native(
                lm_bytecode::NativeInstr::LtBytes
                | lm_bytecode::NativeInstr::LeBytes
                | lm_bytecode::NativeInstr::GtBytes
                | lm_bytecode::NativeInstr::GeBytes,
            ) => {
                let right = self.pop_obj()?;
                let left = self.pop_obj()?;
                let ordering = match (self.vm.heap.get(left), self.vm.heap.get(right)) {
                    (Object::Bytes(left), Object::Bytes(right)) => {
                        left.as_slice().cmp(right.as_slice())
                    }
                    _ => return Err(BAD_TYPE),
                };
                let result = match instr {
                    Instr::Native(lm_bytecode::NativeInstr::LtBytes) => ordering.is_lt(),
                    Instr::Native(lm_bytecode::NativeInstr::LeBytes) => !ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::GtBytes) => ordering.is_gt(),
                    Instr::Native(lm_bytecode::NativeInstr::GeBytes) => !ordering.is_lt(),
                    _ => unreachable!(),
                };
                self.push(Value::Bool(result))?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesCompact) => {
                let reference = self.pop_obj()?;
                let bytes = match self.vm.heap.get(reference) {
                    Object::Bytes(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(bytes.len(), &[Value::Obj(reference)])?;
                let compact = bytes.try_compact().map_err(|_| FaultCode::HeapLimit)?;
                let value = self.alloc(Object::Bytes(compact))?;
                self.push(value)?;
            }
            Instr::Native(lm_bytecode::NativeInstr::BytesTextView) => {
                let reference = self.pop_obj()?;
                let text = match self.vm.heap.get(reference) {
                    Object::Bytes(bytes) => bytes.utf8_view().ok_or(FaultCode::BadCast)?,
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Substring(text))?;
                self.push(value)?;
            }
            _ => unreachable!("the Bytes dispatcher receives one native value instruction"),
        }
        Ok(())
    }

    /// Read one list element outside the main dispatch body.
    #[inline(never)]
    fn exec_list_at(&mut self) -> Result<(), FaultCode> {
        let idx = self.pop_int()?;
        let r = self.pop_obj()?;
        let value = match self.vm.heap.get(r) {
            Object::List { items, .. } => {
                if idx < 0 || idx as usize >= items.len() {
                    return Err(FaultCode::IndexOutOfBounds);
                }
                items[idx as usize]
            }
            _ => return Err(BAD_TYPE),
        };
        self.push(value)
    }

    /// Insert one map entry outside the base dispatch body.
    #[inline(never)]
    fn exec_map_put(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        ty: u32,
        discard: bool,
    ) -> Result<(), FaultCode> {
        let value = self.pop()?;
        let key = self.pop()?;
        let r = self.pop_obj()?;
        self.frozen_guard(r)?;
        let pos = self.map_lookup(r, key)?;
        let previous = match pos {
            Some(pos) => match self.vm.heap.get_mut(r) {
                Object::Map { entries, .. } => {
                    let entry = entries.get_mut(pos).ok_or(BAD_STATE)?;
                    if discard {
                        entry.1 = value;
                        None
                    } else {
                        Some(std::mem::replace(&mut entry.1, value))
                    }
                }
                _ => return Err(BAD_TYPE),
            },
            None => {
                self.reserve(32, &[Value::Obj(r), key, value])?;
                match self.vm.heap.get_mut(r) {
                    Object::Map { entries, index } => {
                        index.epoch.bump()?;
                        entries.push((key, value));
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                None
            }
        };
        if !discard {
            match previous {
                Some(previous) => self.push(previous)?,
                None => {
                    let ty = self.close_option_family(module, envs, ty)?;
                    self.push(Value::EmptyCase { ty, arm: 1 })?;
                }
            }
        }
        Ok(())
    }

    /// Execute one native option collection operation.
    #[inline(never)]
    fn exec_option_collection(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        op: OptionCollectionOp,
    ) -> Result<(), FaultCode> {
        match op {
            OptionCollectionOp::OptionNone(ty) => {
                let ty = self.close_option_family(module, envs, ty)?;
                self.push(Value::EmptyCase { ty, arm: 1 })?;
            }
            OptionCollectionOp::OptionPayload(ty) => {
                let value = *self.vm.operands.last().ok_or(BAD_STATE)?;
                let family = self.close_option_family(module, envs, ty)?;
                if matches!(value, Value::EmptyCase { ty, arm: 1 } if ty == family) {
                    return Err(BAD_TYPE);
                }
            }
            OptionCollectionOp::ListGet(ty) => {
                let idx = self.pop_int()?;
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::List { items, .. } if idx >= 0 => items.get(idx as usize).copied(),
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                };
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            OptionCollectionOp::MapGet(ty) => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let value = match self.map_lookup(r, key)? {
                    Some(pos) => match self.vm.heap.get(r) {
                        Object::Map { entries, .. } => Some(entries.get(pos).ok_or(BAD_STATE)?.1),
                        _ => return Err(BAD_TYPE),
                    },
                    None => None,
                };
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Execute one collection traversal operation.
    #[inline(never)]
    fn exec_collection_iteration(&mut self, op: CollectionIterationOp) -> Result<(), FaultCode> {
        match op {
            CollectionIterationOp::ListEpoch => {
                let r = self.pop_obj()?;
                let epoch = match self.vm.heap.get_mut(r) {
                    Object::List { epoch, .. } => epoch.observe(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(i64::from(epoch)))?;
            }
            CollectionIterationOp::ListIterLen => {
                let expected = self.pop_int()?;
                let r = self.pop_obj()?;
                let (len, epoch) = match self.vm.heap.get(r) {
                    Object::List { items, epoch } => (items.len(), epoch.0),
                    _ => return Err(BAD_TYPE),
                };
                if expected < 0 || epoch != expected as u32 {
                    return Err(FaultCode::CollectionModified);
                }
                self.push(Value::Int(len as i64))?;
            }
            CollectionIterationOp::MapEpoch => {
                let r = self.pop_obj()?;
                let epoch = match self.vm.heap.get_mut(r) {
                    Object::Map { index, .. } => index.epoch.observe(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(i64::from(epoch)))?;
            }
            CollectionIterationOp::MapIterLen => {
                let expected = self.pop_int()?;
                let r = self.pop_obj()?;
                let (len, epoch) = match self.vm.heap.get(r) {
                    Object::Map { entries, index } => (entries.len(), index.epoch.0),
                    _ => return Err(BAD_TYPE),
                };
                if expected < 0 || epoch != expected as u32 {
                    return Err(FaultCode::CollectionModified);
                }
                self.push(Value::Int(len as i64))?;
            }
            CollectionIterationOp::MapEntry { value } => {
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                let entry = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } if index >= 0 => {
                        entries.get(index as usize).copied()
                    }
                    Object::Map { .. } => None,
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                let value = if value { entry.1 } else { entry.0 };
                self.push(value)?;
            }
        }
        Ok(())
    }

    /// Create one callback outside the main dispatch body.
    #[inline(never)]
    fn exec_make_callback(&mut self, func: u32, captures: u32) -> Result<(), FaultCode> {
        let split = self
            .vm
            .operands
            .len()
            .checked_sub(captures as usize)
            .ok_or(BAD_STATE)?;
        let captured: Vec<Value> = self.vm.operands.split_off(split);
        let value = self.alloc_callback(func, captured, self.frame_env())?;
        self.push(value)
    }

    /// Validate one heap closure as a nonescaping callback.
    #[inline(never)]
    fn exec_as_callback(&mut self) -> Result<(), FaultCode> {
        let value = *self.vm.operands.last().ok_or(BAD_STATE)?;
        let Value::Obj(reference) = value else {
            return Err(BAD_TYPE);
        };
        if !matches!(self.vm.heap.get(reference), Object::Closure { .. }) {
            return Err(BAD_TYPE);
        }
        Ok(())
    }

    /// Execute one extended collection operation outside the hot dispatch body.
    #[inline(never)]
    fn exec_collection_extension(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        op: CollectionExtensionOp,
    ) -> Result<(), FaultCode> {
        match op {
            CollectionExtensionOp::ListCapacity => {
                let r = self.pop_obj()?;
                let capacity = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.capacity(),
                    _ => return Err(BAD_TYPE),
                };
                let capacity = i64::try_from(capacity).map_err(|_| FaultCode::HeapLimit)?;
                self.push(Value::Int(capacity))?;
            }
            CollectionExtensionOp::ListSet => {
                let value = self.pop()?;
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let item = match self.vm.heap.get_mut(r) {
                    Object::List { items, .. } if index >= 0 => items.get_mut(index as usize),
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                }
                .ok_or(FaultCode::IndexOutOfBounds)?;
                *item = value;
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListPop(ty) => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let value = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } if !items.is_empty() => {
                        epoch.bump()?;
                        items.pop()
                    }
                    Object::List { .. } => None,
                    _ => return Err(BAD_TYPE),
                };
                if value.is_some() {
                    self.vm.heap.recharge(r);
                }
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            CollectionExtensionOp::ListInsert => {
                let value = self.pop()?;
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let length = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.len(),
                    _ => return Err(BAD_TYPE),
                };
                if index < 0 || index as usize > length {
                    return Err(FaultCode::IndexOutOfBounds);
                }
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } => {
                        epoch.bump()?;
                        items.insert(index as usize, value);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListRemove { swap } => {
                let index = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let value = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch }
                        if index >= 0 && (index as usize) < items.len() =>
                    {
                        epoch.bump()?;
                        if swap {
                            items.swap_remove(index as usize)
                        } else {
                            items.remove(index as usize)
                        }
                    }
                    Object::List { .. } => return Err(FaultCode::IndexOutOfBounds),
                    _ => return Err(BAD_TYPE),
                };
                self.vm.heap.recharge(r);
                self.push(value)?;
            }
            CollectionExtensionOp::ListReserve => {
                let additional = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let growth = additional.checked_mul(16).ok_or(FaultCode::HeapLimit)?;
                if let Object::List { items, epoch } = self.vm.heap.get(r) {
                    if additional > items.capacity().saturating_sub(items.len()) {
                        epoch.ensure_bumpable()?;
                    }
                } else {
                    return Err(BAD_TYPE);
                }
                self.reserve(growth, &[Value::Obj(r)])?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::List { items, .. } => {
                        let before = items.capacity();
                        items
                            .try_reserve(additional)
                            .map_err(|_| FaultCode::HeapLimit)?;
                        items.capacity() != before
                    }
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    match self.vm.heap.get_mut(r) {
                        Object::List { epoch, .. } => epoch.bump()?,
                        _ => return Err(BAD_TYPE),
                    }
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListTruncate => {
                let length = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let length = usize::try_from(length).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } if length < items.len() => {
                        epoch.bump()?;
                        items.truncate(length);
                        true
                    }
                    Object::List { .. } => false,
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    self.vm.heap.recharge(r);
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::ListContains => {
                let needle = self.pop()?;
                let r = self.pop_obj()?;
                let items = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items,
                    _ => return Err(BAD_TYPE),
                };
                let mut found = false;
                for item in items {
                    if self.values_equal(module, *item, needle)? {
                        found = true;
                        break;
                    }
                }
                self.push(Value::Bool(found))?;
            }
            CollectionExtensionOp::ListReorder => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                match self.vm.heap.get_mut(r) {
                    Object::List { epoch, .. } => epoch.bump()?,
                    _ => return Err(BAD_TYPE),
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::MapRemove(ty) => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let position = self.map_lookup(r, key)?;
                let value = match position {
                    Some(position) => match self.vm.heap.get_mut(r) {
                        Object::Map { entries, index } => {
                            index.epoch.bump()?;
                            let value = entries.remove(position).1;
                            index.clear();
                            Some(value)
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    None => None,
                };
                if value.is_some() {
                    self.vm.heap.recharge(r);
                }
                match value {
                    Some(value) => self.push(value)?,
                    None => {
                        let ty = self.close_option_family(module, envs, ty)?;
                        self.push(Value::EmptyCase { ty, arm: 1 })?;
                    }
                }
            }
            CollectionExtensionOp::MapClear => {
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::Map { entries, index } if !entries.is_empty() => {
                        index.epoch.bump()?;
                        entries.clear();
                        index.clear();
                        true
                    }
                    Object::Map { .. } => false,
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    self.vm.heap.recharge(r);
                }
                self.push(Value::Unit)?;
            }
            CollectionExtensionOp::MapReserve => {
                let additional = self.pop_int()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let additional =
                    usize::try_from(additional).map_err(|_| FaultCode::IndexOutOfBounds)?;
                let growth = additional.checked_mul(32).ok_or(FaultCode::HeapLimit)?;
                if let Object::Map { entries, index } = self.vm.heap.get(r) {
                    if additional > entries.capacity().saturating_sub(entries.len()) {
                        index.epoch.ensure_bumpable()?;
                    }
                } else {
                    return Err(BAD_TYPE);
                }
                self.reserve(growth, &[Value::Obj(r)])?;
                let changed = match self.vm.heap.get_mut(r) {
                    Object::Map { entries, .. } => {
                        let before = entries.capacity();
                        entries
                            .try_reserve(additional)
                            .map_err(|_| FaultCode::HeapLimit)?;
                        entries.capacity() != before
                    }
                    _ => return Err(BAD_TYPE),
                };
                if changed {
                    match self.vm.heap.get_mut(r) {
                        Object::Map { index, .. } => index.epoch.bump()?,
                        _ => return Err(BAD_TYPE),
                    }
                }
                self.push(Value::Unit)?;
            }
        }
        Ok(())
    }

    /// Execute one interface call outside the base dispatch body.
    #[inline(never)]
    fn exec_interface_call(
        &mut self,
        module: &Module,
        dispatch: &[crate::DispatchRow],
        envs: &mut TypeEnvs,
        interface: u32,
        method: u32,
        recv_ty: u32,
    ) -> Result<(), FaultCode> {
        let requirement = module
            .interfaces
            .get(interface as usize)
            .and_then(|contract| contract.methods.get(method as usize))
            .ok_or(BAD_STATE)?;
        let selector = requirement.selector;
        let argc = u32::try_from(requirement.params.len()).map_err(|_| BAD_STATE)?;
        self.call_interface(module, dispatch, envs, selector, argc, recv_ty)
    }

    /// Read and validate one syntax tree value.
    fn syntax_tree_parts(
        &self,
        reference: ObjRef,
        tree_class: u32,
    ) -> Result<(Value, Value, SharedText, SharedBytes), FaultCode> {
        let Object::Instance { class, fields, .. } = self.vm.heap.get(reference) else {
            return Err(BAD_TYPE);
        };
        if *class != tree_class {
            return Err(BAD_TYPE);
        }
        let [source, records] = fields.as_slice() else {
            return Err(BAD_TYPE);
        };
        let source_ref = source.as_obj().ok_or(BAD_TYPE)?;
        let records_ref = records.as_obj().ok_or(BAD_TYPE)?;
        let Object::Str(text) = self.vm.heap.get(source_ref) else {
            return Err(BAD_TYPE);
        };
        let Object::Bytes(bytes) = self.vm.heap.get(records_ref) else {
            return Err(BAD_TYPE);
        };
        Ok((*source, *records, text.clone(), bytes.clone()))
    }

    /// Read and validate one syntax element value.
    fn syntax_element_parts(
        &self,
        reference: ObjRef,
        node: u32,
        token: u32,
        trivia: u32,
    ) -> Result<(Value, Value, SharedText, SharedBytes, u32), FaultCode> {
        let Object::Instance { class, fields, .. } = self.vm.heap.get(reference) else {
            return Err(BAD_TYPE);
        };
        if *class != node && *class != token && *class != trivia {
            return Err(BAD_TYPE);
        }
        let [source, records, Value::Int(index)] = fields.as_slice() else {
            return Err(BAD_TYPE);
        };
        let index = u32::try_from(*index).map_err(|_| BAD_TYPE)?;
        let source_ref = source.as_obj().ok_or(BAD_TYPE)?;
        let records_ref = records.as_obj().ok_or(BAD_TYPE)?;
        let Object::Str(text) = self.vm.heap.get(source_ref) else {
            return Err(BAD_TYPE);
        };
        let Object::Bytes(bytes) = self.vm.heap.get(records_ref) else {
            return Err(BAD_TYPE);
        };
        Ok((*source, *records, text.clone(), bytes.clone(), index))
    }

    /// Allocate one syntax view with shared immutable backing.
    fn alloc_syntax_view(
        &mut self,
        class: u32,
        source: Value,
        records: Value,
        index: u32,
    ) -> Result<Value, FaultCode> {
        let value = self.alloc(Object::Instance {
            class,
            fields: vec![source, records, Value::Int(i64::from(index))],
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(FaultCode::MalformedState)?;
        self.vm.heap.set_frozen(reference);
        Ok(value)
    }

    /// Allocate one frozen syntax tree with immutable backing.
    fn alloc_syntax_tree(
        &mut self,
        class: u32,
        source: Value,
        records: Value,
    ) -> Result<Value, FaultCode> {
        let value = self.alloc(Object::Instance {
            class,
            fields: vec![source, records],
            env: Witness::EMPTY,
        })?;
        let reference = value.as_obj().ok_or(FaultCode::MalformedState)?;
        self.vm.heap.set_frozen(reference);
        Ok(value)
    }

    fn syntax_view_class(
        class: lm_abi::syntax::SyntaxClass,
        node: u32,
        token: u32,
        trivia: u32,
    ) -> u32 {
        match class {
            lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid => node,
            lm_abi::syntax::SyntaxClass::Token => token,
            lm_abi::syntax::SyntaxClass::Trivia => trivia,
        }
    }

    /// Execute one public syntax instruction.
    #[inline(never)]
    fn exec_syntax(
        &mut self,
        instr: ExtendedInstr,
        tree: u32,
        node: u32,
        token: u32,
        trivia: u32,
        builder: u32,
    ) -> Result<(), FaultCode> {
        match instr {
            ExtendedInstr::SyntaxTreeRoot => {
                let tree_ref = self.pop_obj()?;
                let (source, records, text, data) = self.syntax_tree_parts(tree_ref, tree)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let root = view.record(view.root()).map_err(|_| FaultCode::BadCast)?;
                if !matches!(
                    root.class,
                    lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
                ) {
                    return Err(FaultCode::BadCast);
                }
                if root.lo != 0 || root.hi as usize != text.len() {
                    return Err(FaultCode::BadCast);
                }
                let value = self.alloc_syntax_view(node, source, records, view.root())?;
                self.push(value)?;
            }
            ExtendedInstr::SyntaxKind
            | ExtendedInstr::SyntaxCategory
            | ExtendedInstr::SyntaxRangeStart
            | ExtendedInstr::SyntaxRangeEnd
            | ExtendedInstr::SyntaxText => {
                let element = self.pop_obj()?;
                let (_, _, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                match instr {
                    ExtendedInstr::SyntaxKind => self.push(Value::Int(i64::from(record.kind)))?,
                    ExtendedInstr::SyntaxCategory => {
                        self.push(Value::Int(i64::from(record.class as u8)))?
                    }
                    ExtendedInstr::SyntaxRangeStart => {
                        self.push(Value::Int(i64::from(record.lo)))?
                    }
                    ExtendedInstr::SyntaxRangeEnd => self.push(Value::Int(i64::from(record.hi)))?,
                    ExtendedInstr::SyntaxText => {
                        let slice = text
                            .slice(record.lo as usize, record.hi as usize)
                            .ok_or(FaultCode::BadCast)?;
                        let value = self.alloc(Object::Substring(slice))?;
                        self.push(value)?;
                    }
                    _ => unreachable!("the syntax scalar dispatcher receives a scalar operation"),
                }
            }
            ExtendedInstr::SyntaxChildren => {
                let element = self.pop_obj()?;
                let (source, records, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                let mut descriptors = Vec::new();
                descriptors
                    .try_reserve_exact(record.child_len as usize)
                    .map_err(|_| FaultCode::HeapLimit)?;
                for offset in 0..record.child_len {
                    let index = view.child(record, offset).map_err(|_| FaultCode::BadCast)?;
                    let child = view.record(index).map_err(|_| FaultCode::BadCast)?;
                    descriptors.push((
                        Self::syntax_view_class(child.class, node, token, trivia),
                        index,
                    ));
                }
                let base = self.vm.operands.len();
                for (class, index) in descriptors {
                    let child = self.alloc_syntax_view(class, source, records, index)?;
                    self.push(child)?;
                }
                let items = self.vm.operands.split_off(base);
                let list = self.alloc(Object::List {
                    items,
                    epoch: StructuralEpoch::default(),
                })?;
                self.push(list)?;
            }
            ExtendedInstr::SyntaxDetach => {
                let element = self.pop_obj()?;
                let (_, _, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let detached = lm_abi::syntax::detach_syntax(data.as_slice(), text.len(), index)
                    .map_err(|_| FaultCode::BadCast)?;
                let source = text
                    .slice(detached.source_start as usize, detached.source_end as usize)
                    .ok_or(FaultCode::BadCast)?
                    .try_compact()
                    .map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&detached.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(detached.root).map_err(|_| FaultCode::BadCast)?;
                let class = Self::syntax_view_class(record.class, node, token, trivia);
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(class, source, records, detached.root)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxBuildToken | ExtendedInstr::SyntaxBuildTrivia => {
                let text_ref = self.pop_obj()?;
                let kind = u16::try_from(self.pop_int()?).map_err(|_| FaultCode::BadCast)?;
                let builder_ref = self.pop_obj()?;
                match self.vm.heap.get(builder_ref) {
                    Object::Instance { class, fields, .. }
                        if *class == builder && fields.is_empty() => {}
                    _ => return Err(BAD_TYPE),
                }
                let text = match self.vm.heap.get(text_ref) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let (class, syntax_class) = if matches!(instr, ExtendedInstr::SyntaxBuildToken) {
                    (token, lm_abi::syntax::SyntaxClass::Token)
                } else {
                    (trivia, lm_abi::syntax::SyntaxClass::Trivia)
                };
                let encoded = lm_abi::syntax::build_syntax_leaf(syntax_class, kind, text.as_str())
                    .map_err(|_| FaultCode::BadCast)?;
                let records =
                    SharedBytes::try_from_slice(&encoded).map_err(|_| FaultCode::HeapLimit)?;
                let source = Value::Obj(text_ref);
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(class, source, records, 0)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxBuildNode => {
                let children_ref = self.pop_obj()?;
                let kind = u16::try_from(self.pop_int()?).map_err(|_| FaultCode::BadCast)?;
                let builder_ref = self.pop_obj()?;
                match self.vm.heap.get(builder_ref) {
                    Object::Instance { class, fields, .. }
                        if *class == builder && fields.is_empty() => {}
                    _ => return Err(BAD_TYPE),
                }
                let child_values = match self.vm.heap.get(children_ref) {
                    Object::List { items, .. } => {
                        let mut copy = Vec::new();
                        copy.try_reserve_exact(items.len())
                            .map_err(|_| FaultCode::HeapLimit)?;
                        copy.extend_from_slice(items);
                        copy
                    }
                    _ => return Err(BAD_TYPE),
                };
                let mut owned = Vec::new();
                owned
                    .try_reserve_exact(child_values.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for child in child_values {
                    let child = child.as_obj().ok_or(BAD_TYPE)?;
                    let (_, _, source, records, index) =
                        self.syntax_element_parts(child, node, token, trivia)?;
                    owned.push((source, records, index));
                }
                let mut parts = Vec::new();
                parts
                    .try_reserve_exact(owned.len())
                    .map_err(|_| FaultCode::HeapLimit)?;
                for (source, records, index) in &owned {
                    parts.push(lm_abi::syntax::SyntaxPart {
                        source: source.as_str(),
                        records: records.as_slice(),
                        index: *index,
                    });
                }
                let built = lm_abi::syntax::build_syntax_node(kind, &parts)
                    .map_err(|_| FaultCode::BadCast)?;
                let source =
                    SharedText::try_from_string(built.source).map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&built.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let view = lm_abi::syntax::SyntaxView::new(records.as_slice(), source.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let root = view.root();
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_view(node, source, records, root)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            ExtendedInstr::SyntaxToTree => {
                let element = self.pop_obj()?;
                let (source, records, text, data, index) =
                    self.syntax_element_parts(element, node, token, trivia)?;
                let view = lm_abi::syntax::SyntaxView::new(data.as_slice(), text.len())
                    .map_err(|_| FaultCode::BadCast)?;
                let record = view.record(index).map_err(|_| FaultCode::BadCast)?;
                if !matches!(
                    record.class,
                    lm_abi::syntax::SyntaxClass::Node | lm_abi::syntax::SyntaxClass::Invalid
                ) {
                    return Err(FaultCode::BadCast);
                }
                if index == view.root() && record.lo == 0 && record.hi as usize == text.len() {
                    let value = self.alloc_syntax_tree(tree, source, records)?;
                    self.push(value)?;
                    return Ok(());
                }
                let detached = lm_abi::syntax::detach_syntax(data.as_slice(), text.len(), index)
                    .map_err(|_| FaultCode::BadCast)?;
                let source = text
                    .slice(detached.source_start as usize, detached.source_end as usize)
                    .ok_or(FaultCode::BadCast)?
                    .try_compact()
                    .map_err(|_| FaultCode::HeapLimit)?;
                let records = SharedBytes::try_from_slice(&detached.records)
                    .map_err(|_| FaultCode::HeapLimit)?;
                let source = self.alloc(Object::Str(source))?;
                self.push(source)?;
                let records = self.alloc(Object::Bytes(records))?;
                self.push(records)?;
                let value = self.alloc_syntax_tree(tree, source, records)?;
                self.vm.operands.truncate(self.vm.operands.len() - 2);
                self.push(value)?;
            }
            _ => unreachable!("the syntax dispatcher receives one syntax instruction"),
        }
        Ok(())
    }

    /// Execute one added instruction outside the base dispatch body.
    #[inline(never)]
    fn exec_extended(
        &mut self,
        module: &Module,
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        instr: ExtendedInstr,
    ) -> Result<ExecOutcome, FaultCode> {
        match instr {
            ExtendedInstr::MakeCallback { func, captures } => {
                self.exec_make_callback(func, captures)?;
            }
            ExtendedInstr::AsCallback => self.exec_as_callback()?,
            ExtendedInstr::OptionSome { .. } => {
                // The payload already has the native `Some` representation.
            }
            ExtendedInstr::OptionNone { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::OptionNone(ty))?;
            }
            ExtendedInstr::OptionPayload { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::OptionPayload(ty))?;
            }
            ExtendedInstr::ListGet { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::ListGet(ty))?;
            }
            ExtendedInstr::MapGet { ty } => {
                self.exec_option_collection(module, envs, OptionCollectionOp::MapGet(ty))?;
            }
            ExtendedInstr::ListEpoch => {
                self.exec_collection_iteration(CollectionIterationOp::ListEpoch)?;
            }
            ExtendedInstr::ListIterLen => {
                self.exec_collection_iteration(CollectionIterationOp::ListIterLen)?;
            }
            ExtendedInstr::MapEpoch => {
                self.exec_collection_iteration(CollectionIterationOp::MapEpoch)?;
            }
            ExtendedInstr::MapIterLen => {
                self.exec_collection_iteration(CollectionIterationOp::MapIterLen)?;
            }
            ExtendedInstr::MapKeyAt => {
                self.exec_collection_iteration(CollectionIterationOp::MapEntry { value: false })?;
            }
            ExtendedInstr::MapValueAt => {
                self.exec_collection_iteration(CollectionIterationOp::MapEntry { value: true })?;
            }
            ExtendedInstr::ListCapacity => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListCapacity)?;
            }
            ExtendedInstr::ListSet => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListSet)?;
            }
            ExtendedInstr::ListPop { ty } => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListPop(ty))?;
            }
            ExtendedInstr::ListInsert => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListInsert)?;
            }
            ExtendedInstr::ListRemove => {
                self.exec_collection_extension(
                    module,
                    envs,
                    CollectionExtensionOp::ListRemove { swap: false },
                )?;
            }
            ExtendedInstr::ListSwapRemove => {
                self.exec_collection_extension(
                    module,
                    envs,
                    CollectionExtensionOp::ListRemove { swap: true },
                )?;
            }
            ExtendedInstr::ListReserve => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListReserve)?;
            }
            ExtendedInstr::ListTruncate => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListTruncate)?;
            }
            ExtendedInstr::ListContains => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListContains)?;
            }
            ExtendedInstr::ListReorder => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::ListReorder)?;
            }
            ExtendedInstr::MapRemove { ty } => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapRemove(ty))?;
            }
            ExtendedInstr::MapClear => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapClear)?;
            }
            ExtendedInstr::MapReserve => {
                self.exec_collection_extension(module, envs, CollectionExtensionOp::MapReserve)?;
            }
            ExtendedInstr::CallSlot { slot, app } => {
                let target = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Function(target)) => *target,
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                if app == lm_bytecode::NO_APP {
                    let argc = module
                        .funcs
                        .get(target as usize)
                        .ok_or(BAD_STATE)?
                        .params
                        .len();
                    self.push_frame(module, target, argc, None, TypeEnvId::EMPTY)?;
                } else {
                    self.call_generic(module, envs, target, app)?;
                }
            }
            ExtendedInstr::NewSlot { slot, app } => {
                let constructor = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Class { constructor, .. }) => *constructor,
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                if app == lm_bytecode::NO_APP {
                    let argc = module
                        .funcs
                        .get(constructor as usize)
                        .ok_or(BAD_STATE)?
                        .params
                        .len();
                    self.push_frame(module, constructor, argc, None, TypeEnvId::EMPTY)?;
                } else {
                    self.call_generic(module, envs, constructor, app)?;
                }
            }
            ExtendedInstr::LoadSlot { slot } => {
                match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Value(_)) => return Ok(ExecOutcome::LoadSlot { slot }),
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                }
            }
            ExtendedInstr::SendSlot { slot } => {
                let (proc, generation) = match slots.and_then(|slots| slots.get(slot as usize)) {
                    Some(ImageSlotTarget::Process { proc, generation }) => (*proc, *generation),
                    Some(ImageSlotTarget::Empty) => return Err(FaultCode::InvalidVmState),
                    _ => return Err(BAD_STATE),
                };
                let message = self.pop()?;
                let handle = self.alloc(Object::NativeHandle { proc, generation })?;
                return Ok(ExecOutcome::Perform {
                    op: lm_abi::OP_PROC_SEND,
                    args: vec![handle, message],
                });
            }
            ExtendedInstr::SyntaxTreeRoot
            | ExtendedInstr::SyntaxKind
            | ExtendedInstr::SyntaxCategory
            | ExtendedInstr::SyntaxRangeStart
            | ExtendedInstr::SyntaxRangeEnd
            | ExtendedInstr::SyntaxText
            | ExtendedInstr::SyntaxChildren
            | ExtendedInstr::SyntaxDetach
            | ExtendedInstr::SyntaxBuildToken
            | ExtendedInstr::SyntaxBuildTrivia
            | ExtendedInstr::SyntaxBuildNode
            | ExtendedInstr::SyntaxToTree => {
                let tree = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TREE];
                let node = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_NODE];
                let token = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TOKEN];
                let trivia = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_TRIVIA];
                let builder = module.core_roles[lm_bytecode::corepin::ROLE_SYNTAX_BUILDER];
                self.exec_syntax(instr, tree, node, token, trivia, builder)?;
            }
            ExtendedInstr::DynPack { ty } => {
                let closed = envs
                    .close(module, ty, self.frame_env())
                    .map_err(env_fault)?;
                let value = self.pop()?;
                let package = self.alloc(Object::DynValue { value, ty: closed })?;
                self.push(package)?;
            }
            ExtendedInstr::DynRender => {
                let package = self.pop_obj()?;
                let (value, ty) = match self.vm.heap.get(package) {
                    Object::DynValue { value, ty } => (*value, *ty),
                    _ => return Err(BAD_TYPE),
                };
                return Ok(ExecOutcome::DynamicRender { value, ty });
            }
        }
        Ok(ExecOutcome::Continue)
    }

    /// Execute one fetched instruction of the current frame.
    ///
    /// `envs` is the type environment table of the world. A
    /// monomorphic instruction never reads it, so a monomorphic
    /// program performs no type work.
    #[inline(always)]
    fn exec_instr(
        &mut self,
        module: &Module,
        dispatch: &[crate::DispatchRow],
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        instr: Instr,
    ) -> Result<ExecOutcome, FaultCode> {
        if matches!(instr, Instr::Native(_)) {
            self.exec_native_instr(instr)?;
            return Ok(ExecOutcome::Continue);
        }
        match instr {
            Instr::ConstUnit => self.push(Value::Unit)?,
            Instr::ConstBool(v) => self.push(Value::Bool(v))?,
            Instr::ConstInt(v) => self.push(Value::Int(v))?,
            Instr::ConstStr(idx) => {
                // Literal strings intern per machine: the first load
                // allocates one frozen object, and every later load
                // reuses it. Literals are collection roots.
                let idx = idx as usize;
                if self.vm.literals.len() <= idx {
                    self.vm.literals.resize(idx + 1, None);
                }
                let value = match self.vm.literals[idx] {
                    Some(r) => Value::Obj(r),
                    None => {
                        let text = module.strings[idx].clone();
                        let value = self.alloc(Object::Str(text.into()))?;
                        if let Value::Obj(r) = value {
                            self.vm.literals[idx] = Some(r);
                        }
                        value
                    }
                };
                self.push(value)?;
            }
            Instr::LoadLocal(slot) => {
                let at = self.local_at(slot)?;
                let value = *self.vm.locals.get(at).ok_or(BAD_STATE)?;
                self.push(value)?;
            }
            Instr::StoreLocal(slot) => {
                let value = self.pop()?;
                let at = self.local_at(slot)?;
                *self.vm.locals.get_mut(at).ok_or(BAD_STATE)? = value;
            }
            Instr::Pop => {
                self.pop()?;
            }
            Instr::Add => self.int_binary(i64::checked_add)?,
            Instr::Sub => self.int_binary(i64::checked_sub)?,
            Instr::Mul => self.int_binary(i64::checked_mul)?,
            Instr::Div => {
                let (at, a, b) = self.int_pair()?;
                if b == 0 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::DivideByZero);
                }
                if a == i64::MIN && b == -1 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                }
                self.replace_pair(at, Value::Int(a / b));
            }
            Instr::Rem => {
                let (at, a, b) = self.int_pair()?;
                if b == 0 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::DivideByZero);
                }
                if a == i64::MIN && b == -1 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                }
                self.replace_pair(at, Value::Int(a % b));
            }
            Instr::Neg => {
                let a = self.pop_int()?;
                let value = a.checked_neg().ok_or(FaultCode::IntegerOverflow)?;
                self.push(Value::Int(value))?;
            }
            Instr::Not => {
                let a = self.pop_bool()?;
                self.push(Value::Bool(!a))?;
            }
            Instr::LtInt => self.int_compare(|a, b| a < b)?,
            Instr::LeInt => self.int_compare(|a, b| a <= b)?,
            Instr::GtInt => self.int_compare(|a, b| a > b)?,
            Instr::GeInt => self.int_compare(|a, b| a >= b)?,
            Instr::EqInt => self.int_compare(|a, b| a == b)?,
            Instr::NeInt => self.int_compare(|a, b| a != b)?,
            Instr::EqBool => {
                let b = self.pop_bool()?;
                let a = self.pop_bool()?;
                self.push(Value::Bool(a == b))?;
            }
            Instr::NeBool => {
                let b = self.pop_bool()?;
                let a = self.pop_bool()?;
                self.push(Value::Bool(a != b))?;
            }
            Instr::Jump(target) => {
                let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                frame.block = target;
                frame.ip = 0;
            }
            Instr::JumpIfFalse(target) => {
                if !self.pop_bool()? {
                    let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                    frame.block = target;
                    frame.ip = 0;
                }
            }
            Instr::JumpIfTrue(target) => {
                if self.pop_bool()? {
                    let frame = self.vm.frames.last_mut().ok_or(BAD_STATE)?;
                    frame.block = target;
                    frame.ip = 0;
                }
            }
            Instr::Native(_) => unreachable!("native instructions return before dispatch"),
            Instr::EqValue | Instr::NeValue => {
                let b = self.pop()?;
                let a = self.pop()?;
                let equal = self.values_equal(module, a, b)?;
                let want = matches!(instr, Instr::EqValue);
                self.push(Value::Bool(equal == want))?;
            }
            Instr::CallInterface {
                interface,
                method,
                recv_ty,
            } => {
                self.exec_interface_call(module, dispatch, envs, interface, method, recv_ty)?;
            }
            Instr::Extended(instr) => {
                let outcome = self.exec_extended(module, envs, slots, instr)?;
                if !matches!(outcome, ExecOutcome::Continue) {
                    return Ok(outcome);
                }
            }
            Instr::EqRef => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                self.push(Value::Bool(self.references_equal(module, a, b)))?;
            }
            Instr::NeRef => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                self.push(Value::Bool(!self.references_equal(module, a, b)))?;
            }
            // A direct call of a non-generic function copies the empty
            // environment, so it allocates nothing and reads no table.
            Instr::Call(callee) => {
                let argc = module.funcs[callee as usize].params.len();
                self.push_frame(module, callee, argc, None, TypeEnvId::EMPTY)?;
            }
            // A generic call derives one environment from the caller
            // environment and the application of the call site. The
            // table caches the pair, so a repeated call reuses one
            // index.
            Instr::CallG { func: callee, app } => {
                self.call_generic(module, envs, callee, app)?;
            }
            Instr::CallVirtual { selector, argc } => {
                let argc = argc as usize;
                let recv = self.peek(argc)?;
                let class = self.virtual_class(module, recv)?;
                let target = method_of(dispatch, class, selector)?;
                self.push_frame(module, target, argc + 1, None, TypeEnvId::EMPTY)?;
            }
            // A generic virtual call binds the receiver class
            // arguments first and the own arguments of the method
            // after them. The receiver object carries its class
            // arguments, so the runtime reads them from the value it
            // dispatched on.
            Instr::CallVirtualG {
                selector,
                argc,
                app,
            } => {
                self.call_virtual_generic(module, dispatch, envs, selector, argc, app)?;
            }
            // A closure call installs the environment the creator
            // frame held. The call site applies no type argument, so
            // the closure value is the only evidence.
            Instr::CallValue { argc } => {
                let argc = argc as usize;
                let callee_pos = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc + 1)
                    .ok_or(BAD_STATE)?;
                let callee = self.vm.operands.remove(callee_pos);
                let (target, env, capture) = match callee {
                    Value::Obj(reference) => match self.vm.heap.get(reference) {
                        Object::Closure { func, env, .. } => {
                            (*func, env.env(), FrameCapture::Closure(reference))
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    Value::Callback(reference) => {
                        let descriptor = self.callback(reference)?;
                        (
                            descriptor.func,
                            descriptor.env,
                            FrameCapture::Callback(reference),
                        )
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push_frame(module, target, argc, Some(capture), env)?;
            }
            // The closure retains the environment of the frame that
            // built it. Capture cannot rebuild it later, because the
            // closure outlives that frame.
            Instr::MakeClosure { func, captures } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(captures as usize)
                    .ok_or(BAD_STATE)?;
                let captured: Vec<Value> = self.vm.operands.split_off(split);
                let env = Witness(self.frame_env());
                let value = self.alloc(Object::Closure {
                    func,
                    captures: captured,
                    env,
                })?;
                self.push(value)?;
            }
            Instr::LoadCapture(idx) => {
                let frame = self.vm.frames.last().ok_or(BAD_STATE)?;
                let closure = frame.closure.ok_or(BAD_STATE)?;
                let value = match closure {
                    FrameCapture::Closure(reference) => match self.vm.heap.get(reference) {
                        Object::Closure { captures, .. } => {
                            *captures.get(idx as usize).ok_or(BAD_TYPE)?
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    FrameCapture::Callback(reference) => *self
                        .callback(reference)?
                        .captures
                        .get(idx as usize)
                        .ok_or(BAD_TYPE)?,
                };
                self.push(value)?;
            }
            // A plain class takes no type argument, so the instance
            // records the empty environment and allocates nothing.
            Instr::New(class) => {
                let field_count = module.classes[class as usize].fields.len();
                let value = self.alloc(Object::Instance {
                    class,
                    fields: vec![Value::Uninit; field_count],
                    env: Witness::EMPTY,
                })?;
                self.push(value)?;
            }
            // A generic instance records its own class arguments, so a
            // later dispatch and a later reflection query read them
            // from the object itself.
            Instr::NewG { class, app } => {
                let value = self.new_generic(module, envs, class, app)?;
                self.push(value)?;
            }
            Instr::TupleNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(count as usize)
                    .ok_or(BAD_STATE)?;
                let items: Vec<Value> = self.vm.operands.split_off(split);
                let value = self.alloc(Object::Tuple { items })?;
                self.push(value)?;
            }
            Instr::TupleGet(index) => {
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::Tuple { items } => *items.get(index as usize).ok_or(BAD_TYPE)?,
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            Instr::IsType(ty) => {
                let value = self.pop()?;
                let matches = self.value_matches_class(module, envs, value, ty)?;
                self.push(Value::Bool(matches))?;
            }
            Instr::CastType(ty) => {
                let value = self.pop()?;
                if !self.value_matches_class(module, envs, value, ty)? {
                    return Err(FaultCode::BadCast);
                }
                self.push(value)?;
            }
            Instr::LoadField(field) => {
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::Instance { fields, .. } => {
                        *fields.get(field as usize).ok_or(BAD_TYPE)?
                    }
                    _ => return Err(BAD_TYPE),
                };
                if value == Value::Uninit {
                    return Err(FaultCode::UninitializedField);
                }
                self.push(value)?;
            }
            Instr::StoreField(field) => {
                let value = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                match self.vm.heap.get_mut(r) {
                    Object::Instance { fields, .. } => {
                        *fields.get_mut(field as usize).ok_or(BAD_TYPE)? = value;
                    }
                    _ => return Err(BAD_TYPE),
                }
            }
            Instr::ListNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(count as usize)
                    .ok_or(BAD_STATE)?;
                let items: Vec<Value> = self.vm.operands.split_off(split);
                let value = self.alloc(Object::List {
                    items,
                    epoch: StructuralEpoch::default(),
                })?;
                self.push(value)?;
            }
            Instr::ListLen => {
                let r = self.pop_obj()?;
                let len = match self.vm.heap.get(r) {
                    Object::List { items, .. } => items.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::ListAt => {
                self.exec_list_at()?;
            }
            Instr::ListPush => {
                let value = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.vm.heap.get_mut(r) {
                    Object::List { items, epoch } => {
                        epoch.bump()?;
                        items.push(value);
                    }
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(r);
                self.push(Value::Unit)?;
            }
            Instr::MapNew { count, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(2 * count as usize)
                    .ok_or(BAD_STATE)?;
                let flat: Vec<Value> = self.vm.operands.split_off(split);
                let mut entries: Vec<(Value, Value)> = Vec::new();
                let mut index = MapIndex::default();
                for pair in flat.chunks_exact(2) {
                    let (key, value) = (pair[0], pair[1]);
                    let hash = self.key_hash(key);
                    let hit = index
                        .candidates(hash)
                        .find(|i| self.key_eq(entries[*i as usize].0, key));
                    match hit {
                        Some(pos) => entries[pos as usize].1 = value,
                        None => {
                            index.insert(hash, entries.len() as u32);
                            entries.push((key, value));
                        }
                    }
                }
                let value = self.alloc(Object::Map { entries, index })?;
                self.push(value)?;
            }
            Instr::MapLen => {
                let r = self.pop_obj()?;
                let len = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } => entries.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::MapHas => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let found = self.map_lookup(r, key)?.is_some();
                self.push(Value::Bool(found))?;
            }
            Instr::MapAt => {
                let key = self.pop()?;
                let r = self.pop_obj()?;
                let pos = match self.map_lookup(r, key)? {
                    Some(pos) => pos,
                    None => return Err(FaultCode::MissingKey),
                };
                let value = match self.vm.heap.get(r) {
                    Object::Map { entries, .. } => entries.get(pos).ok_or(BAD_STATE)?.1,
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            Instr::MapPut { ty, discard } => {
                self.exec_map_put(module, envs, ty, discard)?;
            }
            Instr::Freeze => {
                let r = self.pop_obj()?;
                // The freeze mode validates the whole reachable graph
                // against its limits before any bit goes on, so a
                // rejected freeze changes nothing.
                lm_graph::freeze(&mut self.vm.heap, r, &self.config.graph)?;
                self.push(Value::Obj(r))?;
            }
            Instr::Digest { ty } => {
                let env = self.frame_env();
                let value = self.pop_obj()?;
                return Ok(ExecOutcome::Digest { value, ty, env });
            }
            Instr::EqDigest | Instr::NeDigest => {
                let b = self.pop_obj()?;
                let a = self.pop_obj()?;
                // A digest compares by value, never by reference
                // (specification 6.4).
                let equal = match (self.vm.heap.get(a), self.vm.heap.get(b)) {
                    (Object::NativeDigest(x), Object::NativeDigest(y)) => x == y,
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Bool(equal == matches!(instr, Instr::EqDigest)))?;
            }
            Instr::Return => {
                let value = self.pop()?;
                let frame = self.vm.frames.pop().ok_or(BAD_STATE)?;
                self.vm.operands.truncate(frame.base_operand as usize);
                self.vm.locals.truncate(frame.base_local as usize);
                if self.vm.frames.is_empty() {
                    if !self.callbacks.is_empty() {
                        self.collect_callbacks();
                    }
                    return Ok(ExecOutcome::Terminal(value));
                }
                self.push(value)?;
                if !self.callbacks.is_empty() {
                    self.collect_callbacks();
                }
            }
            Instr::Unreachable => {
                return Err(FaultCode::UnreachableCode);
            }
            Instr::Perform { op, argc, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc as usize)
                    .ok_or(BAD_STATE)?;
                let args = self.vm.operands.split_off(split);
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::PerformValue { argc, .. } => {
                let split = self
                    .vm
                    .operands
                    .len()
                    .checked_sub(argc as usize)
                    .ok_or(BAD_STATE)?;
                let args = self.vm.operands.split_off(split);
                let callee = self.pop()?;
                let op = match callee {
                    Value::Op(op) => op,
                    _ => return Err(BAD_TYPE),
                };
                return Ok(ExecOutcome::Perform { op, args });
            }
            Instr::OpConst(op) => {
                self.push(Value::Op(op))?;
            }
            Instr::TableEdit { action, kind, slot } => {
                let mock = if action == 2 { Some(self.pop()?) } else { None };
                let table = self.pop_obj()?;
                return Ok(ExecOutcome::TableEdit {
                    table,
                    action,
                    kind,
                    slot,
                    mock,
                });
            }
            Instr::AsCall { op, ty } => {
                let request = self.pop_obj()?;
                return Ok(ExecOutcome::AsCall {
                    request,
                    op,
                    ty,
                    env: self.frame_env(),
                });
            }
            Instr::CallArgs => {
                let call = self.pop_obj()?;
                return Ok(ExecOutcome::CallArgs { call });
            }
            Instr::FaultCode => {
                let r = self.pop_obj()?;
                let code = match self.vm.heap.get(r) {
                    Object::NativeFault { code, .. } => *code,
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Str(code.to_string().into()))?;
                self.push(value)?;
            }
            Instr::RequestOp => {
                let request = self.pop_obj()?;
                return Ok(ExecOutcome::RequestOp { request });
            }
            Instr::FaultDenied => {
                let r = self.pop_obj()?;
                let reason = match self.vm.heap.get(r) {
                    Object::Str(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                // The code is fixed. A holder states why it denied
                // the request, and it cannot name another code.
                let value = self.alloc(Object::NativeFault {
                    code: FaultCode::PolicyDenied,
                    message: reason.to_string(),
                    op: None,
                })?;
                self.push(value)?;
            }
        }
        Ok(ExecOutcome::Continue)
    }

    /// Execute until a boundary or an instruction count expires.
    ///
    /// `None` means the count expired after `retired` instructions.
    /// A boundary result includes the instruction that produced it.
    pub fn exec_for_quantum(
        &mut self,
        module: &Module,
        dispatch: &[crate::DispatchRow],
        envs: &mut TypeEnvs,
        slots: Option<&[ImageSlotTarget]>,
        limit: u32,
    ) -> (Result<Option<ExecOutcome>, FaultCode>, u32) {
        debug_assert!(limit > 0);
        let original_fuel = self.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(limit));
        let held_fuel = original_fuel - batch_fuel;
        let count_expiry = u64::from(limit) <= original_fuel;
        self.vm.fuel = batch_fuel;

        // Verification bounds every function, block, branch, and instruction.
        // Snapshot admission applies the same bounds to restored frames.
        // Keep the current block until a call or branch changes it.
        let mut cached_func = u32::MAX;
        let mut cached_block = u32::MAX;
        let mut code: &[Instr] = &[];
        let outcome = loop {
            if self.vm.fuel == 0 {
                break Err(FaultCode::OutOfFuel);
            }
            self.vm.fuel -= 1;
            let Some(frame) = self.vm.frames.last() else {
                break Err(BAD_STATE);
            };
            let (func, block, ip) = (frame.func, frame.block, frame.ip);
            if func != cached_func || block != cached_block {
                code = &module.funcs[func as usize].blocks[block as usize];
                cached_func = func;
                cached_block = block;
            }
            let instr = code[ip as usize];
            let Some(frame) = self.vm.frames.last_mut() else {
                break Err(BAD_STATE);
            };
            frame.ip += 1;
            match self.exec_instr(module, dispatch, envs, slots, instr) {
                Ok(ExecOutcome::Continue) => {}
                outcome => break outcome,
            }
        };
        let retired = u32::try_from(batch_fuel - self.vm.fuel)
            .expect("one execution batch retires at most its u32 limit");
        self.vm.fuel += held_fuel;

        match outcome {
            Err(FaultCode::OutOfFuel) if count_expiry => (Ok(None), retired),
            Ok(outcome) => (Ok(Some(outcome)), retired),
            Err(code) => (Err(code), retired),
        }
    }

    /// Close an `Option` family or arm type to its family type.
    fn close_option_family(
        &self,
        module: &Module,
        envs: &mut TypeEnvs,
        ty: u32,
    ) -> Result<ClosedTypeId, FaultCode> {
        let closed = envs
            .close(module, ty, self.frame_env())
            .map_err(env_fault)?;
        let (class, argument) = match envs.ty(closed) {
            Some(ClosedType::Inst(class, args)) if args.len() == 1 => (*class, args[0]),
            _ => return Err(BAD_STATE),
        };
        let option = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
        let some = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME];
        let none = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE];
        if option == lm_bytecode::NO_ROLE {
            return Err(BAD_STATE);
        }
        if class == option {
            return Ok(closed);
        }
        if class != some && class != none {
            return Err(BAD_STATE);
        }
        envs.intern(ClosedType::Inst(option, vec![argument]))
            .map_err(env_fault)
    }

    /// Test one value against a class type.
    fn value_matches_class(
        &self,
        module: &Module,
        envs: &mut TypeEnvs,
        value: Value,
        ty: u32,
    ) -> Result<bool, FaultCode> {
        let target = match module.types.get(ty as usize).ok_or(BAD_STATE)? {
            lm_bytecode::BcType::Class(class) | lm_bytecode::BcType::Inst(class, _) => *class,
            _ => return Err(BAD_STATE),
        };
        let option = module.core_roles[lm_bytecode::corepin::ROLE_OPTION];
        let some = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_SOME];
        let none = module.core_roles[lm_bytecode::corepin::ROLE_OPTION_NONE];
        if target == option || target == some || target == none {
            let family = self.close_option_family(module, envs, ty)?;
            let is_none = matches!(
                value,
                Value::EmptyCase { ty, arm: 1 } if ty == family
            );
            return Ok(target == option || (target == none) == is_none);
        }
        let r = value.as_obj().ok_or(BAD_TYPE)?;
        self.instance_matches(module, r, ty)
    }

    /// Return true when the instance class equals or extends the target.
    fn instance_matches(&self, module: &Module, r: ObjRef, ty: u32) -> Result<bool, FaultCode> {
        let target = match module.types.get(ty as usize).ok_or(BAD_STATE)? {
            lm_bytecode::BcType::Class(c) | lm_bytecode::BcType::Inst(c, _) => *c,
            _ => return Err(BAD_STATE),
        };
        let mut class = self.virtual_class(module, Value::Obj(r))?;
        // The class chain of a verified module is acyclic, and the
        // step bound holds whatever built the state, so the walk never
        // spins on a hand-built table.
        for _ in 0..=module.classes.len() {
            if class == target {
                return Ok(true);
            }
            match module.classes.get(class as usize).and_then(|c| c.parent()) {
                Some(p) => class = p,
                None => return Ok(false),
            }
        }
        Err(BAD_STATE)
    }

    /// Append text to a string builder with a growth reservation.
    fn sb_append(&mut self, sb: ObjRef, text: &str) -> Result<(), FaultCode> {
        let growth = match self.vm.heap.get(sb) {
            Object::StrBuilder(buf) => buf.reserve_growth(text.len()),
            _ => return Err(BAD_TYPE),
        }
        .ok_or(FaultCode::InvalidVmState)?;
        if growth != 0 {
            self.reserve(growth, &[Value::Obj(sb)])?;
        }
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => {
                if growth != 0
                    && !buf
                        .try_reserve(text.len())
                        .map_err(|_| FaultCode::HeapLimit)?
                {
                    return Err(FaultCode::InvalidVmState);
                }
                if !buf.append_str(text) {
                    return Err(FaultCode::InvalidVmState);
                }
            }
            _ => return Err(BAD_TYPE),
        }
        if growth != 0 {
            self.vm.heap.recharge_local(sb);
        }
        self.push(Value::Obj(sb))
    }

    /// Append one integer without a temporary string allocation.
    fn sb_append_int(&mut self, sb: ObjRef, value: i64) -> Result<(), FaultCode> {
        let length = integer_text_len(value);
        let growth = match self.vm.heap.get(sb) {
            Object::StrBuilder(buf) => buf.reserve_growth(length),
            _ => return Err(BAD_TYPE),
        }
        .ok_or(FaultCode::InvalidVmState)?;
        if growth != 0 {
            self.reserve(growth, &[Value::Obj(sb)])?;
        }
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => {
                if growth != 0 && !buf.try_reserve(length).map_err(|_| FaultCode::HeapLimit)? {
                    return Err(FaultCode::InvalidVmState);
                }
                if !buf.append_int(value) {
                    return Err(FaultCode::InvalidVmState);
                }
            }
            _ => return Err(BAD_TYPE),
        }
        if growth != 0 {
            self.vm.heap.recharge_local(sb);
        }
        self.push(Value::Obj(sb))
    }

    /// Create one machine-local callback descriptor.
    fn alloc_callback(
        &mut self,
        func: u32,
        captures: Vec<Value>,
        env: TypeEnvId,
    ) -> Result<Value, FaultCode> {
        let owner_depth = u32::try_from(self.vm.frames.len()).map_err(|_| FaultCode::StackLimit)?;
        let descriptor = CallbackDescriptor {
            func,
            captures,
            env,
            owner_depth,
        };
        if let Some((slot, entry)) = self
            .callbacks
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.descriptor.is_none())
        {
            entry.descriptor = Some(descriptor);
            return Ok(Value::Callback(CallbackRef {
                slot: slot as u32,
                generation: entry.generation,
            }));
        }
        self.callbacks
            .try_reserve(1)
            .map_err(|_| FaultCode::StackLimit)?;
        let slot = self.callbacks.len() as u32;
        self.callbacks.push(CallbackSlot {
            generation: 0,
            descriptor: Some(descriptor),
        });
        Ok(Value::Callback(CallbackRef {
            slot,
            generation: 0,
        }))
    }

    /// Resolve one callback reference.
    pub(crate) fn callback(
        &self,
        reference: CallbackRef,
    ) -> Result<&CallbackDescriptor, FaultCode> {
        let slot = self
            .callbacks
            .get(reference.slot as usize)
            .ok_or(BAD_TYPE)?;
        if slot.generation != reference.generation {
            return Err(BAD_TYPE);
        }
        slot.descriptor.as_ref().ok_or(BAD_TYPE)
    }

    /// Release callbacks that cannot remain after one frame return.
    fn collect_callbacks(&mut self) {
        let depth = self.vm.frames.len() as u32;
        if !self.callbacks.iter().any(|slot| {
            slot.descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.owner_depth >= depth)
        }) {
            return;
        }
        let mut marked = vec![false; self.callbacks.len()];
        let mut work = Vec::new();
        let mut mark_value = |value: Value| {
            if let Value::Callback(reference) = value {
                work.push(reference);
            }
        };
        for value in self.vm.locals.iter().chain(self.vm.operands.iter()) {
            mark_value(*value);
        }
        for frame in &self.vm.frames {
            if let Some(FrameCapture::Callback(reference)) = frame.closure {
                mark_value(Value::Callback(reference));
            }
        }
        while let Some(reference) = work.pop() {
            let Some(slot) = self.callbacks.get(reference.slot as usize) else {
                continue;
            };
            if slot.generation != reference.generation
                || marked.get(reference.slot as usize).copied().unwrap_or(true)
            {
                continue;
            }
            marked[reference.slot as usize] = true;
            if let Some(descriptor) = &slot.descriptor {
                for value in &descriptor.captures {
                    if let Value::Callback(child) = value {
                        work.push(*child);
                    }
                }
            }
        }
        for (index, slot) in self.callbacks.iter_mut().enumerate() {
            let candidate = slot
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.owner_depth >= depth);
            if candidate && !marked[index] {
                slot.descriptor = None;
                slot.generation = slot.generation.wrapping_add(1);
            }
        }
    }

    /// Push a frame. The top `consume` operand values become the first
    /// local slots in order. `closure` supplies capture context for a
    /// closure call.
    fn push_frame(
        &mut self,
        module: &Module,
        callee: u32,
        consume: usize,
        closure: Option<FrameCapture>,
        env: TypeEnvId,
    ) -> Result<(), FaultCode> {
        if self.vm.frames.len() as u32 >= self.config.max_frames {
            return Err(FaultCode::StackLimit);
        }
        let func = module.funcs.get(callee as usize).ok_or(BAD_STATE)?;
        let base_local = self.vm.locals.len() as u32;
        let arg_start = self
            .vm
            .operands
            .len()
            .checked_sub(consume)
            .ok_or(BAD_STATE)?;
        let new_locals = self.vm.locals.len() + func.local_count() as usize;
        if new_locals + self.vm.operands.len() > self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.vm
            .locals
            .extend_from_slice(&self.vm.operands[arg_start..]);
        self.vm.operands.truncate(arg_start);
        // The slots after the parameters start without a value. The
        // marker states that fact: an uninitialized slot is not a unit
        // value, and a snapshot keeps the two apart.
        self.vm.locals.resize(new_locals, Value::Uninit);
        let base_operand = self.vm.operands.len() as u32;
        self.vm.frames.push(Frame {
            func: callee,
            block: 0,
            ip: 0,
            base_local,
            base_operand,
            closure,
            env,
        });
        Ok(())
    }

    pub fn push(&mut self, value: Value) -> Result<(), FaultCode> {
        if self.vm.operands.len() + self.vm.locals.len() >= self.config.max_stack_values as usize {
            return Err(FaultCode::StackLimit);
        }
        self.vm.operands.push(value);
        Ok(())
    }

    /// The arena position of one local slot of the running frame.
    ///
    /// The frame states its own local base, so a restored machine can
    /// state one the arena does not hold. The caller reads the arena
    /// through `get`, so the one bounds test of the slice answers the
    /// position as well.
    #[inline]
    fn local_at(&self, slot: u32) -> Result<usize, FaultCode> {
        let base = self.vm.frames.last().ok_or(BAD_STATE)?.base_local;
        Ok(base as usize + slot as usize)
    }

    /// The operand `back` places below the top of the stack.
    #[inline]
    fn peek(&self, back: usize) -> Result<Value, FaultCode> {
        let at = self
            .vm
            .operands
            .len()
            .checked_sub(back + 1)
            .ok_or(BAD_STATE)?;
        Ok(self.vm.operands[at])
    }

    /// Take the top operand.
    ///
    /// The independent verifier proves the operand type at every
    /// program point of every executed function, so verified code
    /// never reaches the error arm of this call or of the readers
    /// below.
    ///
    /// A restored machine states its own operand arena. Admission
    /// proves the structure of that arena and no type of it, so the
    /// readers test the tag and raise `TypeMismatch`. A short stack
    /// raises `MalformedState`. Both stop the machine and leave the
    /// host running.
    #[inline]
    fn pop(&mut self) -> Result<Value, FaultCode> {
        self.vm.operands.pop().ok_or(BAD_STATE)
    }

    #[inline]
    fn pop_int(&mut self) -> Result<i64, FaultCode> {
        match self.pop()? {
            Value::Int(v) => Ok(v),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    fn pop_bool(&mut self) -> Result<bool, FaultCode> {
        match self.pop()? {
            Value::Bool(v) => Ok(v),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    fn pop_char(&mut self) -> Result<char, FaultCode> {
        match self.pop()? {
            Value::Char(value) => Ok(value),
            _ => Err(BAD_TYPE),
        }
    }

    #[inline]
    fn pop_obj(&mut self) -> Result<ObjRef, FaultCode> {
        match self.pop()? {
            Value::Obj(r) => Ok(r),
            _ => Err(BAD_TYPE),
        }
    }

    /// Get immutable text from a String or Substring object.
    fn text_value(&self, reference: ObjRef) -> Result<&SharedText, FaultCode> {
        match self.vm.heap.get(reference) {
            Object::Str(text) | Object::Substring(text) => Ok(text),
            _ => Err(BAD_TYPE),
        }
    }

    /// Read two integer operands and preserve successful input.
    ///
    /// An error consumes the same operands as two ordered `pop_int`
    /// calls. A successful caller replaces both operands in place.
    #[inline(always)]
    fn int_pair(&mut self) -> Result<(usize, i64, i64), FaultCode> {
        let len = self.vm.operands.len();
        if len < 2 {
            return self.short_int_pair();
        }
        let b = match self.vm.operands[len - 1] {
            Value::Int(value) => value,
            _ => {
                self.vm.operands.truncate(len - 1);
                return Err(BAD_TYPE);
            }
        };
        let at = len - 2;
        let a = match self.vm.operands[at] {
            Value::Int(value) => value,
            _ => {
                self.vm.operands.truncate(at);
                return Err(BAD_TYPE);
            }
        };
        Ok((at, a, b))
    }

    #[cold]
    #[inline(never)]
    fn short_int_pair(&mut self) -> Result<(usize, i64, i64), FaultCode> {
        match self.vm.operands.pop() {
            None | Some(Value::Int(_)) => Err(BAD_STATE),
            Some(_) => Err(BAD_TYPE),
        }
    }

    #[inline(always)]
    fn replace_pair(&mut self, at: usize, value: Value) {
        self.vm.operands[at] = value;
        self.vm.operands.truncate(at + 1);
    }

    #[inline(always)]
    fn int_binary(&mut self, op: impl Fn(i64, i64) -> Option<i64>) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        let Some(value) = op(a, b) else {
            self.vm.operands.truncate(at);
            return Err(FaultCode::IntegerOverflow);
        };
        self.replace_pair(at, Value::Int(value));
        Ok(())
    }

    #[inline(always)]
    fn int_compare(&mut self, op: impl Fn(i64, i64) -> bool) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        self.replace_pair(at, Value::Bool(op(a, b)));
        Ok(())
    }

    /// Compare references under the function identity rule.
    /// Structural equality of two enum values (specification 6.4).
    ///
    /// Two values are equal when they hold the same case and every
    /// field pair is equal. A field takes the rule of its own form: a
    /// scalar, text, or bytes field compares by value, a nested enum
    /// or tuple field compares structurally, and every other object
    /// compares by reference.
    ///
    /// The walk keeps an explicit work stack. An enum value can nest
    /// as deeply as its construction, and a deep value must not grow
    /// the host stack.
    ///
    /// The body stays out of the dispatch loop. Every instruction
    /// pays for the size of that loop, and this comparison runs on
    /// one instruction alone.
    #[inline(never)]
    fn values_equal(&self, module: &Module, a: Value, b: Value) -> Result<bool, FaultCode> {
        let mut work: Vec<(Value, Value)> = vec![(a, b)];
        while let Some((left, right)) = work.pop() {
            let equal = match (left, right) {
                (Value::Unit, Value::Unit) => true,
                (Value::Bool(x), Value::Bool(y)) => x == y,
                (Value::Int(x), Value::Int(y)) => x == y,
                (Value::Char(x), Value::Char(y)) => x == y,
                (Value::Op(x), Value::Op(y)) => x == y,
                (Value::EmptyCase { ty: xt, arm: xa }, Value::EmptyCase { ty: yt, arm: ya }) => {
                    xt == yt && xa == ya
                }
                (Value::Obj(x), Value::Obj(y)) => {
                    if x == y {
                        continue;
                    }
                    match (self.vm.heap.get(x), self.vm.heap.get(y)) {
                        (Object::Str(s), Object::Str(t))
                        | (Object::Str(s), Object::Substring(t))
                        | (Object::Substring(s), Object::Str(t))
                        | (Object::Substring(s), Object::Substring(t)) => s == t,
                        (Object::Bytes(s), Object::Bytes(t)) => s == t,
                        (Object::NativeDigest(s), Object::NativeDigest(t)) => s == t,
                        (
                            Object::Instance {
                                class: ac,
                                fields: af,
                                ..
                            },
                            Object::Instance {
                                class: bc,
                                fields: bf,
                                ..
                            },
                        ) => {
                            // An ordinary class keeps reference
                            // identity, so only an enum case walks.
                            let is_case = module
                                .classes
                                .get(*ac as usize)
                                .map(|c| c.kind == lm_bytecode::BcClassKind::Case)
                                .unwrap_or(false);
                            if !is_case || ac != bc || af.len() != bf.len() {
                                false
                            } else {
                                for (x, y) in af.iter().zip(bf.iter()) {
                                    if matches!(x, Value::Uninit) || matches!(y, Value::Uninit) {
                                        return Err(FaultCode::UninitializedField);
                                    }
                                    work.push((*x, *y));
                                }
                                continue;
                            }
                        }
                        (Object::Tuple { items: ai }, Object::Tuple { items: bi }) => {
                            if ai.len() != bi.len() {
                                false
                            } else {
                                for (x, y) in ai.iter().zip(bi.iter()) {
                                    work.push((*x, *y));
                                }
                                continue;
                            }
                        }
                        _ => self.references_equal(module, x, y),
                    }
                }
                _ => false,
            };
            if !equal {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn references_equal(&self, module: &Module, a: ObjRef, b: ObjRef) -> bool {
        if a == b {
            return true;
        }
        let (
            Object::Closure {
                func: a_func,
                captures: a_captures,
                env: a_env,
            },
            Object::Closure {
                func: b_func,
                captures: b_captures,
                env: b_env,
            },
        ) = (self.vm.heap.get(a), self.vm.heap.get(b))
        else {
            return false;
        };
        a_func == b_func
            && a_captures.is_empty()
            && b_captures.is_empty()
            && a_env.env() == TypeEnvId::EMPTY
            && b_env.env() == TypeEnvId::EMPTY
            && module
                .bindings
                .iter()
                .any(|binding| binding.func == *a_func)
    }

    fn str_compare(&mut self, want_equal: bool) -> Result<(), FaultCode> {
        let b = self.pop_obj()?;
        let a = self.pop_obj()?;
        let equal = self.text_value(a)? == self.text_value(b)?;
        self.push(Value::Bool(equal == want_equal))
    }
}

fn integer_text_len(value: i64) -> usize {
    let mut magnitude = value.unsigned_abs();
    let mut len = usize::from(value < 0) + 1;
    while magnitude >= 10 {
        magnitude /= 10;
        len += 1;
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_set_overlap_has_stable_precedence() {
        let mut table = PolicyTable::default();
        let client = lm_abi::group_by_name("Tcp.Client").unwrap() as usize;
        let stream = lm_abi::group_by_name("Tcp.Stream").unwrap() as usize;
        table.group[client] = Some(Action::Pass);
        assert!(matches!(
            table.lookup(lm_abi::OP_TCP_READ),
            Some(Action::Pass)
        ));
        assert!(table.lookup(lm_abi::OP_TCP_LISTEN).is_none());

        table.group[stream] = Some(Action::Block);
        assert!(matches!(
            table.lookup(lm_abi::OP_TCP_READ),
            Some(Action::Block)
        ));
        assert!(matches!(
            table.lookup(lm_abi::OP_TCP_CONNECT),
            Some(Action::Pass)
        ));

        table.exact[lm_abi::OP_TCP_READ as usize] = Some(Action::Pass);
        assert!(matches!(
            table.lookup(lm_abi::OP_TCP_READ),
            Some(Action::Pass)
        ));
    }

    /// The memory cost of one type environment witness.
    ///
    /// A frame stores one index, and the closure and the instance
    /// payloads store one each. `Object` is a Rust enum, so its size
    /// is the size of its largest variant, and the witness fits the
    /// existing padding of both payload variants.
    #[test]
    fn the_witness_costs_one_index_and_no_object_growth() {
        assert_eq!(std::mem::size_of::<Witness>(), 4);
        assert_eq!(std::mem::size_of::<Frame>(), 36);
        // The compact map index fixes the largest payload size. The
        // two witness fields fit without increasing that size.
        assert_eq!(std::mem::size_of::<Object>(), 56);
    }

    /// A fallible operand reader costs no register.
    ///
    /// Every typed reader of the interpreter answers
    /// `Result<_, FaultCode>` instead of asserting the tag. The value
    /// tag holds a niche, so the fault code fits inside the value and
    /// the return keeps the size it had.
    #[test]
    fn a_fallible_read_keeps_the_value_size() {
        assert_eq!(std::mem::size_of::<FaultCode>(), 1);
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::size_of::<Result<Value, FaultCode>>(), 16);
        assert_eq!(std::mem::size_of::<Result<ObjRef, FaultCode>>(), 12);
        assert_eq!(std::mem::size_of::<Result<bool, FaultCode>>(), 2);
        // An integer read pays one word, because `i64` has no niche.
        assert_eq!(std::mem::size_of::<Result<i64, FaultCode>>(), 16);
    }

    #[test]
    fn an_integer_pair_replaces_two_operands_in_place() {
        let mut machine = Machine::empty(VmConfig::default(), None);
        assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_STATE));

        machine.vm.operands = vec![Value::Int(7)];
        assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_STATE));
        assert!(machine.vm.operands.is_empty());

        machine.vm.operands = vec![Value::Bool(false)];
        assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_TYPE));
        assert!(machine.vm.operands.is_empty());

        machine.vm.operands = vec![Value::Int(7), Value::Int(5)];
        machine
            .int_binary(i64::checked_add)
            .expect("the addition succeeds");
        assert_eq!(machine.vm.operands, vec![Value::Int(12)]);

        machine.vm.operands = vec![Value::Bool(false), Value::Int(5)];
        assert_eq!(
            machine.int_binary(i64::checked_add),
            Err(FaultCode::TypeMismatch)
        );
        assert!(machine.vm.operands.is_empty());
    }

    #[test]
    fn integer_text_lengths_cover_signed_bounds() {
        assert_eq!(integer_text_len(0), 1);
        assert_eq!(integer_text_len(9), 1);
        assert_eq!(integer_text_len(10), 2);
        assert_eq!(integer_text_len(-10), 3);
        assert_eq!(integer_text_len(i64::MIN), i64::MIN.to_string().len());
        assert_eq!(integer_text_len(i64::MAX), i64::MAX.to_string().len());
    }

    #[test]
    fn request_ordinal_exhaustion_does_not_wrap() {
        let mut machine = Machine::empty(VmConfig::default(), None);
        machine.vm.next_ordinal = u64::MAX;
        assert_eq!(
            machine.take_request_ordinal(),
            Err(FaultCode::IntegerOverflow)
        );
        assert_eq!(machine.vm.next_ordinal, u64::MAX);
    }

    #[test]
    fn mailbox_metrics_saturate() {
        let mut mailbox = Mailbox::new(1);
        mailbox.accepted = u64::MAX;
        mailbox.delivered = u64::MAX;
        mailbox.push(Value::Int(1));
        assert_eq!(mailbox.accepted, u64::MAX);
        assert_eq!(mailbox.pop(), Some(Value::Int(1)));
        assert_eq!(mailbox.delivered, u64::MAX);
    }

    #[test]
    fn a_terminal_proc_keeps_only_its_dense_result_heap() {
        let mut machine = Machine::empty(VmConfig::default(), None);
        machine.is_proc = true;
        machine.vm.locals = Vec::with_capacity(1024);
        machine.vm.operands = Vec::with_capacity(1024);
        for _ in 0..1500 {
            machine
                .alloc(Object::Str("dead".into()))
                .expect("the dead object fits");
        }
        let result = machine
            .alloc(Object::Str("live".into()))
            .expect("the result fits");
        machine.set_done(result);
        let Some(Terminal::Done(Value::Obj(reference))) = machine.vm.terminal else {
            panic!("the proc stores its result");
        };
        assert_eq!(reference.slot, 0);
        assert_eq!(machine.vm.heap.slot_count(), 1);
        assert_eq!(machine.vm.locals.capacity(), 0);
        assert_eq!(machine.vm.operands.capacity(), 0);
        assert_eq!(machine.vm.heap.get(reference), &Object::Str("live".into()));
    }
}
