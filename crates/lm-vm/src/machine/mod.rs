//! One machine record: heap, frames, arenas, fuel, policy table,
//! pending perform, and terminal storage.
//!
//! `exec_instr` retires exactly one instruction of this machine. It
//! never runs another machine and never recurses: every operation
//! that reaches outside the machine returns an `ExecOutcome` for the
//! world driver.

use crate::resource::{ResourceBudget, ResourceRegistry};
use crate::{FaultCode, NamespaceRuntime, VmConfig};
use lm_bytecode::closed::{ClosedType, ClosedTypeId, TypeEnvFull, TypeEnvs};
use lm_bytecode::{ExtendedInstr, Instr, Module as CompiledModulePayload, NumericInstr};
use lm_heap::{
    process_lookup_hash, FaultSite, Heap, MapEntry, MapIndex, NativeByteBuffer,
    NativeStringBuilder, Object, SharedBytes, SharedText, StructuralEpoch, TextRef,
};
use lm_value::{canonical_float_bits, CallbackRef, ObjRef, TypeEnvId, Value, Witness};
use std::fmt::Write as _;
use std::sync::Arc;

/// The largest typed wait table of one machine.
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
fn method_of(
    dispatch: &lm_bytecode::CodeTable<crate::DispatchRow>,
    class: u32,
    selector: u32,
) -> Result<u32, FaultCode> {
    dispatch
        .get(class as usize)
        .and_then(|row| row.method(selector))
        .ok_or(BAD_TYPE)
}

/// One verified interface call site.
#[derive(Debug, Clone, Copy)]
struct InterfaceCallSite {
    interface: u32,
    method: u32,
    recv_ty: u32,
    app: u32,
}

/// Encode one map epoch and optional index slot as an opaque `Int`.
pub(crate) fn map_probe_token(epoch: u32, slot: Option<u32>) -> Result<i64, FaultCode> {
    let low = match slot {
        Some(slot) => slot.checked_add(1).ok_or(BAD_STATE)?,
        None => 0,
    };
    Ok(((u64::from(epoch) << 32) | u64::from(low)) as i64)
}

/// Decode one map probe token.
pub(crate) fn map_probe_parts(token: i64) -> Result<(u32, Option<u32>), FaultCode> {
    if token == 0 {
        return Err(BAD_STATE);
    }
    let bits = token as u64;
    let epoch = (bits >> 32) as u32;
    if epoch == 0 {
        return Err(BAD_STATE);
    }
    let low = bits as u32;
    Ok((epoch, (low != 0).then_some(low.wrapping_sub(1))))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitSource {
    /// Read one message from the owning proc mailbox.
    Receive,
    /// Drive one holder-owned child machine.
    Drive { target: VmId },
    /// Select between two existing wait trees.
    Choice { first: u64, second: u64 },
    /// Select one result from a homogeneous runtime-sized wait set.
    Any { roots: Arc<[u64]> },
    /// One exact operation that becomes visible only after selection.
    Operation {
        op: u32,
        ordinal: u64,
        scope: u64,
        consume_resource: Option<u64>,
        reply_ty: u32,
        env: TypeEnvId,
        ready: Option<Value>,
    },
}

/// One wait entry in its owner machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEntry {
    pub source: WaitSource,
    /// A choice owns linked entries. Their old tokens are stale.
    pub linked: bool,
}

/// One operation that is producing a selectable wait source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitPreparation {
    pub op: u32,
    pub reply_ty: u32,
    pub env: TypeEnvId,
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
    pub trace: Vec<FaultSite>,
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

/// One native policy table with an implicit default of block.
///
/// Each vector ends at its highest edited slot. A default table owns
/// no allocation, so operation-manifest growth does not enlarge each machine.
#[derive(Debug)]
pub struct PolicyTable {
    exact: Vec<Option<Action>>,
    group: Vec<Option<Action>>,
    bundle: std::sync::Arc<lm_abi::AbiBundle>,
}

impl Default for PolicyTable {
    fn default() -> PolicyTable {
        PolicyTable {
            exact: Vec::new(),
            group: Vec::new(),
            bundle: lm_abi::standard_bundle(),
        }
    }
}

impl PolicyTable {
    pub(crate) fn set_bundle(&mut self, bundle: std::sync::Arc<lm_abi::AbiBundle>) {
        self.bundle = bundle;
    }

    pub(crate) fn bundle(&self) -> &std::sync::Arc<lm_abi::AbiBundle> {
        &self.bundle
    }

    fn action(entries: &[Option<Action>], slot: u32) -> Option<Action> {
        entries.get(slot as usize).copied().flatten()
    }

    fn set(
        entries: &mut Vec<Option<Action>>,
        limit: u32,
        slot: u32,
        action: Option<Action>,
    ) -> bool {
        if slot >= limit {
            return false;
        }
        if action.is_some() && entries.len() <= slot as usize {
            entries.resize(slot as usize + 1, None);
        }
        if let Some(cell) = entries.get_mut(slot as usize) {
            *cell = action;
            while entries.last().is_some_and(Option::is_none) {
                entries.pop();
            }
        }
        true
    }

    pub(crate) fn set_exact(&mut self, slot: u32, action: Option<Action>) -> bool {
        Self::set(&mut self.exact, self.bundle.op_count(), slot, action)
    }

    pub(crate) fn set_group(&mut self, slot: u32, action: Option<Action>) -> bool {
        Self::set(&mut self.group, self.bundle.group_count(), slot, action)
    }

    pub(crate) fn group_action(&self, slot: u32) -> Option<Action> {
        Self::action(&self.group, slot)
    }

    pub(crate) fn clear(&mut self) {
        self.exact.clear();
        self.group.clear();
    }

    pub(crate) fn entry_count(&self) -> usize {
        self.exact.iter().flatten().count() + self.group.iter().flatten().count()
    }

    fn actions(&self) -> impl Iterator<Item = Action> + '_ {
        self.exact
            .iter()
            .chain(self.group.iter())
            .filter_map(|action| *action)
    }

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
        if let Some(action) = Self::action(&self.exact, op) {
            return Some(action);
        }
        let mut passed = false;
        let groups = self.bundle.groups_containing_op(op).unwrap_or_default();
        for group in groups {
            match Self::action(&self.group, *group) {
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
    /// Reflection refinement can extend this environment inside one
    /// case arm.
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
    MapNextIndex,
    MapEntry { value: bool },
}

#[derive(Clone, Copy)]
enum CollectionExtensionOp {
    ListCapacity,
    ListSet,
    ListSwap,
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
    /// The instruction reached a native entry point.
    #[doc(hidden)]
    ContinueNative,
    /// The last frame returned this terminal value.
    Terminal(Value),
    /// Guest code stopped itself with a message.
    Raise { code: FaultCode, message: String },
    /// Guest code re-raised one complete stored fault.
    Reraise(FaultRec),
    /// A perform: the arguments are recorded in `Pending` by the
    /// driver.
    Perform { op: u32, args: Vec<Value> },
    /// Prepare one exact operation as a selectable source.
    PrepareWait {
        op: u32,
        argc: u32,
        reply_ty: u32,
        env: TypeEnvId,
    },
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
    /// Render the dynamic result of another machine.
    DynamicRenderRef { vm: u32, generation: u32 },
    /// Reify one named function as portable verified code.
    FunctionCode {
        function: u32,
        origin: Option<[u8; 32]>,
    },
    /// Reify one named class as portable verified code.
    ClassCode {
        class: u32,
        origin: Option<[u8; 32]>,
    },
}

/// Why one execution batch stopped without a world action.
pub(crate) enum ExecError {
    Fault(FaultCode),
}

/// Interpreter policy for transitions into native code.
#[derive(Clone, Copy)]
pub(crate) enum NativeResume<'a> {
    Disabled,
    EveryDirectCall,
    ReturnToDepth {
        depth: usize,
    },
    Tiered {
        state: &'a crate::jit::NativeCodeState,
        resume_depth: Option<usize>,
        profile: bool,
    },
}

const TIER_SAMPLE_INTERVAL: u32 = 64;

struct InterpreterNative<'a> {
    policy: NativeResume<'a>,
    sample: u8,
    check_native_calls: bool,
}

impl<'a> InterpreterNative<'a> {
    fn new(policy: NativeResume<'a>, fuel: u64) -> InterpreterNative<'a> {
        let mixed_fuel = fuel ^ (fuel >> 7) ^ (fuel >> 17);
        let check_native_calls = match policy {
            NativeResume::Tiered { state, .. } => state.has_compiled_code(),
            _ => false,
        };
        InterpreterNative {
            policy,
            sample: mixed_fuel as u8 & (TIER_SAMPLE_INTERVAL as u8 - 1),
            check_native_calls,
        }
    }

    #[inline(always)]
    fn after_call(&mut self, target: u32) -> bool {
        match self.policy {
            NativeResume::Disabled => false,
            NativeResume::EveryDirectCall => true,
            NativeResume::ReturnToDepth { .. } => false,
            NativeResume::Tiered { state, profile, .. } => {
                let sampled = sample_tier_event(&mut self.sample);
                if self.check_native_calls {
                    state
                        .enter_frame(
                            target,
                            if sampled { TIER_SAMPLE_INTERVAL } else { 0 },
                            profile,
                        )
                        .enter_native
                } else {
                    sampled && state.note_event(target, TIER_SAMPLE_INTERVAL, profile)
                }
            }
        }
    }

    #[inline(always)]
    fn after_return(&self, depth: usize) -> bool {
        match self.policy {
            NativeResume::Disabled => false,
            NativeResume::EveryDirectCall => true,
            NativeResume::ReturnToDepth { depth: target } => target == depth,
            NativeResume::Tiered { resume_depth, .. } => resume_depth == Some(depth),
        }
    }

    #[inline(always)]
    fn after_backedge(&mut self, function: u32) -> bool {
        match self.policy {
            NativeResume::Tiered { state, profile, .. } if sample_tier_event(&mut self.sample) => {
                state.note_event(function, TIER_SAMPLE_INTERVAL, profile)
            }
            _ => false,
        }
    }
}

#[inline(always)]
fn sample_tier_event(counter: &mut u8) -> bool {
    *counter = counter.wrapping_add(1) & (TIER_SAMPLE_INTERVAL as u8 - 1);
    *counter == 0
}

/// The serializable state of one machine.
///
/// These fields contain the compact machine state from specification 16.4.
/// `Machine` stores the cold callback arena separately.
///
/// The interpreter runs as a method of `Machine` because allocation
/// also needs the policy roots.
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
    pub literals: Vec<Value>,
}

/// One machine with compact state, callback state, and host state.
pub struct Machine {
    /// The code namespace that defines this machine.
    pub namespace: lm_link::NamespaceId,
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
    /// The function result type is the machine result type.
    /// A class proc body takes its proc instance first.
    /// A closure proc body is nullary.
    /// This record remains after the machine drops its frames.
    pub body_func: Option<FunctionVersionId>,
    /// The type environment of the machine body activation.
    ///
    /// The machine witness. The two types above close through it, so a
    /// machine past its constructor still names both.
    pub witness: TypeEnvId,
    /// True when a proc launch operation launched this machine.
    ///
    /// The flag names machines that received a proc birth grant.
    /// Restore rebuilds the proc control grant.
    /// Ownership does not determine this flag.
    /// `Proc.Run` transfers a plain machine and mints no grant.
    pub is_proc: bool,
    /// True when the holder restored this run without its result type.
    ///
    /// The terminal value of such a run crosses the boundary as a
    /// `DynValue`. The flag rides in a snapshot, so a branch or a
    /// restore of the run keeps the same delivery.
    pub dynamic_result: bool,
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
    /// The original operation whose reply becomes a wait source.
    pub preparing_wait: Option<WaitPreparation>,
    /// Clock-free execution counters.
    execution_metrics: MachineExecutionMetrics,
    /// Native frames retained across one ordinary scheduler quantum.
    ///
    /// Snapshots exclude this process-local execution state.
    native_continuation: Option<Box<crate::jit::NativeContinuation>>,
    /// Parent depth for one bounded interpreter bridge.
    ///
    /// Snapshots exclude this process-local execution hint.
    native_return_depth: Option<usize>,
    /// Derived type environments used only by native execution.
    ///
    /// Snapshots exclude this process-local cache.
    pub(crate) native_type_environments: lm_jit::NativeTypeEnvironmentCache,
    /// Resolved interface targets used only by native execution.
    ///
    /// Snapshots exclude this process-local cache.
    pub(crate) native_resolved_calls: lm_jit::NativeResolvedCallCache,
    /// Compiled result retained between the paired regex instructions.
    ///
    /// Snapshots exclude this process-local cache.
    pub(crate) pending_regex_compile: Option<ObjRef>,
}

/// Clock-free execution counters for one machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineExecutionMetrics {
    pub native_calls: u64,
    pub collections: u64,
}

struct PortableDefinitionInfo {
    module_name: String,
    qualified_key: String,
    hashes: lm_bytecode::identity::DefinitionHashes,
    related_functions: Vec<u32>,
}

fn portable_definition_index(
    code: &lm_heap::PortableCode,
    module: &CompiledModulePayload,
) -> Result<u32, FaultCode> {
    let mut matches = module.exports.iter().filter_map(|export| match code.kind {
        lm_heap::PortableCodeKind::Function if export.kind == lm_bytecode::ExportKind::Function => {
            Some(export.def)
        }
        lm_heap::PortableCodeKind::Class if export.kind.is_class() => Some(export.def),
        _ => None,
    });
    let selected = matches.next().ok_or(BAD_STATE)?;
    if matches.next().is_some() {
        return Err(BAD_STATE);
    }
    Ok(selected)
}

fn portable_definition_info_payload(
    code: &lm_heap::PortableCode,
    module: &CompiledModulePayload,
    identity: &lm_bytecode::identity::ModuleIdentity,
) -> Result<PortableDefinitionInfo, FaultCode> {
    let index = portable_definition_index(code, module)?;
    match code.kind {
        lm_heap::PortableCodeKind::Function => {
            let binding = module
                .bindings
                .iter()
                .filter(|binding| binding.func == index && binding.class == lm_bytecode::NO_CLASS)
                .min_by(|left, right| left.key.cmp(&right.key))
                .ok_or(BAD_STATE)?;
            let hashes = lm_bytecode::identity::function_definition_hashes(module, identity, index)
                .map_err(|_| BAD_STATE)?;
            Ok(PortableDefinitionInfo {
                module_name: binding
                    .key
                    .rsplit_once('.')
                    .map_or("", |(prefix, _)| prefix)
                    .to_string(),
                qualified_key: binding.key.clone(),
                hashes,
                related_functions: vec![index],
            })
        }
        lm_heap::PortableCodeKind::Class => {
            let class = module.classes.get(index as usize).ok_or(BAD_STATE)?;
            let suffix = format!(".{}", class.name);
            let module_name = if class.key == class.name {
                String::new()
            } else {
                class
                    .key
                    .strip_suffix(&suffix)
                    .ok_or(BAD_STATE)?
                    .to_string()
            };
            let hashes = lm_bytecode::identity::class_definition_hashes(module, identity, index)
                .map_err(|_| BAD_STATE)?;
            Ok(PortableDefinitionInfo {
                module_name,
                qualified_key: class.key.clone(),
                hashes,
                related_functions: class
                    .methods
                    .iter()
                    .map(|(_, function)| *function)
                    .collect(),
            })
        }
        _ => Err(BAD_TYPE),
    }
}

mod calls;
mod collections;
mod execution;
mod heap;
pub(crate) use heap::BorrowedStringKey;
mod interpreter;
mod reflection;
mod regex;
pub(crate) use regex::{build_regex_match, regex_group_text};
mod stack;
mod state;
mod syntax;
mod values;

pub(crate) fn integer_text_len(value: i64) -> usize {
    let mut magnitude = value.unsigned_abs();
    let mut len = usize::from(value < 0) + 1;
    while magnitude >= 10 {
        magnitude /= 10;
        len += 1;
    }
    len
}

fn shift_amount(value: i64) -> Result<u32, FaultCode> {
    let amount = u32::try_from(value).map_err(|_| FaultCode::ShiftOutOfRange)?;
    if amount > 63 {
        return Err(FaultCode::ShiftOutOfRange);
    }
    Ok(amount)
}

fn rotation_amount(value: i64, mask: u32) -> u32 {
    (value as u64 & u64::from(mask)) as u32
}

fn float_eq(left: u64, right: u64) -> bool {
    let left = f64::from_bits(left);
    let right = f64::from_bits(right);
    left == right || (left.is_nan() && right.is_nan())
}

fn float_hash(bits: u64) -> i64 {
    let bits = canonical_float_bits(bits);
    if bits << 1 == 0 {
        0
    } else {
        bits as i64
    }
}

fn float_fits_int(value: f64) -> bool {
    value >= i64::MIN as f64 && value < 9_223_372_036_854_775_808.0
}

const FLOAT_TEXT_CAPACITY: usize = 400;

pub(crate) struct FloatText {
    bytes: [u8; FLOAT_TEXT_CAPACITY],
    len: usize,
}

impl FloatText {
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len])
            .expect("float formatting always produces valid UTF-8")
    }
}

impl std::fmt::Write for FloatText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len.checked_add(text.len()).ok_or(std::fmt::Error)?;
        let target = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
        target.copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}

pub(crate) fn float_text(value: f64) -> Result<FloatText, std::fmt::Error> {
    let mut text = FloatText {
        bytes: [0; FLOAT_TEXT_CAPACITY],
        len: 0,
    };
    write!(&mut text, "{value}")?;
    Ok(text)
}

/// Parse one Float text form. Status 1 means invalid text.
/// Status 2 means a finite decimal overflowed to infinity.
pub(crate) fn parse_float_text(text: &str) -> Result<f64, i64> {
    match text {
        "NaN" => return Ok(f64::NAN),
        "inf" | "+inf" => return Ok(f64::INFINITY),
        "-inf" => return Ok(f64::NEG_INFINITY),
        _ => {}
    }
    if !is_decimal_float_text(text) {
        return Err(1);
    }
    let value = text.parse::<f64>().map_err(|_| 1)?;
    if value.is_infinite() {
        Err(2)
    } else {
        Ok(value)
    }
}

/// Test the decimal grammar accepted by `Text.parse_float`.
fn is_decimal_float_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = usize::from(matches!(bytes.first(), Some(b'+') | Some(b'-')));
    let mut digits = 0usize;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
        digits += 1;
    }
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(bytes.get(at), Some(b'e') | Some(b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+') | Some(b'-')) {
            at += 1;
        }
        let exponent = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if at == exponent {
            return false;
        }
    }
    at == bytes.len()
}

/// One resolved code position, before any allocation.
#[derive(Debug, Clone)]
pub(crate) struct CodeOrigin {
    /// The source path and byte range, when debug data names them.
    pub(crate) source: Option<(String, (u32, u32))>,
    /// The structural hash of the function.
    pub(crate) digest: [u8; 32],
    /// The instruction offset inside the function.
    pub(crate) offset: i64,
}

/// Resolve one trace site against the code that executed it.
pub(crate) fn code_origin(
    module: &NamespaceRuntime,
    debug: &lm_bytecode::debug::DebugInfo,
    identity: &lm_bytecode::identity::ModuleIdentity,
    site: FaultSite,
) -> Result<CodeOrigin, FaultCode> {
    let mapping = debug
        .functions
        .iter()
        .rev()
        .find(|mapping| mapping.function == site.function);
    let source = mapping
        .map(|mapping| debug.sources.get(mapping.source as usize).ok_or(BAD_STATE))
        .transpose()?;
    let function = module.funcs.get(site.function as usize).ok_or(BAD_STATE)?;
    let block = function.blocks.get(site.block as usize).ok_or(BAD_STATE)?;
    if site.instruction as usize >= block.len() {
        return Err(BAD_STATE);
    }
    let mut offset = 0usize;
    for prior in function.blocks.iter().take(site.block as usize) {
        offset = offset.checked_add(prior.len()).ok_or(BAD_STATE)?;
    }
    offset = offset
        .checked_add(site.instruction as usize)
        .ok_or(BAD_STATE)?;
    let offset = i64::try_from(offset).map_err(|_| BAD_STATE)?;
    let digest = *identity
        .func_hashes
        .get(site.function as usize)
        .ok_or(BAD_STATE)?;
    let source = mapping
        .zip(source)
        .map(|(mapping, source)| (source.path.clone(), (mapping.lo, mapping.hi)));
    Ok(CodeOrigin {
        source,
        digest,
        offset,
    })
}

#[cfg(test)]
mod tests;
