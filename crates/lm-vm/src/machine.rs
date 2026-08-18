//! One machine record: heap, frames, arenas, fuel, policy table,
//! pending perform, and terminal storage.
//!
//! `exec_instr` retires exactly one instruction of this machine. It
//! never runs another machine and never recurses: every operation
//! that reaches outside the machine returns an `ExecOutcome` for the
//! world driver.

use crate::resource::{ResourceBudget, ResourceRegistry};
use crate::{FaultCode, VmConfig};
use lm_bytecode::closed::{TypeEnvFull, TypeEnvs};
use lm_bytecode::{Instr, Module};
use lm_heap::{Heap, HeapBudget, MapIndex, Object};
use lm_value::{ObjRef, TypeEnvId, Value, Witness};
use std::hash::{Hash, Hasher};

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
    /// The action for one exact operation: exact entry, then group
    /// entry. `None` is the default block.
    pub fn lookup(&self, op: u32) -> Option<Action> {
        self.exact[op as usize].or(self.group[lm_abi::op_group(op) as usize])
    }
}

/// One explicit VM frame.
pub struct Frame {
    pub func: u32,
    pub block: u32,
    pub ip: u32,
    pub base_local: u32,
    pub base_operand: u32,
    /// The active closure object for `LoadCapture`.
    pub closure: Option<ObjRef>,
    /// The type environment of this activation.
    ///
    /// The call site supplies it. A monomorphic call copies the empty
    /// environment, so a monomorphic frame does no type work.
    pub env: TypeEnvId,
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
    /// A policy-table edit through a table handle.
    TableEdit {
        table: ObjRef,
        action: u32,
        kind: u32,
        slot: u32,
        mock: Option<Value>,
    },
    /// The operation identity test of a `Call` pattern.
    AsCall { request: ObjRef, op: u32 },
    /// `call.args()`.
    CallArgs { call: ObjRef },
    /// `value.digest()`. The world resolves code and class identity,
    /// so the digest never names a numeric slot.
    Digest { value: ObjRef },
}

/// The serializable state of one machine.
///
/// Every field here is machine state in the sense of specification
/// 16.4: a snapshot codec can copy the bytes. Specification 17.2
/// lists the same contents, and it excludes policy tables, live host
/// callbacks, and live host handles. Those live beside this record in
/// `Machine` and never enter it.
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

/// One machine: its serializable state plus the four kinds of state a
/// snapshot never copies.
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
    pub body_func: Option<u32>,
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
}

impl Machine {
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
            gate: 0,
            start_body: None,
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
            closure,
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
            if let Some(r) = frame.closure {
                roots.push(r);
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
            if let Some(r) = frame.closure {
                roots.push(r);
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

    /// Allocate one object. When the cap would be exceeded, collect
    /// first. The children of the new object are roots during the
    /// collection because they are not yet reachable from the arenas.
    pub fn alloc(&mut self, object: Object) -> Result<Value, FaultCode> {
        let cost = object.cost();
        if self.vm.heap.would_exceed(cost) {
            let mut extra = Vec::new();
            object.children(&mut extra);
            self.collect_garbage(&extra);
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
                    (Object::Str(s1), Object::Str(s2)) => s1 == s2,
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// The lookup hash of one map key. Strings hash by content, so
    /// the hash agrees with `key_eq`. The hash feeds the per-map
    /// index only and never leaves the process.
    fn key_hash(&self, key: Value) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match key {
            Value::Bool(b) => {
                0u8.hash(&mut h);
                b.hash(&mut h);
            }
            Value::Int(v) => {
                1u8.hash(&mut h);
                v.hash(&mut h);
            }
            Value::Obj(r) => match self.vm.heap.get(r) {
                Object::Str(s) => {
                    2u8.hash(&mut h);
                    s.hash(&mut h);
                }
                _ => 3u8.hash(&mut h),
            },
            _ => 4u8.hash(&mut h),
        }
        h.finish()
    }

    /// Find the entry position of a key in the map object `r` through
    /// the hash index. The index is a cache: the call first indexes
    /// the entries appended since the last lookup.
    fn map_lookup(&mut self, r: ObjRef, key: Value) -> Result<Option<usize>, FaultCode> {
        let (built, len) = match self.vm.heap.get(r) {
            Object::Map { entries, index } => (index.built, entries.len()),
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
            Object::Map { entries, index } => (entries, index.candidates(hash)),
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

    /// Execute exactly one instruction of the current frame.
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
    ) -> Result<ExecOutcome, FaultCode> {
        if self.vm.fuel == 0 {
            return Err(FaultCode::OutOfFuel);
        }
        self.vm.fuel -= 1;
        // The fetch indexes the code tables without a bounds test.
        // Four rules together prove that every reachable position
        // resolves:
        //
        // 1. `LoadedModule` construction is the only path to
        //    execution, and it admits a module only when the import
        //    table is empty. An imported function is the one function
        //    that carries no body, so an executable module has none.
        // 2. `verify_module` therefore skips no function, and
        //    `verify_func` rejects a function with no blocks.
        // 3. `verify_func` ends every block with a terminator,
        //    forbids a terminator before the end, and bounds every
        //    branch target. So a live frame never steps past its
        //    block, and `Call`, `New`, and `ConstStr` name a real row.
        // 4. Snapshot admission checks the function, the block, and
        //    the counter of each restored frame.
        //
        // Change any one of those rules and restore the bounds test.
        let frame = self.vm.frames.last().ok_or(BAD_STATE)?;
        let instr =
            module.funcs[frame.func as usize].blocks[frame.block as usize][frame.ip as usize];
        self.vm.frames.last_mut().ok_or(BAD_STATE)?.ip += 1;
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
                        let value = self.alloc(Object::Str(text))?;
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
                let Some(value) = a.checked_div(b) else {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                };
                self.replace_pair(at, Value::Int(value));
            }
            Instr::Rem => {
                let (at, a, b) = self.int_pair()?;
                if b == 0 {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::DivideByZero);
                }
                let Some(value) = a.checked_rem(b) else {
                    self.vm.operands.truncate(at);
                    return Err(FaultCode::IntegerOverflow);
                };
                self.replace_pair(at, Value::Int(value));
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
            Instr::EqStr => self.str_compare(true)?,
            Instr::NeStr => self.str_compare(false)?,
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
                let class = match self.vm.heap.get(recv.as_obj().ok_or(BAD_TYPE)?) {
                    Object::Instance { class, .. } => *class,
                    _ => return Err(BAD_TYPE),
                };
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
                let r = callee.as_obj().ok_or(BAD_TYPE)?;
                let (target, env) = match self.vm.heap.get(r) {
                    Object::Closure { func, env, .. } => (*func, env.env()),
                    _ => return Err(BAD_TYPE),
                };
                self.push_frame(module, target, argc, Some(r), env)?;
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
                let value = match self.vm.heap.get(closure) {
                    Object::Closure { captures, .. } => {
                        *captures.get(idx as usize).ok_or(BAD_TYPE)?
                    }
                    _ => return Err(BAD_TYPE),
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
                let r = self.pop_obj()?;
                let matches = self.instance_matches(module, r, ty)?;
                self.push(Value::Bool(matches))?;
            }
            Instr::CastType(ty) => {
                let r = self.pop_obj()?;
                if !self.instance_matches(module, r, ty)? {
                    return Err(FaultCode::BadCast);
                }
                self.push(Value::Obj(r))?;
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
                let value = self.alloc(Object::List { items })?;
                self.push(value)?;
            }
            Instr::ListLen => {
                let r = self.pop_obj()?;
                let len = match self.vm.heap.get(r) {
                    Object::List { items } => items.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::ListAt => {
                let idx = self.pop_int()?;
                let r = self.pop_obj()?;
                let value = match self.vm.heap.get(r) {
                    Object::List { items } => {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(FaultCode::IndexOutOfBounds);
                        }
                        items[idx as usize]
                    }
                    _ => return Err(BAD_TYPE),
                };
                self.push(value)?;
            }
            Instr::ListPush => {
                let value = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                self.reserve(16, &[Value::Obj(r), value])?;
                match self.vm.heap.get_mut(r) {
                    Object::List { items } => items.push(value),
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
            Instr::MapPut => {
                let value = self.pop()?;
                let key = self.pop()?;
                let r = self.pop_obj()?;
                self.frozen_guard(r)?;
                let pos = self.map_lookup(r, key)?;
                match pos {
                    Some(pos) => match self.vm.heap.get_mut(r) {
                        Object::Map { entries, .. } => {
                            entries.get_mut(pos).ok_or(BAD_STATE)?.1 = value;
                        }
                        _ => return Err(BAD_TYPE),
                    },
                    None => {
                        // The appended entry joins the index on the
                        // next lookup.
                        self.reserve(32, &[Value::Obj(r), key, value])?;
                        match self.vm.heap.get_mut(r) {
                            Object::Map { entries, .. } => entries.push((key, value)),
                            _ => return Err(BAD_TYPE),
                        }
                        self.vm.heap.recharge(r);
                    }
                }
                self.push(Value::Unit)?;
            }
            Instr::SbNew => {
                let value = self.alloc(Object::StrBuilder(String::new()))?;
                self.push(value)?;
            }
            Instr::SbAppendStr => {
                let s = self.pop_obj()?;
                let sb = self.pop_obj()?;
                self.frozen_guard(sb)?;
                let len = match self.vm.heap.get(s) {
                    Object::Str(text) => text.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.reserve(len, &[Value::Obj(sb), Value::Obj(s)])?;
                if !self.vm.heap.append_string(sb, s) {
                    return Err(BAD_TYPE);
                }
                self.vm.heap.recharge(sb);
                self.push(Value::Obj(sb))?;
            }
            Instr::SbAppendInt => {
                let v = self.pop_int()?;
                let sb = self.pop_obj()?;
                self.frozen_guard(sb)?;
                self.sb_append_int(sb, v)?;
            }
            Instr::SbAppendBool => {
                let v = self.pop_bool()?;
                let sb = self.pop_obj()?;
                self.frozen_guard(sb)?;
                let text = if v { "true" } else { "false" };
                self.sb_append(sb, text)?;
            }
            Instr::SbBuild => {
                let sb = self.pop_obj()?;
                let text = match self.vm.heap.get(sb) {
                    Object::StrBuilder(text) => text.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::BbNew => {
                let value = self.alloc(Object::ByteBuf(Vec::new()))?;
                self.push(value)?;
            }
            Instr::BbAppend => {
                let v = self.pop_int()?;
                let bb = self.pop_obj()?;
                self.frozen_guard(bb)?;
                let byte = u8::try_from(v).map_err(|_| FaultCode::IntegerOverflow)?;
                self.reserve(1, &[Value::Obj(bb)])?;
                match self.vm.heap.get_mut(bb) {
                    Object::ByteBuf(bytes) => bytes.push(byte),
                    _ => return Err(BAD_TYPE),
                }
                self.vm.heap.recharge(bb);
                self.push(Value::Obj(bb))?;
            }
            Instr::BbLen => {
                let bb = self.pop_obj()?;
                let len = match self.vm.heap.get(bb) {
                    Object::ByteBuf(bytes) => bytes.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::BbBuild => {
                let bb = self.pop_obj()?;
                let bytes = match self.vm.heap.get(bb) {
                    Object::ByteBuf(bytes) => bytes.clone(),
                    _ => return Err(BAD_TYPE),
                };
                let text = String::from_utf8(bytes).map_err(|_| FaultCode::BadCast)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::BytesNew => {
                let string = self.pop_obj()?;
                let bytes = match self.vm.heap.get(string) {
                    Object::Str(text) => text.as_bytes().to_vec(),
                    _ => return Err(BAD_TYPE),
                };
                let value = self.alloc(Object::Bytes(bytes))?;
                self.push(value)?;
            }
            Instr::BytesLen => {
                let bytes = self.pop_obj()?;
                let len = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => bytes.len(),
                    _ => return Err(BAD_TYPE),
                };
                self.push(Value::Int(len as i64))?;
            }
            Instr::BytesText => {
                let bytes = self.pop_obj()?;
                let text = match self.vm.heap.get(bytes) {
                    Object::Bytes(bytes) => String::from_utf8(bytes.clone()),
                    _ => return Err(BAD_TYPE),
                }
                .map_err(|_| FaultCode::BadCast)?;
                let value = self.alloc(Object::Str(text))?;
                self.push(value)?;
            }
            Instr::Freeze => {
                let r = self.pop_obj()?;
                // The freeze mode validates the whole reachable graph
                // against its limits before any bit goes on, so a
                // rejected freeze changes nothing.
                lm_graph::freeze(&mut self.vm.heap, r, &self.config.graph)?;
                self.push(Value::Obj(r))?;
            }
            Instr::Digest => {
                let r = self.pop_obj()?;
                return Ok(ExecOutcome::Digest { value: r });
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
            Instr::Return => {
                let value = self.pop()?;
                let frame = self.vm.frames.pop().ok_or(BAD_STATE)?;
                self.vm.operands.truncate(frame.base_operand as usize);
                self.vm.locals.truncate(frame.base_local as usize);
                if self.vm.frames.is_empty() {
                    return Ok(ExecOutcome::Terminal(value));
                }
                self.push(value)?;
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
            Instr::AsCall(op) => {
                let request = self.pop_obj()?;
                return Ok(ExecOutcome::AsCall { request, op });
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
                let value = self.alloc(Object::Str(code.to_string()))?;
                self.push(value)?;
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
                    message: reason,
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
        limit: u32,
    ) -> (Result<Option<ExecOutcome>, FaultCode>, u32) {
        debug_assert!(limit > 0);
        let original_fuel = self.vm.fuel;
        let batch_fuel = original_fuel.min(u64::from(limit));
        let held_fuel = original_fuel - batch_fuel;
        let count_expiry = u64::from(limit) <= original_fuel;
        self.vm.fuel = batch_fuel;

        let outcome = loop {
            match self.exec_instr(module, dispatch, envs) {
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

    /// Return true when the instance class equals or extends the
    /// class named by the target type index.
    fn instance_matches(&self, module: &Module, r: ObjRef, ty: u32) -> Result<bool, FaultCode> {
        let target = match module.types.get(ty as usize).ok_or(BAD_STATE)? {
            lm_bytecode::BcType::Class(c) | lm_bytecode::BcType::Inst(c, _) => *c,
            _ => return Err(BAD_STATE),
        };
        let mut class = match self.vm.heap.get(r) {
            Object::Instance { class, .. } => *class,
            _ => return Err(BAD_TYPE),
        };
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
        self.reserve(text.len(), &[Value::Obj(sb)])?;
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => buf.push_str(text),
            _ => return Err(BAD_TYPE),
        }
        self.vm.heap.recharge(sb);
        self.push(Value::Obj(sb))
    }

    /// Append one integer without a temporary string allocation.
    fn sb_append_int(&mut self, sb: ObjRef, value: i64) -> Result<(), FaultCode> {
        self.reserve(integer_text_len(value), &[Value::Obj(sb)])?;
        match self.vm.heap.get_mut(sb) {
            Object::StrBuilder(buf) => {
                std::fmt::Write::write_fmt(buf, format_args!("{value}")).map_err(|_| BAD_STATE)?;
            }
            _ => return Err(BAD_TYPE),
        }
        self.vm.heap.recharge(sb);
        self.push(Value::Obj(sb))
    }

    /// Push a frame. The top `consume` operand values become the first
    /// local slots in order. `closure` supplies capture context for a
    /// closure call.
    fn push_frame(
        &mut self,
        module: &Module,
        callee: u32,
        consume: usize,
        closure: Option<ObjRef>,
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
    fn pop_obj(&mut self) -> Result<ObjRef, FaultCode> {
        match self.pop()? {
            Value::Obj(r) => Ok(r),
            _ => Err(BAD_TYPE),
        }
    }

    /// Read two integer operands and preserve successful input.
    ///
    /// An error consumes the same operands as two ordered `pop_int`
    /// calls. A successful caller replaces both operands in place.
    #[inline]
    fn int_pair(&mut self) -> Result<(usize, i64, i64), FaultCode> {
        let len = self.vm.operands.len();
        if len == 0 {
            return Err(BAD_STATE);
        }
        let b = match self.vm.operands[len - 1] {
            Value::Int(value) => value,
            _ => {
                self.vm.operands.truncate(len - 1);
                return Err(BAD_TYPE);
            }
        };
        if len == 1 {
            self.vm.operands.clear();
            return Err(BAD_STATE);
        }
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

    #[inline]
    fn replace_pair(&mut self, at: usize, value: Value) {
        self.vm.operands[at] = value;
        self.vm.operands.truncate(at + 1);
    }

    fn int_binary(&mut self, op: impl Fn(i64, i64) -> Option<i64>) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        let Some(value) = op(a, b) else {
            self.vm.operands.truncate(at);
            return Err(FaultCode::IntegerOverflow);
        };
        self.replace_pair(at, Value::Int(value));
        Ok(())
    }

    fn int_compare(&mut self, op: impl Fn(i64, i64) -> bool) -> Result<(), FaultCode> {
        let (at, a, b) = self.int_pair()?;
        self.replace_pair(at, Value::Bool(op(a, b)));
        Ok(())
    }

    /// Compare references under the function identity rule.
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
        let equal = match (self.vm.heap.get(a), self.vm.heap.get(b)) {
            (Object::Str(s1), Object::Str(s2)) => s1 == s2,
            _ => return Err(BAD_TYPE),
        };
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
                .alloc(Object::Str("dead".to_string()))
                .expect("the dead object fits");
        }
        let result = machine
            .alloc(Object::Str("live".to_string()))
            .expect("the result fits");
        machine.set_done(result);
        let Some(Terminal::Done(Value::Obj(reference))) = machine.vm.terminal else {
            panic!("the proc stores its result");
        };
        assert_eq!(reference.slot, 0);
        assert_eq!(machine.vm.heap.slot_count(), 1);
        assert_eq!(machine.vm.locals.capacity(), 0);
        assert_eq!(machine.vm.operands.capacity(), 0);
        assert_eq!(
            machine.vm.heap.get(reference),
            &Object::Str("live".to_string())
        );
    }
}
